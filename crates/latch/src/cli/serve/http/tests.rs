use super::*;
use crate::cli::serve::{attachments, routes::DEVICE_GRANT_HEADER};
use axum::http::header;

#[test]
fn every_registered_handler_comes_from_the_shared_route_table() {
    assert_eq!(ROUTES.len(), 33);
    let mut ids = ROUTES.iter().map(|route| route.id).collect::<Vec<_>>();
    ids.sort_by_key(|id| *id as u8);
    ids.dedup();
    assert_eq!(ids.len(), ROUTES.len());
}

#[tokio::test]
async fn git_wire_bound_counts_json_escaping_and_never_sends_a_512_kib_payload() {
    let error = git::bounded_json(StatusCode::OK, "\u{1}".repeat(100_000)).unwrap_err();
    assert_eq!(error.code, "response_too_large");
    let response = error.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), 512 * 1024)
        .await
        .unwrap();
    assert!(bytes.len() < 512 * 1024);
    let response = git::bounded_json(StatusCode::ACCEPTED, "small").unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[test]
fn missing_session_is_404_without_internal_detail() {
    let mapped = map_engine_error(
        SessionLookupError::UnknownName {
            session: "secret-name".to_owned(),
        }
        .into(),
    );
    assert_eq!(mapped.status, StatusCode::NOT_FOUND);
    assert_eq!(mapped.code, "session_not_found");
    assert_eq!(mapped.message, "session not found");
    assert!(!mapped.message.contains("secret-name"));
}

#[test]
fn current_capabilities_advertise_browsing_and_creation() {
    let value = serde_json::to_value(GatewayEndpoints {
        sessions: true,
        preview: true,
        terminal: true,
        conversation: true,
        browse_directories: true,
        create_session: true,
        stop_session: true,
        attachments: true,
        agent_models: true,
        notification_key: true,
        git_repositories: true,
        git_resolve: true,
        git_status: true,
        git_diff: true,
        git_branches: true,
        git_stage: true,
        git_unstage: true,
        git_commit: true,
        git_create_branch: true,
        git_switch_branch: true,
        git_receipts: true,
        git_sync: true,
        git_fetch: true,
        git_fast_forward: true,
        git_merge: true,
        git_rebase: true,
        git_continue: true,
        git_abort: true,
        git_stage_resolved: true,
        git_push_preview: true,
        git_push: true,
        git_push_reconcile: true,
    })
    .unwrap();
    for name in [
        "gitRepositories",
        "gitResolve",
        "gitStatus",
        "gitDiff",
        "gitBranches",
        "gitStage",
        "gitUnstage",
        "gitCommit",
        "gitCreateBranch",
        "gitSwitchBranch",
        "gitReceipts",
        "gitSync",
        "gitFetch",
        "gitFastForward",
        "gitMerge",
        "gitRebase",
        "gitContinue",
        "gitAbort",
        "gitStageResolved",
        "gitPushPreview",
        "gitPush",
        "gitPushReconcile",
    ] {
        assert_eq!(value[name], true, "{name}");
    }
    assert_eq!(value["notificationKey"], true);
    assert_eq!(value["browseDirectories"], true);
    assert_eq!(value["createSession"], true);
    assert_eq!(value["stopSession"], true);
    assert_eq!(value["agentModels"], true);
}

/// The create route names the agents it launches so a phone shows only
/// controls this gateway serves; the wire spelling is the schema's enum.
#[test]
fn current_capabilities_advertise_supported_session_agents() {
    let value = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: SESSION_AGENTS.to_vec(),
        attachment_max_bytes: None,
    })
    .unwrap();
    assert_eq!(
        value["sessionAgents"],
        serde_json::json!(["claude", "codex"])
    );
    // A gateway that launches no agents omits the key, which an older
    // client decoding a closed object still accepts.
    let none = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: None,
    })
    .unwrap();
    assert!(none.get("sessionAgents").is_none());
    assert!(none.get("attachmentMaxBytes").is_none());
}

#[test]
fn cors_allows_the_bounded_post_route() {
    let mut headers = HeaderMap::new();
    apply_cors(&mut headers, None);
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET, POST, OPTIONS"
    );
}

