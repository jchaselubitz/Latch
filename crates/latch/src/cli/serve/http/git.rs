//! HTTP handlers for the Git routes.

use std::collections::HashMap;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use serde::de::DeserializeOwned;

use super::{is_request_id, session_report, ApiError, AppState, DeviceContext};
use crate::cli::json::SessionSummary;
use crate::cli::serve::contract::{
    GitChangeArea, GitCommitRequest, GitCreateBranchRequest, GitFetchRequest, GitIndexRequest,
    GitOperationControlRequest, GitPreconditions, GitPushReconcileRequest, GitPushReconciliation,
    GitPushRequest, GitSwitchBranchRequest, GitUpdateRequest, GitWorktreeSummaries,
    GitWorktreeSummaryError, GitWorktreeSummaryOutcome,
};
use crate::cli::serve::git::{
    self, FileTarget, GitError, Mutation, MutationError, Selection, Submitted,
};

/// Final wire bound, including JSON escaping and all header/session fields.
/// A mutation's receipt is retained even when its response cannot fit.
pub(super) fn bounded_json<T: serde::Serialize>(
    status: StatusCode,
    value: T,
) -> Result<Response, ApiError> {
    let bytes = serde_json::to_vec(&value).map_err(|_| {
        ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_too_large",
            "Git response could not be encoded. Read its receipt before trying another action.",
        )
    })?;
    if bytes.len() >= 512 * 1024 {
        return Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "response_too_large",
            "Git response is too large for this phone. Use Git on your Mac; read the receipt before trying another action."));
    }
    Ok((
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

/// Largest mutation body. The paired proxy buffers a whole request (headers
/// included) in 32 KiB; a local client gets the same bound.
const MAX_MUTATION_BODY: usize = 32 * 1024;
/// Files one stage or unstage request may name.
const MAX_FILES: usize = 200;
const MAX_SUMMARY_BYTES: usize = 1000;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;

#[derive(serde::Deserialize)]
pub(super) struct FolderQuery {
    path: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct SyncQuery {
    path: Option<String>,
    #[serde(rename = "worktreeRef")]
    worktree_ref: Option<String>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct PushQuery {
    path: Option<String>,
    #[serde(rename = "worktreeRef")]
    worktree_ref: Option<String>,
    remote: Option<String>,
    destination: Option<String>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct DiffQuery {
    path: Option<String>,
    file: Option<String>,
    area: Option<String>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorktreeQuery {
    worktree_ref: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorktreeSummariesQuery {
    worktree_refs: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorktreeDiffQuery {
    worktree_ref: Option<String>,
    file: Option<String>,
    area: Option<String>,
    cursor: Option<String>,
}

fn required(value: Option<String>, name: &str) -> Result<String, ApiError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("{name} is required"),
        )
    })
}

/// Running sessions with their agent phase, for "sessions working here".
async fn running_sessions(state: &AppState) -> Result<Vec<SessionSummary>, ApiError> {
    let mut sessions = session_report(state).await?.sessions;
    let home = state.home.clone();
    tokio::task::spawn_blocking(move || {
        git::refresh_directories(&home, &mut sessions);
        sessions
    })
    .await
    .map_err(|_| super::internal("read session directories"))
}

pub(super) async fn git_repositories(State(state): State<AppState>) -> Result<Response, ApiError> {
    let sessions = running_sessions(&state).await?;
    let list = git::repositories(&state.home, &sessions).await;
    bounded_json(StatusCode::OK, list)
}

pub(super) async fn git_resolve(Query(query): Query<FolderQuery>) -> Result<Response, ApiError> {
    let path = required(query.path, "path")?;
    let repository = git::resolve(&path).await.map_err(map_git_error)?;
    bounded_json(StatusCode::OK, repository)
}

pub(super) async fn git_status(
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<Response, ApiError> {
    let path = required(query.path, "path")?;
    let sessions = running_sessions(&state).await?;
    let status = git::status(&state.home, &path, &sessions)
        .await
        .map_err(map_git_error)?;
    bounded_json(StatusCode::OK, status)
}

pub(super) async fn git_diff(Query(query): Query<DiffQuery>) -> Result<Response, ApiError> {
    let path = required(query.path, "path")?;
    let file = required(query.file, "file")?;
    let area = diff_area(query.area)?;
    let diff = git::file_diff(&path, &file, area, query.cursor.as_deref())
        .await
        .map_err(map_git_error)?;
    bounded_json(StatusCode::OK, diff)
}

fn diff_area(area: Option<String>) -> Result<GitChangeArea, ApiError> {
    match required(area, "area")?.as_str() {
        "conflicted" => Ok(GitChangeArea::Conflicted),
        "staged" => Ok(GitChangeArea::Staged),
        "unstaged" => Ok(GitChangeArea::Unstaged),
        "untracked" => Ok(GitChangeArea::Untracked),
        _ => Err(invalid(
            "area must be conflicted, staged, unstaged, or untracked",
        )),
    }
}

fn paired_device(device: &DeviceContext) -> Result<&str, ApiError> {
    device
        .0
        .as_deref()
        .ok_or_else(|| map_git_error(GitError::WorktreeAccessDenied))
}

fn worktree_reference(value: Option<String>) -> Result<String, ApiError> {
    let reference = required(value, "worktreeRef")?;
    if !is_hex(&reference, 64..=64) {
        return Err(invalid("worktreeRef is malformed"));
    }
    Ok(reference)
}

fn scoped_json(value: impl serde::Serialize) -> Result<Response, ApiError> {
    let mut response = bounded_json(StatusCode::OK, value)?;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(super) async fn git_worktree_open(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<FolderQuery>,
) -> Result<Response, ApiError> {
    let path = required(query.path, "path")?;
    let opened = state
        .worktrees
        .open(&state.home, paired_device(&device)?, path)
        .await
        .map_err(map_git_error)?;
    scoped_json(opened)
}

pub(super) async fn git_worktree_status(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<WorktreeQuery>,
) -> Result<Response, ApiError> {
    let reference = worktree_reference(query.worktree_ref)?;
    let binding = state
        .worktrees
        .resolve(&state.home, paired_device(&device)?, &reference)
        .await
        .map_err(map_git_error)?;
    let sessions = running_sessions(&state).await?;
    let status = git::worktree_status(&binding.worktree, &sessions).await;
    binding.validate(&state.home).await.map_err(map_git_error)?;
    scoped_json(status.map_err(map_git_error)?)
}

pub(super) async fn git_worktree_summaries(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<WorktreeSummariesQuery>,
) -> Result<Response, ApiError> {
    use GitWorktreeSummaryError as Error;
    use GitWorktreeSummaryOutcome as Outcome;

    const MAX_REFERENCES: usize = 8;
    // The deadline includes both identity checks and the status scan.
    const ITEM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

    let device = paired_device(&device)?;
    let references = required(query.worktree_refs, "worktreeRefs")?;
    let references: Vec<_> = references.split(',').take(MAX_REFERENCES + 1).collect();
    if references.len() > MAX_REFERENCES {
        return Err(invalid("worktreeRefs accepts at most 8 references"));
    }
    let revision = git::control_revision(&state.home, device)
        .await
        .map_err(map_git_error)?;
    let mut results = vec![
        Outcome::Error {
            error: Error::InvalidRequest
        };
        references.len()
    ];
    // Keep one outcome per input position while checking and scanning each
    // distinct reference once. Different references can have different
    // bindings even when they point to the same checkout.
    let mut positions = HashMap::<_, Vec<_>>::new();
    for (index, reference) in references.into_iter().enumerate() {
        if is_hex(reference, 64..=64) {
            positions.entry(reference).or_default().push(index);
        }
    }
    let mut reads = tokio::task::JoinSet::new();
    for (reference, indices) in positions {
        let reference = reference.to_owned();
        let device = device.to_owned();
        let home = state.home.clone();
        let worktrees = state.worktrees.clone();
        reads.spawn(async move {
            let read = async {
                let binding = worktrees.resolve(&home, &device, &reference).await?;
                let summary = git::worktree_summary(&binding.worktree).await;
                binding.validate(&home).await?;
                summary
            };
            let result = match tokio::time::timeout(ITEM_DEADLINE, read).await {
                Ok(Ok(summary)) => Outcome::Ok { summary },
                result => {
                    let error = match result {
                        Err(_) | Ok(Err(GitError::TimedOut)) => Error::GitTimeout,
                        Ok(Err(GitError::StaleWorktreeReference)) => Error::StaleWorktreeRef,
                        Ok(Err(GitError::WorktreeAccessDenied)) => Error::WorktreeAccessDenied,
                        Ok(Err(GitError::GitUnavailable)) => Error::GitUnavailable,
                        _ => Error::GitFailed,
                    };
                    Outcome::Error { error }
                }
            };
            (indices, result)
        });
    }
    while let Some(result) = reads.join_next().await {
        let (indices, result) = result.map_err(|_| super::internal("read worktree summaries"))?;
        for index in indices {
            results[index] = result.clone();
        }
    }
    // A grant change while any row is in flight invalidates the entire response.
    if git::control_revision(&state.home, device)
        .await
        .map_err(map_git_error)?
        != revision
    {
        return Err(map_git_error(GitError::WorktreeAccessDenied));
    }
    scoped_json(GitWorktreeSummaries { results })
}

pub(super) async fn git_worktree_diff(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<WorktreeDiffQuery>,
) -> Result<Response, ApiError> {
    let reference = worktree_reference(query.worktree_ref)?;
    let file = required(query.file, "file")?;
    let area = diff_area(query.area)?;
    let binding = state
        .worktrees
        .resolve(&state.home, paired_device(&device)?, &reference)
        .await
        .map_err(map_git_error)?;
    let diff = git::scoped_diff(
        &binding.worktree,
        &reference,
        &file,
        area,
        query.cursor.as_deref(),
    )
    .await;
    binding.validate(&state.home).await.map_err(map_git_error)?;
    scoped_json(diff.map_err(map_git_error)?)
}

/// Stable codes and sentences the phone can show. None of them names a path
/// or echoes Git's own output, which can carry file names and config.
pub(super) fn map_git_error(error: GitError) -> ApiError {
    let (status, code, message) = match error {
        GitError::InvalidPath => (
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must be an accessible absolute directory path",
        ),
        GitError::UnavailablePath => (
            StatusCode::NOT_FOUND,
            "unavailable_path",
            "That folder is no longer on your Mac.",
        ),
        // The one 403 the phone does not read as a lost grant.
        GitError::UnreadableDirectory => (
            StatusCode::FORBIDDEN,
            "unreadable_directory",
            "directory cannot be read",
        ),
        GitError::NotARepository => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_a_repository",
            "That folder is not inside a Git working tree.",
        ),
        GitError::BareRepository => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "bare_repository",
            "That is a bare repository. It has no working tree to show.",
        ),
        GitError::UnsafeRepository => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsafe_repository",
            "Git on your Mac refuses this repository because another user owns it. Add it to safe.directory on the Mac to open it.",
        ),
        GitError::GitUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "git_unavailable",
            "Git is not installed on your Mac, or Latch cannot find it.",
        ),
        GitError::TimedOut => (
            StatusCode::GATEWAY_TIMEOUT,
            "git_timeout",
            "Git on your Mac did not answer in time.",
        ),
        GitError::Failed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "git_failed",
            "Git on your Mac could not read this repository.",
        ),
        GitError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the file or area is malformed",
        ),
        GitError::FileNotChanged => (
            StatusCode::NOT_FOUND,
            "file_not_changed",
            "That file no longer has changes there. Refresh the repository.",
        ),
        GitError::StaleCursor => (
            StatusCode::CONFLICT,
            "stale_cursor",
            "The file changed while you were reading it. Reload the diff.",
        ),
        GitError::WorktreeRootRequired => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "worktree_root_required",
            "Choose the root of the Git working tree.",
        ),
        GitError::StaleWorktreeReference => (
            StatusCode::CONFLICT,
            "stale_worktree_ref",
            "This worktree reference is no longer valid. Open the worktree again.",
        ),
        GitError::WorktreeAccessDenied => (
            StatusCode::FORBIDDEN,
            "worktree_access_denied",
            "An active paired device with Control access is required.",
        ),
    };
    ApiError::coded(status, code, message)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BranchQuery {
    path: Option<String>,
    worktree_ref: Option<String>,
}

