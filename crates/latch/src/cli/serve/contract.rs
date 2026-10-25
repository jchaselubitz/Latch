//! Generated from `schemas/remote-access/v2/*.schema.json`; do not edit by hand.
//! Canonical schema set SHA-256: 9dd8707da286f9e25088b8895b680f697bef1cfb65cee42fe9136cb93485d760

use serde::{Deserialize, Serialize};

pub const REMOTE_ACCESS_SCHEMA_VERSION: u8 = 2;
pub const OPERATION_RETENTION_SECONDS: u64 = 10 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub request_id: String,
    pub cwd: String,
    /// Hosted agent to launch directly in the session instead of a standard
    /// shell. Absent means a shell. The gateway accepts only a kind it
    /// advertises in `features.session_agents`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SessionAgent>,
    /// Model the agent starts with, passed to it as `--model`. Valid only
    /// with `agent`, and only an id the gateway lists for that agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Hosted agent kinds a caller may name at creation. The Mac resolves the
/// executable and launch arguments; the caller names only the kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionAgent {
    /// Claude Code, launched as its own executable so the observer plugin
    /// and conversation connector attach.
    Claude,
    /// OpenAI Codex CLI, launched with its conversation connector identity.
    Codex,
}

impl SessionAgent {
    /// The harness marker Latch records for sessions running this agent.
    pub const fn harness(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// One model a hosted agent can start with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentModel {
    /// What the agent accepts as `--model`.
    pub id: String,
    /// The agent's own display name for it.
    pub name: String,
    /// The agent's one-line description, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Models one hosted agent can start with, read fresh from the agent's own
/// model cache on the Mac, with a list bundled with Latch as the fallback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelCatalog {
    /// The agent these models start.
    pub agent: SessionAgent,
    /// Newest and most recommended first, in the agent's own order.
    pub models: Vec<AgentModel>,
    /// The owner's configured default, which may be an alias outside `models`.
    pub default_model: Option<String>,
}

/// What kind of working tree a resolved folder belongs to. Bare repositories
/// have no working tree and are refused rather than reported.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeKind {
    Repository,
    LinkedWorktree,
    Submodule,
}

/// `GET /v2/git/resolve`: a folder resolved to its working tree root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRepository {
    pub root: String,
    pub name: String,
    pub kind: GitWorktreeKind,
}

/// A running Latch session whose folder resolves to a working tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitSessionRef {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub command_label: String,
    pub connector: Option<String>,
    pub agent_phase: Option<String>,
    pub idle_ms: Option<u64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitRepositorySummary {
    pub root: String,
    pub name: String,
    pub kind: GitWorktreeKind,
    pub sessions: Vec<GitSessionRef>,
    pub last_opened_at: Option<String>,
}

