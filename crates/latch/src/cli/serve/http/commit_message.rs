//! Explicit cloud drafting only: no mutation locks, receipts or stored prompts.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::sync::watch;

use super::{AppState, DeviceContext};
use crate::cli::commit_message::{self as config, ConfigError};
use crate::cli::serve::commit_message_provider;
use crate::cli::serve::contract::{
    GitCommitMessageFailure, GitCommitMessageRequest, GitCommitMessageResponse,
    GitDraftFailureCode as Failure, GitDraftReadiness, GitDraftReadinessState,
};
use crate::cli::serve::git::commit_message::collect;

#[derive(Clone, Default)]
pub(super) struct Drafts(Arc<Mutex<HashMap<String, Entry>>>);
struct Entry {
    since: Instant,
    count: u8,
    active: Option<(String, watch::Sender<bool>)>,
}
struct Lease {
    drafts: Drafts,
    device: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.drafts.0.lock() {
            if let Some(entry) = entries.get_mut(&self.device) {
                entry.active = None;
            }
        }
    }
}
impl Drafts {
    fn begin(&self, device: String, id: &str) -> Result<(Lease, watch::Receiver<bool>), Failure> {
        let mut entries = self.0.lock().map_err(|_| Failure::ProviderError)?;
        entries.retain(|_, entry| {
            entry.active.is_some() || entry.since.elapsed() < Duration::from_secs(60)
        });
        if entries.values().filter(|e| e.active.is_some()).count() >= 4
            || (!entries.contains_key(&device) && entries.len() >= 256)
        {
            return Err(Failure::RateLimited);
        }
        let entry = entries.entry(device.clone()).or_insert(Entry {
            since: Instant::now(),
            count: 0,
            active: None,
        });
        if entry.since.elapsed() >= Duration::from_secs(60) {
            entry.since = Instant::now();
            entry.count = 0;
        }
        if entry.active.is_some() || entry.count >= 6 {
            return Err(Failure::RateLimited);
        }
        let (tx, rx) = watch::channel(false);
        entry.active = Some((id.into(), tx));
        entry.count += 1;
        Ok((
            Lease {
                drafts: self.clone(),
                device,
            },
            rx,
        ))
    }
    fn cancel(&self, device: &str, id: &str) {
        if let Ok(entries) = self.0.lock() {
            if let Some((active_id, tx)) = entries.get(device).and_then(|e| e.active.as_ref()) {
                if active_id == id {
                    let _ = tx.send(true);
                }
            }
        }
    }
}

pub(super) async fn readiness(home: crate::session::paths::LatchHome) -> GitDraftReadiness {
    let state = tokio::task::spawn_blocking(move || match config::load(&home) {
        Ok(_) => GitDraftReadinessState::Ready,
        Err(ConfigError::Missing) => GitDraftReadinessState::Unconfigured,
        Err(ConfigError::Unavailable) => GitDraftReadinessState::Unavailable,
    })
    .await
    .unwrap_or(GitDraftReadinessState::Unavailable);
    GitDraftReadiness { state }
}

pub(super) async fn draft(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 8192).await {
        Ok(bytes) => bytes,
        Err(_) => return failure(None, Failure::InvalidRequest),
    };
    let request: GitCommitMessageRequest = match decode(&body) {
        Ok(value) => value,
        Err(code) => return failure(None, code),
    };
    let id = request.request_id.clone();
    if !super::is_request_id(&id)
        || request.path.len() > 4096
        || !request.path.starts_with('/')
        || request.path.contains('\0')
        || request
            .expected
            .branch
            .as_ref()
            .is_some_and(|v| v.len() > 1024)
    {
        return failure(Some(id), Failure::InvalidRequest);
    }
    let (_lease, cancelled) = match state
        .drafts
        .begin(device.0.unwrap_or_else(|| "local".into()), &id)
    {
        Ok(value) => value,
        Err(code) => return failure(Some(id), code),
    };
    let work = async {
        let home = state.home.clone();
        let provider = tokio::task::spawn_blocking(move || config::load(&home))
            .await
            .map_err(|_| Failure::ProviderError)?
            .map_err(|e| match e {
                ConfigError::Missing => Failure::Unconfigured,
                ConfigError::Unavailable => Failure::ProviderError,
            })?;
        generate(request, |context| async move {
            commit_message_provider::generate(provider, &context).await
        })
        .await
    };
    let result = run_until_cancelled(work, cancelled, Duration::from_secs(65)).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(code) => failure(Some(id), code),
    }
}

async fn run_until_cancelled<T>(
    work: impl std::future::Future<Output = Result<T, Failure>>,
    mut cancelled: watch::Receiver<bool>,
    deadline: Duration,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        _ = cancelled.changed() => Err(Failure::Cancelled),
        result = tokio::time::timeout(deadline, work) => result.unwrap_or(Err(Failure::TimedOut)),
    }
}