fn preview_reference(
    path: &Option<String>,
    reference: Option<String>,
) -> Result<Option<String>, ApiError> {
    match (path, reference) {
        (Some(_), Some(_)) => Err(invalid("give path or worktreeRef, not both")),
        (None, Some(reference)) => worktree_reference(Some(reference)).map(Some),
        (_, None) => Ok(None),
    }
}

// A scoped page cursor is valid only with the reference that issued it.
fn preview_cursor<'a>(
    reference: Option<&str>,
    cursor: Option<&'a str>,
    maximum: usize,
) -> Result<Option<&'a str>, ApiError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let cursor = match reference {
        Some(reference) => {
            let (owner, cursor) = cursor
                .split_once(':')
                .ok_or_else(|| map_git_error(GitError::StaleCursor))?;
            if owner != reference {
                return Err(map_git_error(GitError::StaleCursor));
            }
            cursor
        }
        None => cursor,
    };
    if cursor.len() > maximum {
        return Err(invalid("cursor is malformed"));
    }
    Ok(Some(cursor))
}

pub(super) async fn git_branches(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<BranchQuery>,
) -> Result<Response, ApiError> {
    if let Some(reference) = preview_reference(&query.path, query.worktree_ref)? {
        let binding = state
            .worktrees
            .resolve(&state.home, paired_device(&device)?, &reference)
            .await
            .map_err(map_git_error)?;
        let list = git::worktree_branches(&binding.worktree).await;
        binding.validate(&state.home).await.map_err(map_git_error)?;
        return scoped_json(list.map_err(map_git_error)?);
    }
    let path = required(query.path, "path")?;
    let list = git::branches(&path).await.map_err(map_git_error)?;
    bounded_json(StatusCode::OK, list)
}