/// `GET /v2/git/repositories`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitRepositoryList {
    pub from_sessions: Vec<GitRepositorySummary>,
    pub recent: Vec<GitRepositorySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHead {
    pub branch: Option<String>,
    pub oid: Option<String>,
    pub detached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitUpstream {
    pub name: String,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitOperationKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitOperation {
    pub kind: GitOperationKind,
    pub step: Option<u32>,
    pub total: Option<u32>,
    /// Rebase: the branch being rebased, without `refs/heads/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_name: Option<String>,
    /// Rebase: the commit being rebased onto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onto: Option<String>,
    /// Rebase: the commit Git stopped at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    /// Merge: the commit being merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_head: Option<String>,
    /// Merge: the first line of the message Git prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeArea {
    Conflicted,
    Staged,
    Unstaged,
    Untracked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

/// One changed path in one area. A file changed both in the index and in
/// the working tree is two entries, one per area.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusEntry {
    pub path: String,
    pub path_key: String,
    pub orig_path: Option<String>,
    pub orig_path_key: Option<String>,
    pub area: GitChangeArea,
    pub change: GitChangeKind,
    pub conflict: Option<String>,
    pub submodule: bool,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
    /// Digest of the entry as the host saw it; a selected-file mutation
    /// sends it back and is refused if the file changed since.
    pub fingerprint: String,
}

/// The state a mutation was reviewed against. The phone copies it from the
/// status or branch list it showed; each operation rechecks the parts that
/// matter to it immediately before acting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitPreconditions {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub index: String,
    pub worktree: String,
}

/// `GET /v2/git/status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub root: String,
    pub name: String,
    pub kind: GitWorktreeKind,
    pub head: GitHead,
    pub upstream: Option<GitUpstream>,
    pub last_fetched_at: Option<String>,
    pub operation: Option<GitOperation>,
    pub entries: Vec<GitStatusEntry>,
    pub entries_truncated: bool,
    pub total_entries: u64,
    pub sessions: Vec<GitSessionRef>,
    pub observed_at: String,
    pub preconditions: GitPreconditions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffKind {
    Text,
    Binary,
    TooLarge,
    Submodule,
    Directory,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffLineKind {
    Context,
    Added,
    Removed,
    Note,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffLine {
    pub kind: GitDiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffHunk {
    pub index: u32,
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<GitDiffLine>,
    pub omitted_lines: u32,
}

/// `GET /v2/git/diff`: one bounded page of one file's diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub root: String,
    pub path: String,
    pub path_key: String,
    pub orig_path: Option<String>,
    pub area: GitChangeArea,
    pub kind: GitDiffKind,
    pub additions: u64,
    pub deletions: u64,
    pub hunks: Vec<GitDiffHunk>,
    pub total_hunks: u32,
    pub next_cursor: Option<String>,
    pub notes: Vec<String>,
}

/// One local branch in `GET /v2/git/branches`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranch {
    pub name: String,
    pub oid: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub upstream_gone: bool,
    pub subject: String,
    pub committed_at: Option<String>,
    pub checked_out_elsewhere: bool,
}

/// `GET /v2/git/branches`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchList {
    pub root: String,
    pub head: GitHead,
    pub branches: Vec<GitBranch>,
    pub truncated: bool,
    pub preconditions: GitPreconditions,
}

/// A whole file named by a stage or unstage request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitFileRef {
    pub path_key: String,
    pub area: GitChangeArea,
    pub fingerprint: String,
}

/// `POST /v2/git/stage` and `POST /v2/git/unstage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitIndexRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    #[serde(default)]
    pub files: Vec<GitFileRef>,
    #[serde(default)]
    pub all: Option<Vec<GitChangeArea>>,
    /// Stage conflicted files even if they still contain conflict markers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm_conflict_markers: bool,
}

/// `POST /v2/git/commit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCommitRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `POST /v2/git/branches/create`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCreateBranchRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub name: String,
    pub start_point: String,
    pub start_oid: String,
    pub switch: bool,
}

/// `POST /v2/git/branches/switch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitSwitchBranchRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub name: String,
    pub oid: String,
}

/// `POST /v2/git/fetch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitFetchRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub remote: String,
}

/// `POST /v2/git/fast-forward`, `/v2/git/merge` and `/v2/git/rebase`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitUpdateRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub upstream: String,
    pub upstream_oid: String,
}

/// `POST /v2/git/push`: the source, remote and destination exactly as the
/// push review showed them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitPushRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub remote: String,
    pub remote_url: Option<String>,
    pub destination: String,
    pub source_branch: String,
    pub source_oid: String,
    pub destination_oid: Option<String>,
}

/// `POST /v2/git/push/reconcile`: an earlier push's request id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitPushReconcileRequest {
    pub request_id: String,
}

/// The operations Continue and Abort act on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitControlledOperation {
    Merge,
    Rebase,
}

/// `POST /v2/git/continue` and `POST /v2/git/abort`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitOperationControlRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitPreconditions,
    pub operation: GitControlledOperation,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitMutationKind {
    Stage,
    Unstage,
    Commit,
    CreateBranch,
    SwitchBranch,
    Fetch,
    FastForward,
    Merge,
    Rebase,
    ContinueOperation,
    AbortOperation,
    Push,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitReceiptState {
    Pending,
    Completed,
    Rejected,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationOutcome {
    pub code: String,
    pub message: String,
    pub git_output: Option<String>,
    pub git_output_truncated: bool,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub files: Vec<String>,
    pub reconciled: bool,
}

/// Git's latest progress line for a pending request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitProgress {
    pub message: String,
    pub percent: Option<u8>,
    pub updated_at: String,
}

/// The host's durable, device-scoped record of one Git mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationReceipt {
    pub request_id: String,
    pub operation: GitMutationKind,
    pub root: String,
    pub state: GitReceiptState,
    pub accepted_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<GitMutationOutcome>,
    /// When Git was started, after the recheck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Git's latest progress while pending, from this gateway process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<GitProgress>,
}