struct Harness {
    _dir: tempfile::TempDir,
    address: SocketAddr,
    home: LatchHome,
    work: std::path::PathBuf,
}

/// The production router on a real socket, so creation requests travel the
/// same token, grant, and route-table path a phone's do.
async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp");
    let home = LatchHome::new(dir.path().join("latch"));
    home.ensure().expect("home");
    let work = dir.path().join("work");
    std::fs::create_dir(&work).expect("work");
    let token_file = dir.path().join("serve.token");
    std::fs::write(&token_file, "gateway-token").expect("token");
    let hub = crate::conversation::ConversationHub::new(dir.path().join("hub")).expect("hub");
    let app = test_router(home.clone(), token_file, hub, dir.path().join("stub-latch"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Harness {
        _dir: dir,
        address,
        home,
        work,
    }
}

async fn post_create(
    harness: &Harness,
    grant: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    send(harness, "POST", "/v2/sessions", grant, body).await
}

#[tokio::test]
async fn loopback_gateway_rejects_duplicate_grant_headers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let harness = harness().await;
    let request = format!(
        "GET /v2/sessions HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\n{DEVICE_GRANT_HEADER}: observe\r\n{DEVICE_GRANT_HEADER}: control\r\nConnection: close\r\n\r\n"
    );
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 400 "),
        "{}",
        String::from_utf8_lossy(&response)
    );
}

async fn send(
    harness: &Harness,
    method: &str,
    target: &str,
    grant: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let grant = grant.map_or_else(String::new, |grant| {
        format!("{DEVICE_GRANT_HEADER}: {grant}\r\n")
    });
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/json\r\nConnection: close\r\n{grant}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status line");
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let payload = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    (status, payload)
}

fn create_body(cwd: &str) -> String {
    serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": cwd,
    })
    .to_string()
}

#[tokio::test]
async fn creation_refuses_malformed_requests_before_touching_the_engine() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap().to_owned();
    let cases = [
        ("not json", 400, "invalid_request"),
        (r#"{"cwd":"/tmp"}"#, 400, "invalid_request"),
        (
            &serde_json::json!({"requestId": "not-a-uuid", "cwd": work}).to_string(),
            400,
            "invalid_request",
        ),
        (&create_body("relative/path"), 400, "invalid_path"),
        // An agent kind this gateway does not advertise is refused as a
        // malformed request, before any directory or shell lookup.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "unsupported-agent",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        // A model belongs to an agent, and is only ever an id: never a
        // shell's model, and never something an agent could read as a flag.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "model": "claude-opus-5-5",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "claude",
                "model": "--dangerously-skip-permissions",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        // A well-formed id the agent does not list is a stale choice: the
        // phone is told to choose again, and no agent is looked up.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "codex",
                "model": "no-such-model-anywhere",
            })
            .to_string(),
            422,
            "model_unavailable",
        ),
        (
            &create_body(harness._dir.path().join("missing").to_str().unwrap()),
            404,
            "unavailable_path",
        ),
        (
            &create_body(harness._dir.path().join("serve.token").to_str().unwrap()),
            400,
            "invalid_path",
        ),
    ];
    for (body, status, code) in cases {
        let (observed, payload) = post_create(&harness, None, body).await;
        assert_eq!(observed, status, "{body}");
        assert_eq!(payload["error"], code, "{body}");
    }
    // A refused request is a refused request: nothing was created.
    assert!(harness.home.session_ids().unwrap().is_empty());
}

#[tokio::test]
async fn agent_models_are_listed_at_control_for_known_agents_only() {
    let harness = harness().await;
    for agent in ["claude", "codex"] {
        let target = format!("/v2/agents/{agent}/models");
        let (status, payload) = send(&harness, "GET", &target, Some("control"), "").await;
        assert_eq!(status, 200, "{agent}");
        assert_eq!(payload["agent"], agent);
        let models = payload["models"].as_array().expect("models");
        assert!(!models.is_empty(), "{agent} always has a list");
        for model in models {
            assert!(agent_models::is_model_id(model["id"].as_str().unwrap()));
            assert!(!model["name"].as_str().unwrap().is_empty());
        }
        assert!(payload.get("defaultModel").is_some());
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", &target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not list {agent} models");
        }
    }
    let (status, payload) = send(
        &harness,
        "GET",
        "/v2/agents/cursor/models",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(payload["error"], "not_found");
}

