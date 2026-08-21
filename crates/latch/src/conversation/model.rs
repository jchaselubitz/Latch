use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Identity of a conversation, scoped to its Latch session.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ConversationId(String);

impl ConversationId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity of one derivation of a conversation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct GenerationId(u64);
impl GenerationId {
    pub(crate) const fn initial() -> Self {
        Self(1)
    }
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
    /// Opaque wire form. Clients compare it for equality and never parse it.
    pub fn as_wire(self) -> String {
        format!("generation-{}", self.0)
    }
    pub fn from_wire(value: &str) -> Option<Self> {
        value.strip_prefix("generation-")?.parse().ok().map(Self)
    }
}
impl fmt::Display for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "generation-{}", self.0)
    }
}

/// Monotonic revision within one generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Revision(u64);
impl Revision {
    pub const fn zero() -> Self {
        Self(0)
    }
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Observation order assigned exclusively by the projection/HUB.
///
/// Its representation and constructor are private: connectors and external
/// consumers can read an ordinal but cannot mint one.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Ordinal(u64);
impl Ordinal {
    pub fn get(self) -> u64 {
        self.0
    }
    /// Reconstructs a client-supplied pagination boundary. It selects an
    /// existing position and can never mint a new observation slot.
    pub const fn boundary(value: u64) -> Self {
        Self(value)
    }
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
    pub(crate) const fn first() -> Self {
        Self(1)
    }
}

/// Stable item identity within a conversation generation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ConversationItemId(String);
impl ConversationItemId {
    pub fn native(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Stable fallback when an agent source has no native identifier.
    pub fn derived(connector_id: &str, connector_epoch: &str, source_record_id: &str) -> Self {
        let mut hash = Sha256::new();
        for part in [connector_id, "\0", connector_epoch, "\0", source_record_id] {
            hash.update(part.as_bytes());
        }
        Self(format!("derived-{:x}", hash.finalize()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MessageStatus {
    Submitted,
    Observed,
    Partial,
    Complete,
    Failed,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RequestType {
    Permission,
    Question,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RequestStatus {
    Pending,
    Resolved,
    Dismissed,
}

/// Renderable agent-neutral item payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
        parent_message_id: Option<ConversationItemId>,
    },
    Request {
        request_id: String,
        request_type: RequestType,
        prompt: String,
        choices: Vec<String>,
        status: RequestStatus,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationItem {
    pub id: ConversationItemId,
    pub ordinal: Ordinal,
    pub created_at: String,
    pub kind: ConversationItemKind,
}

/// Connector-provided item. It deliberately has no ordinal, revision, or generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedItem {
    pub id: ConversationItemId,
    pub created_at: String,
    pub kind: ConversationItemKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConversationPhase {
    Starting,
    Idle,
    Working,
    AwaitingInput,
    Exited,
    Unavailable,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Availability {
    pub enabled: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorIdentity {
    pub id: String,
    pub version: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationState {
    pub phase: ConversationPhase,
    pub send_message: Availability,
    pub resolve_request: Availability,
    pub pending_request: Option<String>,
    pub connector: Option<ConnectorIdentity>,
}
impl ConversationState {
    pub fn starting(connector: Option<ConnectorIdentity>) -> Self {
        Self {
            phase: ConversationPhase::Starting,
            send_message: Availability {
                enabled: false,
                reason: Some("conversation is starting".into()),
            },
            resolve_request: Availability {
                enabled: false,
                reason: Some("no pending request".into()),
            },
            pending_request: None,
            connector,
        }
    }
    /// No connector can observe or act on this session yet. Terminal attach
    /// stays available as the fallback; the reason says why.
    pub fn unavailable(connector: Option<ConnectorIdentity>, reason: String) -> Self {
        Self {
            phase: ConversationPhase::Unavailable,
            send_message: Availability {
                enabled: false,
                reason: Some(reason.clone()),
            },
            resolve_request: Availability {
                enabled: false,
                reason: Some(reason),
            },
            pending_request: None,
            connector,
        }
    }
}

/// Connector vocabulary. The Hub owns all transport-level ordering values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConnectorMutation {
    Upsert(ObservedItem),
    TruncateAfter(ConversationItemId),
    State(ConversationState),
    Rebuild { reason: String },
}

/// Operation deduplication epoch owned by the Hub/cache, distinct from generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationEpoch(String);
impl OperationEpoch {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
