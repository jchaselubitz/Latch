//! Generated from `schemas/remote-access/v2/*.schema.json`; do not edit by hand.
//! Canonical schema set SHA-256: 83bb60480f37a6df7de48053e00ba606c23bc059bf48713df5e4555e4bdf3167

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

/// Reason carried in a terminal WebSocket close frame. `Detached` is a clean
/// end; every other value says why the single exclusive surface was taken away.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCloseReason {
    Detached,
    Stolen,
    SlowClient,
    SessionExited,
    KernelError,
    ResumeRefused,
}

impl TerminalCloseReason {
    /// Application close code paired with this reason. `Detached` uses the
    /// ordinary 1000.
    pub const fn close_code(self) -> u16 {
        match self {
            Self::Detached => 1000,
            Self::SlowClient => 4408,
            Self::Stolen => 4409,
            Self::SessionExited => 4410,
            Self::KernelError => 4500,
            Self::ResumeRefused => 4411,
        }
    }
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