#[tokio::test]
async fn creation_is_refused_below_control_and_oversized_bodies_are_bounded() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap();
    let claude = serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": work,
        "agent": "claude",
    })
    .to_string();
    for grant in ["observe", "interact"] {
        let (status, _) = post_create(&harness, Some(grant), &create_body(work)).await;
        assert_eq!(status, 403, "{grant} must not create sessions");
        let (status, _) = post_create(&harness, Some(grant), &claude).await;
        assert_eq!(status, 403, "{grant} must not launch agents");
    }
    let oversized = serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": "/".to_owned() + &"a".repeat(MAX_CREATE_BODY_BYTES),
    })
    .to_string();
    let (status, payload) = post_create(&harness, Some("control"), &oversized).await;
    assert_eq!(status, 413);
    assert_eq!(payload["error"], "invalid_request");
    assert!(harness.home.session_ids().unwrap().is_empty());
}

/// Ending what is running on the Mac is a control operation. An observing
/// or interacting phone must not reach it, and the refusal has to happen
/// before the engine is asked anything.
#[tokio::test]
async fn stopping_is_refused_below_the_control_grant() {
    let harness = harness().await;
    for grant in ["observe", "interact"] {
        let (status, _) = send(
            &harness,
            "POST",
            "/v2/sessions/ses_missing/stop",
            Some(grant),
            "",
        )
        .await;
        assert_eq!(status, 403, "{grant} must not stop sessions");
    }
}

/// A stale list is the ordinary case for a phone: the row it tapped may
/// already be gone. That answers 404, not an internal error, and it never
/// echoes the name back.
#[tokio::test]
async fn stopping_an_unknown_session_is_a_plain_not_found() {
    let harness = harness().await;
    let (status, payload) = send(
        &harness,
        "POST",
        "/v2/sessions/ses_missing/stop",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
    // The code is what a client branches on: a bare 404 from a gateway
    // that never had the route carries no `error` at all, and this must
    // never be mistaken for it.
    assert_eq!(payload["error"], "session_not_found");
    assert_eq!(payload["reason"], "session not found");
    assert!(!payload["reason"].to_string().contains("ses_missing"));
}

/// The route table is method-scoped, so a read of the same path is not a
/// route at all. Nothing about a session can be ended by fetching a URL.
#[tokio::test]
async fn a_get_on_the_stop_path_is_not_a_route() {
    let harness = harness().await;
    let (status, _) = send(
        &harness,
        "GET",
        "/v2/sessions/ses_missing/stop",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
}

const ATTACHMENT_SESSION: &str = "ses_attachtest";

/// Records one session whose working directory is the harness's `work`
/// folder. No daemon runs: the attachments route reads only the record.
fn record_attachment_session(harness: &Harness) {
    use crate::session::manifest::{SourceInfo, TerminalSize};
    use crate::session::meta::{self, SessionMeta};
    use crate::session::paths::SessionId;

    let id = SessionId::parse(ATTACHMENT_SESSION).expect("session id");
    let paths = harness.home.session(&id);
    paths.ensure().expect("session dir");
    meta::write_once(
        &paths,
        &SessionMeta {
            format_version: 1,
            id: id.as_str().to_owned(),
            name: "attach".into(),
            title: None,
            cwd: harness.work.clone(),
            command_label: "claude".into(),
            harness: None,
            created_at: "2026-09-25T00:00:00Z".into(),
            initial_size: TerminalSize::new(80, 24),
            source: SourceInfo {
                kind: "test".into(),
                external_run_id: None,
            },
        },
    )
    .expect("write meta");
}

/// Sends raw bytes, so a test can lie about `Content-Length` or omit it.
async fn send_raw(harness: &Harness, request: Vec<u8>) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream.write_all(&request).await.expect("write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status line");
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    (
        status,
        serde_json::from_str(payload).unwrap_or(serde_json::Value::Null),
    )
}

fn upload_request(target: &str, grant: Option<&str>, length: Option<u64>, body: &[u8]) -> Vec<u8> {
    let grant = grant.map_or_else(String::new, |grant| {
        format!("{DEVICE_GRANT_HEADER}: {grant}\r\n")
    });
    let length = length.map_or_else(String::new, |length| {
        format!("Content-Length: {length}\r\n")
    });
    let mut request = format!(
        "POST {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n{grant}{length}\r\n"
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

fn attachments_in(harness: &Harness) -> Vec<String> {
    let folder = harness.work.join(attachments::ATTACHMENTS_DIR);
    let mut names = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != ".gitignore")
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The happy path: the file lands under the session's own folder with a
/// sanitized name, and the receipt carries the path the message will use.
#[tokio::test]
async fn an_attachment_lands_in_the_session_workspace() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=Screen%20Shot.PNG");
    let (status, receipt) = send_raw(
        &harness,
        upload_request(&target, Some("interact"), Some(5), b"hello"),
    )
    .await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["name"], "Screen-Shot.png");
    assert_eq!(
        receipt["relativePath"],
        ".latch-attachments/Screen-Shot.png"
    );
    assert_eq!(receipt["bytes"], 5);
    let path = std::path::PathBuf::from(receipt["path"].as_str().unwrap());
    assert!(path.starts_with(harness.work.canonicalize().unwrap()));
    assert_eq!(std::fs::read(path).unwrap(), b"hello");
}

/// Writing into the workspace is part of sending a message: an observing
/// phone is refused before the engine or the filesystem is touched.
#[tokio::test]
async fn attachments_are_refused_below_the_interact_grant() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let (status, _) = send_raw(
        &harness,
        upload_request(&target, Some("observe"), Some(1), b"x"),
    )
    .await;
    assert_eq!(status, 403);
    assert!(!harness.work.join(attachments::ATTACHMENTS_DIR).exists());
}

