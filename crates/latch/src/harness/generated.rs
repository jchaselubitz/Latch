//! Generated from `fixtures/harness/*.v1.json`; do not edit by hand.

use serde::{Deserialize, Serialize};

/// One normalized harness observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEvent {
    /// Harness session identifier.
    pub session_id: String,
    /// RFC 3339 observation timestamp.
    pub at: String,
    /// Harness version that produced the raw record.
    pub harness_version: String,
    /// Connector derivation epoch used to validate cursors.
    pub connector_epoch: u32,
    /// Variant-specific event data.
    #[serde(flatten)]
    pub payload: HarnessEventPayload,
}

/// Variant-specific event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEventPayload {
    /// User-authored text.
    UserMessage {
        /// Message text.
        text: String,
    },
    /// Streaming assistant text.
    AssistantDelta {
        /// Delta text.
        text: String,
    },
    /// Complete assistant text.
    AssistantMessage {
        /// Message text.
        text: String,
    },
    /// Tool invocation began.
    ToolStarted {
        /// Harness-native tool name.
        tool: String,
        /// Reduced, safe input details.
        input: serde_json::Value,
    },
    /// Tool invocation completed.
    ToolFinished {
        /// Harness-native tool name.
        tool: String,
        /// Reduced, safe output details.
        output: serde_json::Value,
    },
    /// The harness is waiting for a person.
    AwaitingInput {
        /// Stable native request identifier.
        #[serde(rename = "requestId")]
        request_id: String,
        /// Permission or structured question.
        kind: AwaitingInputKind,
        /// Human-readable request.
        prompt: String,
        /// Choices when the harness supplied a closed set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<String>>,
    },
    /// Harness lifecycle state changed.
    Status {
        /// Harness-native normalized status.
        status: String,
    },
}

/// Kinds of input that can block a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingInputKind {
    /// A tool needs authorization.
    Permission,
    /// The harness asked a structured question.
    Question,
}

/// Operations a client may safely offer for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionCapabilities {
    /// Free-text message injection.
    pub send_message: bool,
    /// Direct keypress injection.
    pub send_keys: bool,
    /// Resolution of a specific pending request.
    pub resolve: bool,
    /// Current input-safety decision.
    pub can_send: CanSend,
}

/// Current input-safety decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanSend {
    /// Whether input is currently safe.
    pub ok: bool,
    /// Why input is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
