#!/usr/bin/env python3
"""Generate Rust types and terminal TypeScript types from canonical v2 schemas.

The deliberately small generator keeps the generated representations reviewable
while making the JSON Schema documents the source of truth for protocol major 2.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCHEMAS = ROOT / "schemas/remote-access/v2"
RUST = ROOT / "crates/latch/src/cli/serve/contract.rs"
TYPESCRIPT = ROOT / "packages/client/src/generated.ts"
SCHEMA_NAMES = (
    "create-session-request.schema.json",
    "create-session-response.schema.json",
    "agent-model-catalog.schema.json",
    "conversation-item.schema.json",
    "conversation-state.schema.json",
    "conversation-protocol.schema.json",
    "directory-page.schema.json",
    "gateway-capabilities.schema.json",
    "gateway-readiness.schema.json",
    "terminal-connection.schema.json",
    "git-repository.schema.json",
    "git-repository-list.schema.json",
    "git-status.schema.json",
    "git-file-diff.schema.json",
    "git-branch-list.schema.json",
    "git-mutation-request.schema.json",
    "git-mutation-receipt.schema.json",
    "git-sync.schema.json",
    "git-push.schema.json",
)


def load(name: str) -> dict:
    document = json.loads((SCHEMAS / name).read_text())
    expected = f"https://latch.cooperativ.dev/schemas/remote-access/v2/{name}"
    if document.get("$id") != expected:
        raise ValueError(f"{name} must use canonical id {expected}")
    return document


def contract_digest() -> str:
    payload = "".join((SCHEMAS / name).read_text() for name in SCHEMA_NAMES)
    return hashlib.sha256(payload.encode()).hexdigest()


def rust_source(schema_digest: str) -> str:
    source = f'''//! Generated from `schemas/remote-access/v2/*.schema.json`; do not edit by hand.
//! Canonical schema set SHA-256: {schema_digest}
''' + r'''

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
pub enum MessageRole { User, Assistant }

/// `Partial` is reserved in v2. Clients render unknown future statuses as complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus { Submitted, Queued, Observed, Partial, Complete, Failed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus { Running, Succeeded, Failed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestType { Permission, Question }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus { Pending, Resolved, Dismissed }

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
    Message { role: MessageRole, text: String, status: MessageStatus },
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
pub enum ConversationPhase { Starting, Idle, Working, AwaitingInput, Exited, Unavailable }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationAvailability {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn unavailable_cancel_turn() -> OperationAvailability { OperationAvailability { enabled: false, reason: None } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorIdentity { pub id: String, pub version: String }

/// Why the agent closed its newest turn: its own answer, an interruption, a
/// refusal, or an error.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome { Answer, Aborted, Refusal, Error }

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
pub enum SnapshotReason { Initial, Generation, OperationEpoch, Overflow }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationResultStatus { Accepted, Queued, Refused, Ambiguous, Unknown }

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
    ItemsUpserted { generation: String, revision: u64, items: Vec<ConversationItem> },
    ItemsRemoved {
        generation: String,
        revision: u64,
        #[serde(rename = "itemIds")]
        item_ids: Vec<String>,
    },
    StateChanged { generation: String, revision: u64, state: ConversationState },
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
    Error { code: String, message: String },
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
'''
    return subprocess.run(
        ["rustfmt", "--emit", "stdout"],
        input=source,
        text=True,
        check=True,
        capture_output=True,
    ).stdout


def typescript_endpoints(capabilities: dict) -> str:
    """Emit `GatewayEndpoints` from the discovery schema rather than by hand.

    A key the schema lists as required is always present; every other route is
    optional because a gateway that predates it omits the key, and a client must
    read that omission as unavailable rather than as false-by-default.
    """
    endpoints = capabilities["properties"]["endpoints"]
    required = set(endpoints["required"])
    names = list(endpoints["properties"])
    fields = "".join(
        f"  {name}{'' if name in required else '?'}: boolean;\n" for name in names
    )
    union = " | ".join(f"'{name}'" for name in names)
    return (
        "/** Routes the gateway serves. An absent key means the gateway predates\n"
        " * that route: treat it as unavailable, never as false-by-default. */\n"
        f"export type GatewayEndpoints = {{\n{fields}}};\n"
        f"export type GatewayEndpointName = {union};\n"
    )


def typescript_source(schema_digest: str, capabilities: dict) -> str:
    return f'''// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: {schema_digest}
''' + '''

export type TerminalCloseReason =
  | 'detached'
  | 'stolen'
  | 'slow_client'
  | 'session_exited'
  | 'kernel_error'
  | 'resume_refused';
export const TERMINAL_CLOSE_CODES = {
  detached: 1000,
  slow_client: 4408,
  stolen: 4409,
  session_exited: 4410,
  kernel_error: 4500,
  resume_refused: 4411
} as const satisfies Record<TerminalCloseReason, number>;
export type SessionAgent = 'claude' | 'codex';
export type CreateSessionRequest = {
  requestId: string;
  cwd: string;
  agent?: SessionAgent;
  /** Only with `agent`, and only an id the gateway lists for it. */
  model?: string;
};
export type AgentModel = { id: string; name: string; description?: string };
/** `GET /v2/agents/{agent}/models`, served when `endpoints.agentModels` is true. */
export type AgentModelCatalog = {
  agent: SessionAgent;
  models: AgentModel[];
  defaultModel: string | null;
};
/** `sessionAgents` is absent on a gateway that predates agent creation; read
 * that as shells only. `attachmentMaxBytes` is present exactly when the
 * gateway serves the attachments route. */
export type GatewayFeatures = {
  exclusiveTerminal: boolean;
  sessionAgents?: SessionAgent[];
  attachmentMaxBytes?: number;
};
/** Git routes (all at the control grant), served when the matching
 * `endpoints.git*` key is true. Paths are display text; `pathKey` is the
 * hex-encoded exact bytes the diff route takes. */
export type GitWorktreeKind = 'repository' | 'linked_worktree' | 'submodule';
export type GitRepository = { root: string; name: string; kind: GitWorktreeKind };
export type GitSessionRef = {
  id: string;
  name: string;
  title: string | null;
  commandLabel: string;
  connector: string | null;
  agentPhase: string | null;
  idleMs: number | null;
  active: boolean;
};
export type GitRepositorySummary = GitRepository & {
  sessions: GitSessionRef[];
  lastOpenedAt: string | null;
};
export type GitRepositoryList = {
  fromSessions: GitRepositorySummary[];
  recent: GitRepositorySummary[];
};
export type GitChangeArea = 'conflicted' | 'staged' | 'unstaged' | 'untracked';
export type GitStatusEntry = {
  path: string;
  pathKey: string;
  origPath: string | null;
  origPathKey: string | null;
  area: GitChangeArea;
  change:
    | 'modified'
    | 'added'
    | 'deleted'
    | 'renamed'
    | 'copied'
    | 'type_changed'
    | 'untracked'
    | 'conflicted';
  conflict: string | null;
  submodule: boolean;
  additions: number | null;
  deletions: number | null;
  binary: boolean;
  fingerprint: string;
};
/** Sent back unchanged with every mutation; the Mac rechecks it. */
export type GitPreconditions = {
  branch: string | null;
  head: string | null;
  index: string;
  worktree: string;
};
/** The merge, rebase, cherry-pick or revert in progress. */
export type GitOperation = {
  kind: 'merge' | 'rebase' | 'cherry_pick' | 'revert';
  step: number | null;
  total: number | null;
  headName?: string | null;
  onto?: string | null;
  stoppedAt?: string | null;
  mergeHead?: string | null;
  summary?: string | null;
};
export type GitStatus = GitRepository & {
  head: { branch: string | null; oid: string | null; detached: boolean };
  upstream: { name: string; ahead: number | null; behind: number | null } | null;
  lastFetchedAt: string | null;
  operation: GitOperation | null;
  entries: GitStatusEntry[];
  entriesTruncated: boolean;
  totalEntries: number;
  sessions: GitSessionRef[];
  observedAt: string;
  preconditions: GitPreconditions;
};
export type GitDiffLine = {
  kind: 'context' | 'added' | 'removed' | 'note';
  oldLine: number | null;
  newLine: number | null;
  text: string;
  truncated?: boolean;
};
export type GitDiffHunk = {
  index: number;
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: GitDiffLine[];
  omittedLines: number;
};
export type GitFileDiff = {
  root: string;
  path: string;
  pathKey: string;
  origPath: string | null;
  area: GitChangeArea;
  kind: 'text' | 'binary' | 'too_large' | 'submodule' | 'directory';
  additions: number;
  deletions: number;
  hunks: GitDiffHunk[];
  totalHunks: number;
  nextCursor: string | null;
  notes: string[];
};
export type GitBranch = {
  name: string;
  oid: string;
  current: boolean;
  upstream: string | null;
  ahead: number | null;
  behind: number | null;
  upstreamGone: boolean;
  subject: string;
  committedAt: string | null;
  checkedOutElsewhere: boolean;
};
export type GitBranchList = {
  root: string;
  head: { branch: string | null; oid: string | null; detached: boolean };
  branches: GitBranch[];
  truncated: boolean;
  preconditions: GitPreconditions;
};
/** Git mutations (POST, control grant). `requestId` is a UUID scoped to the
 * device; the answer is always a receipt. */
export type GitFileRef = {
  pathKey: string;
  area: 'conflicted' | 'staged' | 'unstaged' | 'untracked';
  fingerprint: string;
};
type GitMutationBase = { requestId: string; path: string; expected: GitPreconditions };
export type GitIndexRequest = GitMutationBase & {
  files?: GitFileRef[];
  all?: Array<'staged' | 'unstaged' | 'untracked'> | null;
  confirmConflictMarkers?: boolean;
};
export type GitCommitRequest = GitMutationBase & { summary: string; description?: string | null };
export type GitCreateBranchRequest = GitMutationBase & {
  name: string;
  startPoint: string;
  startOid: string;
  switch: boolean;
};
export type GitSwitchBranchRequest = GitMutationBase & { name: string; oid: string };
export type GitFetchRequest = GitMutationBase & { remote: string };
export type GitUpdateRequest = GitMutationBase & { upstream: string; upstreamOid: string };
export type GitPushRequest = GitMutationBase & {
  remote: string;
  remoteUrl: string | null;
  destination: string;
  sourceBranch: string;
  sourceOid: string;
  destinationOid: string | null;
};
export type GitPushReconcileRequest = { requestId: string };
export type GitOperationControlRequest = GitMutationBase & {
  operation: 'merge' | 'rebase';
  confirmed?: boolean;
};
export type GitMutationOutcome = {
  code: string;
  message: string;
  gitOutput: string | null;
  gitOutputTruncated: boolean;
  head: string | null;
  branch: string | null;
  commit: string | null;
  files: string[];
  reconciled: boolean;
};
export type GitMutationReceipt = {
  requestId: string;
  operation:
    | 'stage'
    | 'unstage'
    | 'commit'
    | 'create_branch'
    | 'switch_branch'
    | 'fetch'
    | 'fast_forward'
    | 'merge'
    | 'rebase'
    | 'continue_operation'
    | 'abort_operation'
    | 'push';
  root: string;
  state: 'pending' | 'completed' | 'rejected' | 'uncertain';
  acceptedAt: string;
  finishedAt: string | null;
  outcome: GitMutationOutcome | null;
  startedAt?: string | null;
  progress?: { message: string; percent: number | null; updatedAt: string } | null;
};
export type GitCommitSummary = {
  oid: string;
  subject: string;
  author: string;
  committedAt: string | null;
};
/** `GET /v2/git/sync`: the upstream as last fetched. Never fetches. */
export type GitSyncPreview = {
  root: string;
  head: { branch: string | null; oid: string | null; detached: boolean };
  upstream: {
    name: string;
    ref: string;
    remote: string | null;
    remoteUrl: string | null;
    oid: string | null;
    gone: boolean;
  } | null;
  remotes: Array<{ name: string; url: string | null }>;
  lastFetchedAt: string | null;
  relation:
    | 'up_to_date'
    | 'behind'
    | 'ahead'
    | 'diverged'
    | 'no_upstream'
    | 'upstream_gone'
    | 'detached'
    | 'unborn';
  ahead: number | null;
  behind: number | null;
  fastForwardBlockedReason: string | null;
  incoming: GitCommitSummary[];
  nextCursor: string | null;
  operation: GitOperation | null;
  preconditions: GitPreconditions;
};
export type GitPushTarget = {
  remote: string;
  remoteUrl: string | null;
  destination: string;
  destinationRef: string;
  trackingRef: string | null;
  destinationOid: string | null;
  selectedBy: 'upstream' | 'request';
  newBranch: boolean;
  isDefaultBranch: boolean;
  defaultBranch: string | null;
};
/** `GET /v2/git/push/preview`: what a push would send. Never contacts the remote. */
export type GitPushPreview = {
  root: string;
  head: { branch: string | null; oid: string | null; detached: boolean };
  source: { branch: string; oid: string } | null;
  remotes: Array<{ name: string; url: string | null }>;
  target: GitPushTarget | null;
  selectionRequired: boolean;
  selectionReason: string | null;
  suggestion: { remote: string; destination: string } | null;
  lastFetchedAt: string | null;
  ahead: number | null;
  behind: number | null;
  blockedReason: string | null;
  rejectionLikelyReason: string | null;
  outgoing: GitCommitSummary[];
  nextCursor: string | null;
  operation: GitOperation | null;
  preconditions: GitPreconditions;
};
export type GitPushReconciliation = {
  receipt: GitMutationReceipt;
  remoteChecked: boolean;
  remoteOid: string | null;
  message: string;
};
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
''' + typescript_endpoints(capabilities) + typescript_conversation()


def typescript_conversation() -> str:
    """Derive conversation payloads and both frame directions from the schemas."""
    def render(node: dict, document: dict) -> str:
        if "$ref" in node:
            ref = node["$ref"]
            if ref.startswith("#/"):
                target = document
                for part in ref[2:].split("/"):
                    target = target[part]
                return render(target, document)
            return {"conversation-item.schema.json": "ConversationItem", "conversation-state.schema.json": "ConversationState"}[ref]
        if "const" in node:
            return json.dumps(node["const"])
        if "enum" in node:
            return " | ".join(json.dumps(value) for value in node["enum"])
        for union in ("oneOf", "anyOf"):
            if union in node and "properties" not in node:
                return " | ".join(render(value, document) for value in node[union])
        kind = node.get("type")
        if isinstance(kind, list):
            return " | ".join(render({**node, "type": value}, document) for value in kind)
        if kind == "object" and "oneOf" in node:
            variants = []
            for branch in node["oneOf"]:
                excluded = branch.get("not", {}).get("required", [])
                variant = {**node, "required": node.get("required", []) + branch.get("required", [])}
                del variant["oneOf"]
                variant["properties"] = {k: v for k, v in node["properties"].items() if k not in excluded}
                rendered = render(variant, document)
                if excluded:
                    rendered = rendered[:-2] + " " + " ".join(k + "?: never;" for k in excluded) + " }"
                variants.append(rendered)
            return " | ".join(variants)
        if kind == "object":
            required = node.get("required", [])
            if "properties" not in node:
                return "Record<string, " + render(node["additionalProperties"], document) + ">"
            fields = [name + ("" if name in required else "?") + ": " + render(value, document) + ";" for name, value in node["properties"].items()]
            return "{ " + " ".join(fields) + " }"
        if kind == "array":
            return "Array<" + render(node["items"], document) + ">"
        return {"integer": "number", "number": "number", "string": "string", "boolean": "boolean", "null": "null"}[kind]

    item = load("conversation-item.schema.json")
    state = load("conversation-state.schema.json")
    protocol = load("conversation-protocol.schema.json")
    server_names = {"snapshot", "items_upserted", "items_removed", "state_changed", "operation_result", "history_page", "error"}
    def frames(server: bool) -> str:
        return "\n  | ".join(render(frame, protocol) for frame in protocol["oneOf"] if (frame["properties"]["type"]["const"] in server_names) == server)
    return "\nexport type ConversationItem = " + render(item, item) + ";\n" + "export type ConversationState = " + render(state, state) + ";\n" + "export type ConversationServerMessage =\n  | " + frames(True) + ";\n" + "export type ConversationClientMessage =\n  | " + frames(False) + ";\n"


def main() -> None:
    documents = {name: load(name) for name in SCHEMA_NAMES}
    schema_digest = contract_digest()
    outputs = {
        RUST: rust_source(schema_digest),
        TYPESCRIPT: typescript_source(
            schema_digest, documents["gateway-capabilities.schema.json"]
        ),
    }
    if sys.argv[1:] == ["--check"]:
        stale = [path for path, source in outputs.items() if not path.is_file() or path.read_text() != source]
        if stale:
            names = ", ".join(str(path.relative_to(ROOT)) for path in stale)
            raise SystemExit(f"generated remote-access types are stale: {names}")
        return
    if sys.argv[1:]:
        raise SystemExit("usage: generate-remote-access-types.py [--check]")
    for path, source in outputs.items():
        path.write_text(source)


if __name__ == "__main__":
    main()