#[tokio::test]
async fn attachment_bodies_must_declare_a_bounded_nonzero_length() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let cases = [
        (None, b"x".as_slice(), 411, "invalid_request"),
        (Some(0), b"".as_slice(), 400, "invalid_request"),
        (
            Some(ATTACHMENT_MAX_BYTES + 1),
            b"x".as_slice(),
            413,
            "attachment_too_large",
        ),
    ];
    for (length, body, status, code) in cases {
        let (observed, payload) = send_raw(
            &harness,
            upload_request(&target, Some("interact"), length, body),
        )
        .await;
        assert_eq!(observed, status, "{length:?}");
        assert_eq!(payload["error"], code, "{length:?}");
    }
    // A refused length is refused before the folder is even created.
    assert!(!harness.work.join(attachments::ATTACHMENTS_DIR).exists());
}

/// A body that stops short of what it declared leaves no partial file for
/// an agent to find.
#[tokio::test]
async fn a_truncated_attachment_leaves_nothing_behind() {
    use tokio::io::AsyncWriteExt;

    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    // The phone promises ten bytes, sends five, and goes away.
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream
        .write_all(&upload_request(
            &target,
            Some("interact"),
            Some(10),
            b"short",
        ))
        .await
        .expect("write request");
    let mut appeared = false;
    for _ in 0..100 {
        appeared = !attachments_in(&harness).is_empty();
        if appeared {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        appeared,
        "the partial file is created while the body streams"
    );
    drop(stream);
    // The handler cleans up when its future ends; give it a moment.
    for _ in 0..100 {
        if attachments_in(&harness).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(attachments_in(&harness).is_empty());
}

#[tokio::test]
async fn attaching_to_an_unknown_session_is_a_plain_not_found() {
    let harness = harness().await;
    let (status, payload) = send_raw(
        &harness,
        upload_request(
            "/v2/sessions/ses_missing/attachments?name=a.txt",
            Some("interact"),
            Some(1),
            b"x",
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(payload["error"], "session_not_found");
}

/// A symlink where the folder belongs would let a planted link send the
/// write outside the workspace. The route refuses it with a stable code
/// and writes nothing at the link's target.
#[tokio::test]
async fn a_symlinked_attachments_folder_is_refused() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let elsewhere = harness._dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, harness.work.join(attachments::ATTACHMENTS_DIR))
        .unwrap();
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let (status, payload) = send_raw(
        &harness,
        upload_request(&target, Some("interact"), Some(1), b"x"),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(payload["error"], "attachments_folder_unsafe");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn current_capabilities_advertise_attachments_with_their_limit() {
    let value = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: Some(ATTACHMENT_MAX_BYTES),
    })
    .unwrap();
    assert_eq!(value["attachmentMaxBytes"], ATTACHMENT_MAX_BYTES);
}

#[test]
fn only_a_canonical_uuid_is_accepted_as_a_request_id() {
    assert!(is_request_id("8cba5d78-79a0-4a55-9047-f77e57e463c7"));
    assert!(is_request_id("8CBA5D78-79A0-4A55-9047-F77E57E463C7"));
    for rejected in [
        "",
        "8cba5d78",
        "8cba5d78-79a0-4a55-9047-f77e57e463c",
        "8cba5d78-79a0-4a55-9047-f77e57e463c7-extra",
        "8cba5d78-79a0-4a55-9047-f77e57e463cg",
        "8cba5d7879a04a559047f77e57e463c7",
        "../../etc/passwd",
    ] {
        assert!(!is_request_id(rejected), "{rejected} must be refused");
    }
}

#[test]
fn creation_failures_have_stable_codes_without_engine_detail() {
    let conflict = map_remote_session_error(RemoteSessionError::RequestIdConflict);
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.code, "request_id_conflict");

    let failed = map_remote_session_error(RemoteSessionError::Failed(anyhow::anyhow!(
        "cannot spawn /opt/homebrew/bin/latchd for /Users/person/secret"
    )));
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(failed.code, "session_creation_failed");
    assert!(!failed.message.contains("/Users/"));
    assert!(!failed.message.contains("latchd"));

    // A missing agent is a stable, phone-readable refusal that names the
    // product, not a path on the Mac.
    let missing =
        map_remote_session_error(RemoteSessionError::AgentUnavailable(SessionAgent::Claude));
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(missing.code, "agent_unavailable");
    assert!(missing.message.contains("Claude Code"));
    assert!(!missing.message.contains('/'));
}

#[test]
fn directory_failures_have_stable_codes_without_path_detail() {
    let cases = [
        (BrowseError::InvalidPath, "invalid_path"),
        (BrowseError::UnavailablePath, "unavailable_path"),
        (BrowseError::UnreadableDirectory, "unreadable_directory"),
        (BrowseError::StaleCursor, "stale_cursor"),
    ];
    for (error, code) in cases {
        let mapped = map_browse_error(error);
        assert_eq!(mapped.code, code);
        assert!(!mapped.message.contains("/Users/"));
    }
}

/// Every Git route is refused below the control grant on the loopback
/// router, before any folder is resolved or Git is run, and reached at
/// control through the same route table.
#[tokio::test]
async fn git_routes_are_refused_below_the_control_grant() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap().replace('/', "%2F");
    let targets = [
        "/v2/git/repositories".to_owned(),
        format!("/v2/git/resolve?path={work}"),
        format!("/v2/git/status?path={work}"),
        format!("/v2/git/diff?path={work}&file=61&area=unstaged"),
    ];
    for target in &targets {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
        let (status, body) = send(&harness, "GET", target, Some("control"), "").await;
        assert_ne!(status, 403, "control reaches {target}: {body}");
        // Git routes are reads; a write method is not a route at all.
        let (status, _) = send(&harness, "POST", target, Some("control"), "").await;
        assert!(status == 404 || status == 405, "POST {target} -> {status}");
    }
}

