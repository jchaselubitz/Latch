// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 9dd8707da286f9e25088b8895b680f697bef1cfb65cee42fe9136cb93485d760


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
  gitCommitMessage?: GitDraftReadiness;
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
/** Routes the gateway serves. An absent key means the gateway predates
 * that route: treat it as unavailable, never as false-by-default. */
export type GatewayEndpoints = {
  sessions: boolean;
  preview?: boolean;
  terminal: boolean;
  conversation: boolean;
  browseDirectories?: boolean;
  createSession?: boolean;
  stopSession?: boolean;
  attachments?: boolean;
  agentModels?: boolean;
  notificationKey?: boolean;
  gitRepositories?: boolean;
  gitResolve?: boolean;
  gitStatus?: boolean;
  gitDiff?: boolean;
  gitBranches?: boolean;
  gitStage?: boolean;
  gitUnstage?: boolean;
  gitCommit?: boolean;
  gitCreateBranch?: boolean;
  gitSwitchBranch?: boolean;
  gitReceipts?: boolean;
  gitSync?: boolean;
  gitFetch?: boolean;
  gitFastForward?: boolean;
  gitMerge?: boolean;
  gitRebase?: boolean;
  gitContinue?: boolean;
  gitAbort?: boolean;
  gitStageResolved?: boolean;
  gitPushPreview?: boolean;
  gitPush?: boolean;
  gitPushReconcile?: boolean;
  gitCommitMessage?: boolean;
};
export type GatewayEndpointName = 'sessions' | 'preview' | 'terminal' | 'conversation' | 'browseDirectories' | 'createSession' | 'stopSession' | 'attachments' | 'agentModels' | 'notificationKey' | 'gitRepositories' | 'gitResolve' | 'gitStatus' | 'gitDiff' | 'gitBranches' | 'gitStage' | 'gitUnstage' | 'gitCommit' | 'gitCreateBranch' | 'gitSwitchBranch' | 'gitReceipts' | 'gitSync' | 'gitFetch' | 'gitFastForward' | 'gitMerge' | 'gitRebase' | 'gitContinue' | 'gitAbort' | 'gitStageResolved' | 'gitPushPreview' | 'gitPush' | 'gitPushReconcile' | 'gitCommitMessage';

export type ConversationItem = { id: string; ordinal: number; createdAt: string; kind: { type: "message"; role: "user" | "assistant"; text: string; status: "submitted" | "queued" | "observed" | "partial" | "complete" | "failed"; } | { type: "tool"; name: string; summary: string; status: "running" | "succeeded" | "failed"; parentMessageId?: string | null; } | { type: "request"; requestId: string; requestType: "permission" | "question"; prompt: string; choices: Array<string>; questions?: Array<{ question: string; header?: string; multiSelect: boolean; options: Array<{ label: string; description: string; }>; }>; status: "pending" | "resolved" | "dismissed"; }; };
export type ConversationState = { phase: "starting" | "idle" | "working" | "awaiting_input" | "exited" | "unavailable"; sendMessage: { enabled: boolean; reason?: string | null; }; resolveRequest: { enabled: boolean; reason?: string | null; }; cancelTurn?: { enabled: boolean; reason?: string | null; }; turnOutcome?: "answer" | "aborted" | "refusal" | "error" | null; commands?: Array<{ name: string; description: string; source?: string; }>; pendingRequest: string | null; connector: null | { id: string; version: string; }; };
export type ConversationServerMessage =
  | { type: "snapshot"; generation: string; revision: number; operationEpoch: string; items: Array<ConversationItem>; state: ConversationState; hasMoreBefore: boolean; reason?: "initial" | "generation" | "operation_epoch" | "overflow"; }
  | { type: "items_upserted"; generation: string; revision: number; items: Array<ConversationItem>; }
  | { type: "items_removed"; generation: string; revision: number; itemIds: Array<string>; }
  | { type: "state_changed"; generation: string; revision: number; state: ConversationState; }
  | { type: "operation_result"; operationId: string; status: "accepted" | "queued" | "refused" | "ambiguous" | "unknown"; itemId?: string | null; reason?: string | null; }
  | { type: "history_page"; requestId: string; items: Array<ConversationItem>; hasMoreBefore: boolean; }
  | { type: "error"; code: string; message: string; };
export type ConversationClientMessage =
  | { type: "resume"; generation?: string | null; afterRevision?: number | null; }
  | { type: "send_message"; operationEpoch: string; operationId: string; text: string; }
  | { type: "resolve_request"; operationEpoch: string; operationId: string; requestId: string; choice: string; answers?: never; } | { type: "resolve_request"; operationEpoch: string; operationId: string; requestId: string; answers: Record<string, string>; choice?: never; }
  | { type: "cancel_turn"; operationEpoch: string; operationId: string; }
  | { type: "operation_status"; operationId: string; }
  | { type: "history_request"; requestId: string; beforeOrdinal: number; limit: number; };

// Cloud draft contracts; capability advertisement determines availability.
export type GitDraftExpected = {
  branch: string | null;
  head: string | null;
  index: string;
};
export type GitCommitMessageRequest = {
  requestId: string;
  path: string;
  expected: GitDraftExpected;
};
export type GitDraftSource = {
  root: string;
  branch: string | null;
  head: string | null;
  index: string;
  capturedIndex: string;
};
export type GitDraftOmissionReason = "sensitive_path" | "credential" | "binary" | "patch_limit" | "invalid_text" | "submodule";
export type GitDraftOmission = {
  path: string | null;
  reason: GitDraftOmissionReason;
};
export type GitDraftCoverage = {
  totalFiles: number;
  includedFiles: number;
  patchFiles: number;
  patchBytes: number;
  omissions: Array<GitDraftOmission>;
};
export type GitCommitMessageResponse = {
  requestId: string;
  summary: string;
  body: string | null;
  source: GitDraftSource;
  coverage: GitDraftCoverage;
};
export type GitDraftFailureCode = "unsupported" | "unconfigured" | "invalid_request" | "unauthorized" | "no_staged_changes" | "stale_source" | "unusable_context" | "context_too_large" | "git_unavailable" | "provider_refusal" | "provider_error" | "rate_limited" | "timed_out" | "cancelled";
export type GitCommitMessageFailure = {
  requestId: string | null;
  code: GitDraftFailureCode;
  message: string;
  retryAfterSeconds: number | null;
};
export type GitDraftReadinessState = "unconfigured" | "ready" | "unavailable";
export type GitDraftReadiness = {
  state: GitDraftReadinessState;
};
export type GitCommitMessageCancelRequest = {
  requestId: string;
};