/// The upstream a sync is reviewed against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitSyncUpstream {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub remote: Option<String>,
    pub remote_url: Option<String>,
    pub oid: Option<String>,
    pub gone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitSummary {
    pub oid: String,
    pub subject: String,
    pub author: String,
    pub committed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncRelation {
    UpToDate,
    Behind,
    Ahead,
    Diverged,
    NoUpstream,
    UpstreamGone,
    Detached,
    Unborn,
}

/// `GET /v2/git/sync`: the upstream as last fetched; never fetches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitSyncPreview {
    pub root: String,
    pub head: GitHead,
    pub upstream: Option<GitSyncUpstream>,
    pub remotes: Vec<GitRemote>,
    pub last_fetched_at: Option<String>,
    pub relation: GitSyncRelation,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub fast_forward_blocked_reason: Option<String>,
    pub incoming: Vec<GitCommitSummary>,
    pub next_cursor: Option<String>,
    pub operation: Option<GitOperation>,
    pub preconditions: GitPreconditions,
}

/// The local branch a push sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitPushSource {
    pub branch: String,
    pub oid: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitPushSelectedBy {
    Upstream,
    Request,
}

/// The remote branch a push is reviewed against, as last fetched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPushTarget {
    pub remote: String,
    pub remote_url: Option<String>,
    pub destination: String,
    pub destination_ref: String,
    pub tracking_ref: Option<String>,
    pub destination_oid: Option<String>,
    pub selected_by: GitPushSelectedBy,
    pub new_branch: bool,
    pub is_default_branch: bool,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitPushSuggestion {
    pub remote: String,
    pub destination: String,
}

/// `GET /v2/git/push/preview`: what a push would send; never contacts the
/// remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPushPreview {
    pub root: String,
    pub head: GitHead,
    pub source: Option<GitPushSource>,
    pub remotes: Vec<GitRemote>,
    pub target: Option<GitPushTarget>,
    pub selection_required: bool,
    pub selection_reason: Option<String>,
    pub suggestion: Option<GitPushSuggestion>,
    pub last_fetched_at: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub blocked_reason: Option<String>,
    pub rejection_likely_reason: Option<String>,
    pub outgoing: Vec<GitCommitSummary>,
    pub next_cursor: Option<String>,
    pub operation: Option<GitOperation>,
    pub preconditions: GitPreconditions,
}