/// A folder that is not a repository is a stable code with no path in it.
#[tokio::test]
async fn git_refusals_have_stable_codes_without_path_detail() {
    let harness = harness().await;
    let plain = tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("plain folder outside any repository");
    let path = plain.path().to_str().unwrap().replace('/', "%2F");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/status?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"], "not_a_repository");
    assert!(!body["reason"].as_str().unwrap().contains("/tmp"));

    let (status, body) = send(&harness, "GET", "/v2/git/status", Some("control"), "").await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/diff?path={path}&file=61&area=everything"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");
}

/// Sends `body` as the loopback proxy would for a paired device.
async fn send_as_device(
    harness: &Harness,
    method: &str,
    target: &str,
    device: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/json\r\nConnection: close\r\n{DEVICE_GRANT_HEADER}: control\r\n{}: {device}\r\nContent-Length: {}\r\n\r\n{body}",
        crate::cli::serve::routes::DEVICE_ID_HEADER,
        body.len()
    );
    send_raw(harness, request.into_bytes()).await
}

/// Runs Git for fixture setup without any global configuration.
fn fixture_git(dir: &std::path::Path, words: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(words)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {words:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository in the harness's work folder with one committed file and
/// one unstaged edit.
fn harness_repository(harness: &Harness) -> String {
    let root = std::fs::canonicalize(&harness.work).unwrap();
    fixture_git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Latch Test"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        fixture_git(&root, &["config", key, value]);
    }
    std::fs::write(root.join("README.md"), "one\n").unwrap();
    fixture_git(&root, &["add", "README.md"]);
    fixture_git(&root, &["commit", "-q", "-m", "initial"]);
    std::fs::write(root.join("README.md"), "two\n").unwrap();
    root.to_str().unwrap().to_owned()
}

const GIT_MUTATIONS: [&str; 13] = [
    "/v2/git/stage",
    "/v2/git/unstage",
    "/v2/git/commit",
    "/v2/git/branches/create",
    "/v2/git/branches/switch",
    "/v2/git/fetch",
    "/v2/git/fast-forward",
    "/v2/git/merge",
    "/v2/git/rebase",
    "/v2/git/continue",
    "/v2/git/abort",
    "/v2/git/push",
    "/v2/git/push/reconcile",
];

/// Every Git mutation, the branch list and the receipt lookup are refused
/// below control on the loopback router, before the body is read or any
/// folder resolved, and are not reachable with another method.
#[tokio::test]
async fn git_mutations_are_refused_below_the_control_grant() {
    let harness = harness().await;
    for target in GIT_MUTATIONS {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "POST", target, Some(grant), "{}").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
        let (status, body) = send(&harness, "POST", target, Some("control"), "{}").await;
        assert_eq!(
            status, 400,
            "control reaches {target} and its body is checked: {body}"
        );
        let (status, _) = send(&harness, "GET", target, Some("control"), "").await;
        assert!(status == 404 || status == 405, "GET {target} -> {status}");
    }
    let work = harness.work.to_str().unwrap().replace('/', "%2F");
    for target in [
        format!("/v2/git/branches?path={work}"),
        format!("/v2/git/sync?path={work}"),
        format!("/v2/git/push/preview?path={work}"),
        "/v2/git/receipts/0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e".to_owned(),
    ] {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", &target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
    }
}