fn invalid(message: &str) -> ApiError {
    ApiError::coded(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        message.to_owned(),
    )
}

fn parse_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    if body.len() > MAX_MUTATION_BODY {
        return Err(ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "Git request is too large",
        ));
    }
    serde_json::from_slice(body).map_err(|_| invalid("Git request is malformed"))
}

fn is_hex(value: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_oid(value: &str) -> bool {
    is_hex(value, 40..=64)
}

fn mutation_target<'a>(state: &'a AppState, path: &'a str) -> Result<git::Target<'a>, ApiError> {
    match path.strip_prefix("worktree:") {
        Some(reference) if is_hex(reference, 64..=64) => {
            Ok(git::Target::Scoped(&state.worktrees, reference))
        }
        Some(_) => Err(invalid("worktreeRef is malformed")),
        None => Ok(git::Target::Path(path)),
    }
}

/// Shape checks shared by every mutation. Nothing here decides whether the
/// state still holds; the gateway rechecks that under the worktree lock.
fn common(request_id: &str, path: &str, expected: &GitPreconditions) -> Result<(), ApiError> {
    if !is_request_id(request_id) {
        return Err(invalid("requestId must be a UUID"));
    }
    if path.is_empty() || path.len() > crate::cli::serve::directory::MAX_PATH_BYTES {
        return Err(invalid("path is required"));
    }
    let head_ok = expected.head.as_deref().is_none_or(is_oid);
    let branch_ok = expected
        .branch
        .as_deref()
        .is_none_or(|branch| !branch.is_empty() && branch.len() <= 1024);
    if !head_ok
        || !branch_ok
        || !is_hex(&expected.index, 64..=64)
        || !is_hex(&expected.worktree, 64..=64)
    {
        return Err(invalid(
            "expected must be the preconditions status returned",
        ));
    }
    Ok(())
}

fn selection(request: &GitIndexRequest, staging: bool) -> Result<Selection, ApiError> {
    let allowed = |area: GitChangeArea| {
        if staging {
            matches!(
                area,
                GitChangeArea::Unstaged | GitChangeArea::Untracked | GitChangeArea::Conflicted
            )
        } else {
            area == GitChangeArea::Staged
        }
    };
    match (&request.files[..], request.all.as_deref()) {
        ([], Some(areas)) if !areas.is_empty() => {
            // Resolved conflicts are staged one reviewed file at a time.
            if areas
                .iter()
                .any(|area| !allowed(*area) || *area == GitChangeArea::Conflicted)
            {
                return Err(invalid(if staging {
                    "all may name unstaged and untracked"
                } else {
                    "all may name only staged"
                }));
            }
            let mut areas = areas.to_vec();
            areas.sort_by_key(|area| *area as u8);
            areas.dedup();
            Ok(Selection::All(areas))
        }
        (files, None | Some([])) if !files.is_empty() && files.len() <= MAX_FILES => {
            let mut targets = Vec::with_capacity(files.len());
            for file in files {
                let path = git::status_unhex(&file.path_key)
                    .filter(|path| git::is_relative_path(path))
                    .ok_or_else(|| invalid("pathKey must be a file status listed"))?;
                if !allowed(file.area) || !is_hex(&file.fingerprint, 16..=64) {
                    return Err(invalid(if staging {
                        "only unstaged, untracked or conflicted files can be staged"
                    } else {
                        "only staged files can be unstaged"
                    }));
                }
                targets.push(FileTarget {
                    path_key: git::status_hex(&path),
                    area: file.area,
                    fingerprint: file.fingerprint.clone(),
                });
            }
            Ok(Selection::Files(targets))
        }
        _ => Err(invalid("give either files (at most 200) or all")),
    }
}

/// The receipt, with 202 while it is still running.
fn submitted(result: Result<Submitted, MutationError>) -> Result<Response, ApiError> {
    match result {
        Ok(Submitted::Settled(receipt)) => bounded_json(StatusCode::OK, receipt),
        Ok(Submitted::Pending(receipt)) => bounded_json(StatusCode::ACCEPTED, receipt),
        Err(error) => Err(map_mutation_error(error)),
    }
}

fn map_mutation_error(error: MutationError) -> ApiError {
    match error {
        MutationError::Git(error) => map_git_error(error),
        MutationError::RequestIdConflict => ApiError::coded(
            StatusCode::CONFLICT,
            "request_id_conflict",
            "This request id was already used for a different Git action.",
        ),
        MutationError::Busy => ApiError::coded(
            StatusCode::CONFLICT,
            "worktree_busy",
            "Another change is running in this repository on your Mac. Check again in a moment.",
        ),
        MutationError::Storage => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "receipt_unavailable",
            "Latch could not read or record this request on your Mac.",
        ),
    }
}