/// `POST /v2/git/push/reconcile`: the receipt after asking the remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPushReconciliation {
    pub receipt: GitMutationReceipt,
    pub remote_checked: bool,
    pub remote_oid: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFeatures {
    /// Always true: a terminal connection is the session's one exclusive
    /// surface. The field remains so a client can detect a gateway that
    /// predates the exclusive cutover and refuse it.
    pub exclusive_terminal: bool,
    /// Hosted agent kinds the create route accepts. A gateway that predates
    /// agent creation omits the key, which a client reads as shells only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_agents: Vec<SessionAgent>,
    /// Largest body the attachments route accepts. Present exactly when the
    /// gateway serves that route; an older gateway omits the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_message: Option<GitDraftReadiness>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayReadiness {
    pub format_version: u8,
    pub address: String,
    pub url: String,
    pub protocol_version: u32,
    pub gateway_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// `Partial` is reserved in v2. Clients render unknown future statuses as complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Submitted,
    Queued,
    Observed,
    Partial,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Permission,
    Question,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Resolved,
    Dismissed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItemKind {
    Message {
        role: MessageRole,
        text: String,
        status: MessageStatus,
    },
    Tool {
        name: String,
        summary: String,
        status: ToolStatus,
        #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
    },
    Request {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "requestType")]
        request_type: RequestType,
        prompt: String,
        choices: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        questions: Vec<RequestQuestion>,
        status: RequestStatus,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationItem {
    pub id: String,
    /// Hub-assigned observation order. Clients never sort by `created_at`.
    pub ordinal: u64,
    /// Display metadata only.
    pub created_at: String,
    pub kind: ConversationItemKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPhase {
    Starting,
    Idle,
    Working,
    AwaitingInput,
    Exited,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationAvailability {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn unavailable_cancel_turn() -> OperationAvailability {
    OperationAvailability {
        enabled: false,
        reason: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorIdentity {
    pub id: String,
    pub version: String,
}

/// Why the agent closed its newest turn: its own answer, an interruption, a
/// refusal, or an error.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Answer,
    Aborted,
    Refusal,
    Error,
}

/// A slash command the agent advertises for its composer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvertisedCommand {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    pub phase: ConversationPhase,
    pub send_message: OperationAvailability,
    pub resolve_request: OperationAvailability,
    #[serde(default = "unavailable_cancel_turn")]
    pub cancel_turn: OperationAvailability,
    /// The agent's reason for closing its newest turn, while no turn is open.
    /// Absent when the connector has no outcome to report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_outcome: Option<TurnOutcome>,
    /// Slash commands the agent advertises while a live bridge reports them.
    /// Absent means unknown, not none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<AdvertisedCommand>>,
    /// Derived from the newest request item whose status is pending.
    pub pending_request: Option<String>,
    pub connector: Option<ConnectorIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    Initial,
    Generation,
    OperationEpoch,
    Overflow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationResultStatus {
    Accepted,
    Queued,
    Refused,
    Ambiguous,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationServerMessage {
    Snapshot {
        generation: String,
        revision: u64,
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        items: Vec<ConversationItem>,
        state: ConversationState,
        #[serde(rename = "hasMoreBefore")]
        has_more_before: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SnapshotReason>,
    },
    ItemsUpserted {
        generation: String,
        revision: u64,
        items: Vec<ConversationItem>,
    },
    ItemsRemoved {
        generation: String,
        revision: u64,
        #[serde(rename = "itemIds")]
        item_ids: Vec<String>,
    },
    StateChanged {
        generation: String,
        revision: u64,
        state: ConversationState,
    },
    OperationResult {
        #[serde(rename = "operationId")]
        operation_id: String,
        status: OperationResultStatus,
        #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    HistoryPage {
        #[serde(rename = "requestId")]
        request_id: String,
        items: Vec<ConversationItem>,
        #[serde(rename = "hasMoreBefore")]
        has_more_before: bool,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationClientMessage {
    Resume {
        #[serde(skip_serializing_if = "Option::is_none")]
        generation: Option<String>,
        #[serde(rename = "afterRevision", skip_serializing_if = "Option::is_none")]
        after_revision: Option<u64>,
    },
    SendMessage {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        text: String,
    },
    CancelTurn {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
    },
    ResolveRequest {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<std::collections::BTreeMap<String, String>>,
    },
    HistoryRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "beforeOrdinal")]
        before_ordinal: u64,
        limit: u16,
    },
    OperationStatus {
        #[serde(rename = "operationId")]
        operation_id: String,
    },
}

/// Text frame sent once the terminal surface is held by this socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachedFrame {
    pub r#type: String,
    pub resume_capability: String,
    pub resume_window_seconds: u64,
}

// Cloud draft contracts; capability advertisement determines availability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitDraftExpected {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub index: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCommitMessageRequest {
    pub request_id: String,
    pub path: String,
    pub expected: GitDraftExpected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitDraftSource {
    pub root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub index: String,
    pub captured_index: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDraftOmissionReason {
    SensitivePath,
    Credential,
    Binary,
    PatchLimit,
    InvalidText,
    Submodule,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitDraftOmission {
    pub path: Option<String>,
    pub reason: GitDraftOmissionReason,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitDraftCoverage {
    pub total_files: u64,
    pub included_files: u64,
    pub patch_files: u64,
    pub patch_bytes: u64,
    pub omissions: Vec<GitDraftOmission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCommitMessageResponse {
    pub request_id: String,
    pub summary: String,
    pub body: Option<String>,
    pub source: GitDraftSource,
    pub coverage: GitDraftCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDraftFailureCode {
    Unsupported,
    Unconfigured,
    InvalidRequest,
    Unauthorized,
    NoStagedChanges,
    StaleSource,
    UnusableContext,
    ContextTooLarge,
    GitUnavailable,
    ProviderRefusal,
    ProviderError,
    RateLimited,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCommitMessageFailure {
    pub request_id: Option<String>,
    pub code: GitDraftFailureCode,
    pub message: String,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDraftReadinessState {
    Unconfigured,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitDraftReadiness {
    pub state: GitDraftReadinessState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCommitMessageCancelRequest {
    pub request_id: String,
}