/// A mutation is shaped and typed before anything is accepted: no free-form
/// field, no Git option, no path outside the working tree.
#[tokio::test]
async fn git_mutation_bodies_are_checked_before_acceptance() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let expected = serde_json::json!({
        "branch": "main",
        "head": "0".repeat(40),
        "index": "0".repeat(64),
        "worktree": "0".repeat(64),
    });
    let file = |key: &str, area: &str| serde_json::json!({"pathKey": key, "area": area, "fingerprint": "0".repeat(32)});
    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let cases = [
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": "not-a-uuid", "path": root, "expected": expected, "files": [file("61", "unstaged")]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "unstaged")], "args": ["--force"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "unstaged")], "all": ["untracked"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "all": ["conflicted"]}),
        ),
        (
            "/v2/git/unstage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "staged")], "confirmConflictMarkers": true}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "--upload-pack=touch /tmp/x"}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin main"}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "refspec": "+refs/*:refs/*"}),
        ),
        (
            "/v2/git/fast-forward",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "origin/main"}),
        ),
        (
            "/v2/git/merge",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "-Xours", "upstreamOid": "0".repeat(40)}),
        ),
        (
            "/v2/git/merge",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "0".repeat(40), "strategy": "ours"}),
        ),
        (
            "/v2/git/rebase",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "0".repeat(40), "interactive": true}),
        ),
        (
            "/v2/git/continue",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "cherry_pick"}),
        ),
        (
            "/v2/git/abort",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "rebase", "confirmed": true, "hard": true}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("2e2e2f6574632f706173737764", "unstaged")]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("2f6574632f706173737764", "unstaged")]}),
        ),
        (
            "/v2/git/unstage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "all": ["unstaged"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": {"branch": "main", "head": "HEAD", "index": "x", "worktree": "y"}, "all": ["unstaged"]}),
        ),
        (
            "/v2/git/commit",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "two\nlines"}),
        ),
        (
            "/v2/git/commit",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "   "}),
        ),
        (
            "/v2/git/branches/create",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "--force", "startPoint": "HEAD", "startOid": "0".repeat(40), "switch": false}),
        ),
        (
            "/v2/git/branches/create",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "ok", "startPoint": "origin/main~3", "startOid": "0".repeat(40), "switch": false}),
        ),
        (
            "/v2/git/branches/switch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "@{-1}", "oid": "0".repeat(40)}),
        ),
        (
            "/v2/git/branches/switch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "main", "oid": "HEAD"}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null, "force": true}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "--receive-pack=touch /tmp/x", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "-f", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main:refs/tags/x", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "HEAD", "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "--all", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": "origin/main"}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null, "refspec": "+refs/heads/*:refs/heads/*"}),
        ),
        (
            "/v2/git/push/reconcile",
            serde_json::json!({"requestId": "not-a-uuid"}),
        ),
        (
            "/v2/git/push/reconcile",
            serde_json::json!({"requestId": id, "push": true}),
        ),
    ];
    for (target, body) in cases {
        let (status, reply) =
            send(&harness, "POST", target, Some("control"), &body.to_string()).await;
        assert_eq!(status, 400, "{target} {body}: {reply}");
        assert_eq!(reply["error"], "invalid_request");
    }
    // The push preview takes no option or revision expression either.
    let folder = root.replace('/', "%2F");
    for query in [
        "remote=--upload-pack%3Dtouch%20%2Ftmp%2Fx&destination=main",
        "remote=origin&destination=main~1",
        "remote=origin&destination=HEAD",
        "remote=origin",
        "destination=main",
    ] {
        let target = format!("/v2/git/push/preview?path={folder}&{query}");
        let (status, reply) = send(&harness, "GET", &target, Some("control"), "").await;
        assert_eq!(status, 400, "{target}: {reply}");
        assert_eq!(reply["error"], "invalid_request");
    }
    // Abort is refused unless the user confirmed it.
    for body in [
        serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "merge"}),
        serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "merge", "confirmed": false}),
    ] {
        let (status, reply) = send(
            &harness,
            "POST",
            "/v2/git/abort",
            Some("control"),
            &body.to_string(),
        )
        .await;
        assert_eq!(status, 400, "{body}: {reply}");
        assert_eq!(reply["error"], "confirmation_required");
    }
    // Nothing above was accepted.
    let (status, reply) = send(
        &harness,
        "GET",
        &format!("/v2/git/receipts/{id}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404, "{reply}");
    assert_eq!(reply["error"], "receipt_not_found");

    let large = serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "x", "description": "y".repeat(40 * 1024)});
    let (status, _) = send(
        &harness,
        "POST",
        "/v2/git/commit",
        Some("control"),
        &large.to_string(),
    )
    .await;
    assert_eq!(status, 413);
}