async fn index_change(
    state: AppState,
    device: DeviceContext,
    body: axum::body::Bytes,
    staging: bool,
) -> Result<Response, ApiError> {
    let request: GitIndexRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    let selection = selection(&request, staging)?;
    if request.confirm_conflict_markers && !staging {
        return Err(invalid("confirmConflictMarkers applies only to staging"));
    }
    let mutation = if staging {
        Mutation::Stage {
            selection,
            confirm_conflict_markers: request.confirm_conflict_markers,
        }
    } else {
        Mutation::Unstage { selection }
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

pub(super) async fn git_stage(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    index_change(state, device, body, true).await
}

pub(super) async fn git_unstage(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    index_change(state, device, body, false).await
}

pub(super) async fn git_commit(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitCommitRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    let summary = request.summary.trim();
    if summary.is_empty()
        || summary.len() > MAX_SUMMARY_BYTES
        || summary.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
    {
        return Err(invalid("summary must be one non-empty line"));
    }
    let description = request.description.as_deref().map(str::trim_end);
    if description.is_some_and(|text| text.len() > MAX_DESCRIPTION_BYTES || text.contains('\0')) {
        return Err(invalid("description is too long"));
    }
    let mutation = Mutation::Commit {
        summary: summary.to_owned(),
        description: description
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

pub(super) async fn git_create_branch(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitCreateBranchRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if !git::is_plausible_branch_name(&request.name) {
        return Err(invalid("name is not a branch name Git allows"));
    }
    if request.start_point != "HEAD" && !git::is_plausible_branch_name(&request.start_point) {
        return Err(invalid("startPoint must be HEAD or a local branch"));
    }
    if !is_oid(&request.start_oid) {
        return Err(invalid("startOid must be a commit id"));
    }
    let mutation = Mutation::CreateBranch {
        name: request.name,
        start_point: request.start_point,
        start_oid: request.start_oid,
        switch: request.switch,
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

pub(super) async fn git_switch_branch(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitSwitchBranchRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if !git::is_plausible_branch_name(&request.name) {
        return Err(invalid("name must be a local branch"));
    }
    if !is_oid(&request.oid) {
        return Err(invalid("oid must be a commit id"));
    }
    let mutation = Mutation::SwitchBranch {
        name: request.name,
        oid: request.oid,
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

/// `GET /v2/git/receipts/{requestId}`: only the device that sent the
/// request can find it; any other device gets the same 404 as an unused id.
pub(super) async fn git_receipt(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Path(request_id): Path<String>,
    Query(query): Query<WorktreeQuery>,
) -> Result<Response, ApiError> {
    if !is_request_id(&request_id) {
        return Err(invalid("requestId must be a UUID"));
    }
    let reference = query
        .worktree_ref
        .map(|value| worktree_reference(Some(value)))
        .transpose()?;
    match git::receipt(
        &state.home,
        device.0.as_deref(),
        &request_id,
        reference.as_deref(),
    )
    .await
    {
        Ok(Some(receipt))
            if receipt.state == crate::cli::serve::contract::GitReceiptState::Pending =>
        {
            bounded_json(StatusCode::ACCEPTED, receipt)
        }
        Ok(Some(receipt)) => bounded_json(StatusCode::OK, receipt),
        Ok(None) => Err(ApiError::coded(
            StatusCode::NOT_FOUND,
            "receipt_not_found",
            "Your Mac has no record of that request.",
        )),
        Err(error) => Err(map_mutation_error(error)),
    }
}

pub(super) async fn git_sync(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<SyncQuery>,
) -> Result<Response, ApiError> {
    let reference = preview_reference(&query.path, query.worktree_ref)?;
    let cursor = preview_cursor(reference.as_deref(), query.cursor.as_deref(), 256)?;
    if let Some(reference) = reference {
        let binding = state
            .worktrees
            .resolve(&state.home, paired_device(&device)?, &reference)
            .await
            .map_err(map_git_error)?;
        let preview = git::worktree_sync(&binding.worktree, cursor).await;
        binding.validate(&state.home).await.map_err(map_git_error)?;
        let mut preview = preview.map_err(map_git_error)?;
        preview.next_cursor = preview
            .next_cursor
            .map(|cursor| format!("{reference}:{cursor}"));
        return scoped_json(preview);
    }
    let path = required(query.path, "path")?;
    let preview = git::sync(&path, cursor).await.map_err(map_git_error)?;
    bounded_json(StatusCode::OK, preview)
}

pub(super) async fn git_fetch(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitFetchRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if !git::is_plausible_remote_name(&request.remote) {
        return Err(invalid("remote must be a remote the sync preview listed"));
    }
    let mutation = Mutation::Fetch {
        remote: request.remote,
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

#[derive(Clone, Copy)]
enum Update {
    FastForward,
    Merge,
    Rebase,
}

async fn update(
    state: AppState,
    device: DeviceContext,
    body: axum::body::Bytes,
    kind: Update,
) -> Result<Response, ApiError> {
    let request: GitUpdateRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if request.upstream.is_empty()
        || request.upstream.len() > 1024
        || request.upstream.starts_with('-')
        || request.upstream.chars().any(char::is_control)
    {
        return Err(invalid(
            "upstream must be the upstream the sync preview showed",
        ));
    }
    if !is_oid(&request.upstream_oid) {
        return Err(invalid("upstreamOid must be a commit id"));
    }
    let (upstream, upstream_oid) = (request.upstream, request.upstream_oid);
    let mutation = match kind {
        Update::FastForward => Mutation::FastForward {
            upstream,
            upstream_oid,
        },
        Update::Merge => Mutation::Merge {
            upstream,
            upstream_oid,
        },
        Update::Rebase => Mutation::Rebase {
            upstream,
            upstream_oid,
        },
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

pub(super) async fn git_fast_forward(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    update(state, device, body, Update::FastForward).await
}

pub(super) async fn git_merge(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    update(state, device, body, Update::Merge).await
}

pub(super) async fn git_rebase(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    update(state, device, body, Update::Rebase).await
}

async fn control(
    state: AppState,
    device: DeviceContext,
    body: axum::body::Bytes,
    aborting: bool,
) -> Result<Response, ApiError> {
    let request: GitOperationControlRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if aborting && !request.confirmed {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "Aborting needs the user's explicit confirmation.",
        ));
    }
    let mutation = if aborting {
        Mutation::AbortOperation {
            operation: request.operation,
        }
    } else {
        Mutation::ContinueOperation {
            operation: request.operation,
        }
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

pub(super) async fn git_continue(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    control(state, device, body, false).await
}

pub(super) async fn git_abort(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    control(state, device, body, true).await
}

/// `GET /v2/git/push/preview`: what a push would send, as last fetched.
/// Never contacts the remote.
pub(super) async fn git_push_preview(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<PushQuery>,
) -> Result<Response, ApiError> {
    let reference = preview_reference(&query.path, query.worktree_ref)?;
    let cursor = preview_cursor(reference.as_deref(), query.cursor.as_deref(), 512)?;
    let chosen = match (query.remote.as_deref(), query.destination.as_deref()) {
        (None, None) => None,
        (Some(remote), Some(destination)) => {
            if !git::is_plausible_remote_name(remote) {
                return Err(invalid("remote must be a remote the preview listed"));
            }
            if !git::is_plausible_destination(destination) {
                return Err(invalid("destination is not a branch name Git allows"));
            }
            Some((remote, destination))
        }
        _ => return Err(invalid("give remote and destination together")),
    };
    if let Some(reference) = reference {
        let binding = state
            .worktrees
            .resolve(&state.home, paired_device(&device)?, &reference)
            .await
            .map_err(map_git_error)?;
        let preview = git::worktree_push(&binding.worktree, chosen, cursor).await;
        binding.validate(&state.home).await.map_err(map_git_error)?;
        let mut preview = preview.map_err(map_git_error)?;
        preview.next_cursor = preview
            .next_cursor
            .map(|cursor| format!("{reference}:{cursor}"));
        return scoped_json(preview);
    }
    let path = required(query.path, "path")?;
    let preview = git::push_preview(&path, chosen, cursor)
        .await
        .map_err(map_git_error)?;
    bounded_json(StatusCode::OK, preview)
}

pub(super) async fn git_push(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitPushRequest = parse_body(&body)?;
    common(&request.request_id, &request.path, &request.expected)?;
    if !git::is_plausible_remote_name(&request.remote) {
        return Err(invalid("remote must be a remote the push preview listed"));
    }
    if !git::is_plausible_destination(&request.destination) {
        return Err(invalid("destination is not a branch name Git allows"));
    }
    if !git::is_plausible_branch_name(&request.source_branch) {
        return Err(invalid(
            "sourceBranch must be the branch the preview showed",
        ));
    }
    if !is_oid(&request.source_oid) || !request.destination_oid.as_deref().is_none_or(is_oid) {
        return Err(invalid("sourceOid and destinationOid must be commit ids"));
    }
    if request
        .remote_url
        .as_deref()
        .is_some_and(|url| url.len() > 4096 || url.chars().any(char::is_control))
    {
        return Err(invalid("remoteUrl must be the address the preview showed"));
    }
    let mutation = Mutation::Push {
        remote: request.remote,
        remote_url: request.remote_url,
        destination: request.destination,
        source_branch: request.source_branch,
        source_oid: request.source_oid,
        destination_oid: request.destination_oid,
    };
    submitted(
        git::submit(
            &state.home,
            device.0,
            &request.request_id,
            mutation_target(&state, &request.path)?,
            request.expected,
            mutation,
        )
        .await,
    )
}

/// `POST /v2/git/push/reconcile`: only the device that sent the push can
/// check it; any other device gets the same 404 as an unused id.
pub(super) async fn git_push_reconcile(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<WorktreeQuery>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: GitPushReconcileRequest = parse_body(&body)?;
    if !is_request_id(&request.request_id) {
        return Err(invalid("requestId must be a UUID"));
    }
    let reference = query
        .worktree_ref
        .map(|value| worktree_reference(Some(value)))
        .transpose()?;
    match git::check_push(
        &state.home,
        device.0.as_deref(),
        &request.request_id,
        reference.as_deref(),
    )
    .await
    {
        Ok(check) => bounded_json(
            StatusCode::OK,
            GitPushReconciliation {
                receipt: check.receipt,
                remote_checked: check.remote_checked,
                remote_oid: check.remote_oid,
                message: check.message,
            },
        ),
        Err(git::PushCheckError::NotFound) => Err(ApiError::coded(
            StatusCode::NOT_FOUND,
            "receipt_not_found",
            "Your Mac has no record of that request.",
        )),
        Err(git::PushCheckError::NotAPush) => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "not_a_push",
            "That request was not a push.",
        )),
        Err(git::PushCheckError::StillRunning) => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "still_running",
            "Git is still running this push on your Mac. Keep waiting for its receipt.",
        )),
        Err(git::PushCheckError::Busy) => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "worktree_busy",
            "Another change is running in this repository on your Mac. Check again in a moment.",
        )),
        Err(git::PushCheckError::Scope(error)) => Err(map_mutation_error(error)),
        Err(git::PushCheckError::Storage) => Err(map_mutation_error(MutationError::Storage)),
    }
}
