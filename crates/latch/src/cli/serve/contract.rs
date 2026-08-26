//! Generated from `schemas/remote-access/v2/*.schema.json`; do not edit by hand.
//! Canonical schema set SHA-256: 8deeeadf29c02a04f94411b5ac446a81512085e0619372d035e139acc5d70c23

use serde::{Deserialize, Serialize};

pub const REMOTE_ACCESS_SCHEMA_VERSION: u8 = 2;
pub const OPERATION_RETENTION_SECONDS: u64 = 10 * 60;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorIdentity {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    pub phase: ConversationPhase,
    pub send_message: OperationAvailability,
    pub resolve_request: OperationAvailability,
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
    Refused,
    Ambiguous,
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
    ResolveRequest {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        choice: String,
    },
    HistoryRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "beforeOrdinal")]
        before_ordinal: u64,
        limit: u16,
    },
}