/// Through the real router: a stage is accepted for one device, replayed by
/// the same id without running again, refused when the id is reused for
/// something else, and its receipt is invisible to every other device.
#[tokio::test]
async fn git_receipts_are_scoped_to_the_device_through_the_router() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let encoded = root.replace('/', "%2F");
    let (status, status_body) = send_as_device(
        &harness,
        "GET",
        &format!("/v2/git/status?path={encoded}"),
        "phone-a",
        "",
    )
    .await;
    assert_eq!(status, 200, "{status_body}");
    let readme = status_body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "README.md")
        .unwrap()
        .clone();
    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let body = serde_json::json!({
        "requestId": id,
        "path": root,
        "expected": status_body["preconditions"],
        "files": [{"pathKey": readme["pathKey"], "area": "unstaged", "fingerprint": readme["fingerprint"]}],
    })
    .to_string();
    let (status, first) = send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &body).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["state"], "completed");
    assert_eq!(first["outcome"]["code"], "staged");

    let (status, replay) =
        send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &body).await;
    assert_eq!(status, 200);
    assert_eq!(replay, first);

    let reused = body.replace("\"area\":\"unstaged\"", "\"area\":\"untracked\"");
    assert_ne!(reused, body);
    let (status, conflict) =
        send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &reused).await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["error"], "request_id_conflict");

    let lookup = format!("/v2/git/receipts/{id}");
    let (status, mine) = send_as_device(&harness, "GET", &lookup, "phone-a", "").await;
    assert_eq!(status, 200);
    assert_eq!(mine, first);
    let (status, theirs) = send_as_device(&harness, "GET", &lookup, "phone-b", "").await;
    assert_eq!(status, 404, "{theirs}");
    assert_eq!(theirs["error"], "receipt_not_found");
    let (status, _) = send(&harness, "GET", &lookup, Some("control"), "").await;
    assert_eq!(status, 404);
}