async fn generate<F, Fut>(
    request: GitCommitMessageRequest,
    provider: F,
) -> Result<GitCommitMessageResponse, Failure>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<commit_message_provider::Draft, Failure>>,
{
    let context = collect(&request.path, &request.expected).await?;
    context.revalidate().await?;
    let draft = provider(context.cloud_context.clone()).await?;
    context.revalidate().await?;
    Ok(GitCommitMessageResponse {
        request_id: request.request_id,
        summary: draft.summary,
        body: draft.body,
        source: context.source,
        coverage: context.coverage,
    })
}

fn decode(body: &[u8]) -> Result<GitCommitMessageRequest, Failure> {
    if body.len() > 8192 {
        return Err(Failure::InvalidRequest);
    }
    serde_json::from_slice(body).map_err(|_| Failure::InvalidRequest)
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 256).await {
        Ok(bytes) => bytes,
        Err(_) => return failure(None, Failure::InvalidRequest),
    };
    let request: crate::cli::serve::contract::GitCommitMessageCancelRequest =
        match serde_json::from_slice(&body) {
            Ok(value) if body.len() <= 256 => value,
            _ => return failure(None, Failure::InvalidRequest),
        };
    if !super::is_request_id(&request.request_id) {
        return failure(None, Failure::InvalidRequest);
    }
    state
        .drafts
        .cancel(device.0.as_deref().unwrap_or("local"), &request.request_id);
    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn failure(id: Option<String>, code: Failure) -> Response {
    let (status, message) = match code {
        Failure::InvalidRequest => (StatusCode::BAD_REQUEST, "Invalid draft request."),
        Failure::Unauthorized => (
            StatusCode::FORBIDDEN,
            "Control access is required for cloud drafting.",
        ),
        Failure::Unconfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Set a Gemini credential on your Mac to generate a draft.",
        ),
        Failure::NoStagedChanges => (
            StatusCode::CONFLICT,
            "Stage changes before generating a draft.",
        ),
        Failure::StaleSource => (
            StatusCode::CONFLICT,
            "Staged changes changed. Review and generate again.",
        ),
        Failure::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "Drafting is busy or rate limited. Try again later.",
        ),
        Failure::TimedOut => (StatusCode::GATEWAY_TIMEOUT, "Draft generation timed out."),
        Failure::Cancelled => (StatusCode::CONFLICT, "Draft generation cancelled."),
        Failure::ProviderRefusal => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Google declined to generate a draft.",
        ),
        Failure::UnusableContext | Failure::ContextTooLarge => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Staged context cannot be safely sent. Write a message manually.",
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "Draft generation is unavailable. Check Mac configuration or write a message manually.",
        ),
    };
    let retry_after_seconds = if code == Failure::RateLimited {
        Some(60)
    } else {
        None
    };
    (
        status,
        Json(GitCommitMessageFailure {
            request_id: id,
            code,
            message: message.into(),
            retry_after_seconds,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn draft_provider_smoke_uses_only_staged_context_and_returns_original_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.invalid"],
        ] {
            assert!(std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(root.join("readme.txt"), "staged-example\n").unwrap();
        assert!(std::process::Command::new("git")
            .current_dir(&root)
            .args(["add", "readme.txt"])
            .status()
            .unwrap()
            .success());
        std::fs::write(root.join("readme.txt"), "unstaged-example\n").unwrap();
        let home = crate::session::paths::LatchHome::new(dir.path().join("home"));
        home.ensure().unwrap();
        let path = root.to_str().unwrap();
        let status = crate::cli::serve::git::status(&home, path, &[])
            .await
            .unwrap();
        let expected = crate::cli::serve::contract::GitDraftExpected {
            branch: status.preconditions.branch,
            head: status.preconditions.head,
            index: status.preconditions.index,
        };
        let original_index = std::fs::read(root.join(".git/index")).unwrap();
        let response = generate(GitCommitMessageRequest {
            request_id: "12345678-1234-1234-1234-123456789abc".into(), path: path.into(), expected: expected.clone(),
        }, |context| async move {
            assert!(context.contains("staged-example"));
            assert!(!context.contains("unstaged-example"));
            assert!(!context.contains(path));
            commit_message_provider::parse(br#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"{\"summary\":\"Add a readme example\",\"body\":null}"}]}}]}"#)
        }).await.unwrap();
        assert_eq!(response.summary, "Add a readme example");
        assert_eq!(response.source.index, expected.index);
        assert_eq!(response.source.head, None);
        assert_eq!(response.coverage.included_files, 1);
        assert_eq!(
            std::fs::read(root.join(".git/index")).unwrap(),
            original_index
        );
        assert!(matches!(
            commit_message_provider::parse(br#"{"promptFeedback":{"blockReason":"SAFETY"}}"#),
            Err(Failure::ProviderRefusal)
        ));
        assert!(matches!(commit_message_provider::parse(br#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"{\"summary\":\"bad\\nsubject\",\"body\":null}"}]}}]}"#), Err(Failure::ProviderError)));
    }

    #[tokio::test]
    async fn draft_limits_and_cancellation_are_device_scoped() {
        let drafts = Drafts::default();
        let (lease, mut rx) = drafts
            .begin("phone-a".into(), "request-a")
            .unwrap_or_else(|_| panic!("admit"));
        assert!(matches!(
            drafts.begin("phone-a".into(), "other"),
            Err(Failure::RateLimited)
        ));
        drafts.cancel("phone-b", "request-a");
        assert!(!*rx.borrow());
        drafts.cancel("phone-a", "request-a");
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
        drop(lease);
        assert!(drafts.begin("phone-a".into(), "other").is_ok());
    }

    #[tokio::test]
    async fn draft_rejects_source_changes_while_provider_is_pending_and_skips_provider_when_stale()
    {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("safe.txt"), "staged data\n").unwrap();
        git(&["add", "."]);
        let home = crate::session::paths::LatchHome::new(dir.path().join("home"));
        home.ensure().unwrap();
        let status = crate::cli::serve::git::status(&home, root.to_str().unwrap(), &[])
            .await
            .unwrap();
        let request = GitCommitMessageRequest {
            request_id: "12345678-1234-1234-1234-123456789abc".into(),
            path: root.to_str().unwrap().into(),
            expected: crate::cli::serve::contract::GitDraftExpected {
                branch: status.preconditions.branch,
                head: status.preconditions.head,
                index: status.preconditions.index,
            },
        };
        let response = generate(request.clone(), |_| async {
            std::fs::write(root.join("safe.txt"), "new staged data\n").unwrap();
            git(&["add", "safe.txt"]);
            Ok(commit_message_provider::Draft {
                summary: "Update safe data".into(),
                body: None,
            })
        })
        .await;
        assert!(matches!(response, Err(Failure::StaleSource)));
        assert!(matches!(
            generate(request, |_| async {
                panic!("stale requests must never disclose source")
            })
            .await,
            Err(Failure::StaleSource)
        ));
    }

    #[test]
    fn draft_admission_enforces_global_device_and_window_limits_and_releases_leases() {
        let drafts = Drafts::default();
        let mut leases = Vec::new();
        for i in 0..4 {
            leases.push(
                drafts
                    .begin(format!("phone-{i}"), "id")
                    .unwrap_or_else(|_| panic!("admit"))
                    .0,
            );
        }
        assert!(matches!(
            drafts.begin("fifth".into(), "id"),
            Err(Failure::RateLimited)
        ));
        leases.clear();
        for _ in 0..6 {
            drop(
                drafts
                    .begin("rate".into(), "id")
                    .unwrap_or_else(|_| panic!("admit")),
            );
        }
        assert!(matches!(
            drafts.begin("rate".into(), "id"),
            Err(Failure::RateLimited)
        ));
        drafts.0.lock().unwrap().get_mut("rate").unwrap().since =
            Instant::now() - Duration::from_secs(61);
        assert!(drafts.begin("rate".into(), "id").is_ok());
        for i in 0..256 {
            drop(drafts.begin(format!("bounded-{i}"), "id"));
        }
        assert_eq!(drafts.0.lock().unwrap().len(), 256);
        assert!(matches!(
            drafts.begin("too-many".into(), "id"),
            Err(Failure::RateLimited)
        ));
    }

    #[tokio::test]
    async fn draft_cancel_and_deadline_drop_work_and_release_admission() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        for cancel in [true, false] {
            let drafts = Drafts::default();
            let (lease, rx) = drafts
                .begin("phone".into(), "id")
                .unwrap_or_else(|_| panic!("admit"));
            let dropped = Arc::new(AtomicBool::new(false));
            let guard = Dropped(dropped.clone());
            let work = async move {
                let _guard = guard;
                std::future::pending::<Result<(), Failure>>().await
            };
            if cancel {
                drafts.cancel("phone", "id");
            }
            let result = run_until_cancelled(work, rx, Duration::from_millis(1)).await;
            assert_eq!(
                result,
                Err(if cancel {
                    Failure::Cancelled
                } else {
                    Failure::TimedOut
                })
            );
            assert!(dropped.load(Ordering::SeqCst));
            drop(lease);
            assert!(drafts.begin("phone".into(), "next").is_ok());
        }
    }
}