/// The sync preview is a read at control: it serves what the last fetch
/// left (here, a repository with no remote) and refuses a cursor that no
/// longer describes the upstream.
#[tokio::test]
async fn git_sync_previews_without_fetching_and_maps_a_stale_cursor() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let path = root.replace('/', "%2F");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/sync?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["relation"], "no_upstream");
    assert_eq!(body["upstream"], serde_json::Value::Null);
    assert_eq!(body["incoming"], serde_json::json!([]));
    assert!(body["fastForwardBlockedReason"].is_string());
    assert!(body["preconditions"]["index"].is_string());

    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/sync?path={path}&cursor=v1-{}-50", "a".repeat(40)),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "stale_cursor");
}

/// The push preview is a read at control that never contacts a remote (here
/// there is none to contact); a push check is scoped to the device that
/// sent the push and is only for pushes.
#[tokio::test]
async fn git_push_preview_and_checks_through_the_router() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let path = root.replace('/', "%2F");
    let (status, preview) = send(
        &harness,
        "GET",
        &format!("/v2/git/push/preview?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["source"]["branch"], "main");
    assert_eq!(preview["target"], serde_json::Value::Null);
    assert_eq!(preview["selectionRequired"], false);
    assert!(preview["blockedReason"]
        .as_str()
        .unwrap()
        .contains("no remotes"));
    let (status, stale) = send(
        &harness,
        "GET",
        &format!("/v2/git/push/preview?path={path}&cursor=p1-a-b-50"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["error"], "stale_cursor");

    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let check = serde_json::json!({ "requestId": id }).to_string();
    let (status, missing) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-a",
        &check,
    )
    .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["error"], "receipt_not_found");

    // A receipt that is not a push cannot be checked against a remote, and
    // another device cannot even find it.
    let (_, status_body) = send_as_device(
        &harness,
        "GET",
        &format!("/v2/git/status?path={path}"),
        "phone-a",
        "",
    )
    .await;
    let readme = status_body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "README.md")
        .unwrap()
        .clone();
    let stage = serde_json::json!({
        "requestId": id,
        "path": root,
        "expected": status_body["preconditions"],
        "files": [{"pathKey": readme["pathKey"], "area": "unstaged", "fingerprint": readme["fingerprint"]}],
    })
    .to_string();
    let (status, _) = send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &stage).await;
    assert_eq!(status, 200);
    let (status, not_push) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-a",
        &check,
    )
    .await;
    assert_eq!(status, 409, "{not_push}");
    assert_eq!(not_push["error"], "not_a_push");
    let (status, theirs) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-b",
        &check,
    )
    .await;
    assert_eq!(status, 404, "{theirs}");
    assert_eq!(theirs["error"], "receipt_not_found");
}
