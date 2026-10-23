//! Connector boundary. Implementations keep all agent-specific I/O behind this trait.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{ConnectorIdentity, ConnectorMutation, ConversationItemId};
use crate::cli::serve::routes::Grant;

/// Action ids every connector must recognise. They are the three structured
/// operations the v2 WebSocket can carry, so they are named here rather than in
/// the gateway, which must stay agent-neutral.
pub const ACTION_SEND_MESSAGE: &str = "send_message";
pub const ACTION_CANCEL_TURN: &str = "cancel_turn";
pub const ACTION_RESOLVE_REQUEST: &str = "resolve_request";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Detection {
    Unsupported,
    Pending { reason: String },
    Supported(ConnectorIdentity),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PollBudget {
    pub max_records: usize,
    /// Maximum encoded transition bytes, including checkpoint deltas.
    pub max_bytes: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointDelta {
    pub source_offsets: Vec<SourceOffset>,
    pub active_branch_delta: Vec<BranchEntry>,
    /// Connector-private, bounded runtime state needed to make the offsets
    /// usable after restart (for example one pending request and active tools).
    #[serde(default, with = "compact_bytes")]
    pub connector_state: Option<Vec<u8>>,
}

/// New journals use base64; existing number-array journals remain readable.
mod compact_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => serializer.serialize_some(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Encoded {
            Compact(String),
            Legacy(Vec<u8>),
        }
        Option::<Encoded>::deserialize(deserializer)?
            .map(|encoded| match encoded {
                Encoded::Compact(value) => STANDARD.decode(value).map_err(serde::de::Error::custom),
                Encoded::Legacy(value) => Ok(value),
            })
            .transpose()
    }
}
impl CheckpointDelta {
    pub fn is_empty(&self) -> bool {
        self.source_offsets.is_empty()
            && self.active_branch_delta.is_empty()
            && self.connector_state.is_none()
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceOffset {
    pub source: String,
    pub offset: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BranchEntry {
    pub source_id: String,
    /// The record this one follows on the active branch after it was applied.
    pub parent_id: Option<String>,
    /// The last conversation item the record emitted, when it emitted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PollResult {
    pub mutations: Vec<ConnectorMutation>,
    pub checkpoint_delta: CheckpointDelta,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDescriptor {
    pub id: String,
    pub required_grant: Grant,
    pub enabled: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorAction {
    pub id: String,
    pub payload: serde_json::Value,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyResult {
    Queued {
        correlation: Option<ConversationItemId>,
    },
    Accepted {
        correlation: Option<ConversationItemId>,
    },
    Refused {
        reason: String,
    },
}

/// An agent adapter. `poll` is its sole observation-I/O entry point and `apply`
/// is its sole action-I/O entry point; callers run both outside the state actor.
pub trait Connector: Send {
    fn detect(&self) -> Detection;
    /// Blocks until observation is likely to produce a change. Event-capable
    /// connectors use `event_timeout`; fallback connectors sleep only for the
    /// shorter polling interval. The subsequent `poll` remains authoritative.
    fn wait_for_activity(
        &mut self,
        fallback_poll: Duration,
        _event_timeout: Duration,
    ) -> Result<()> {
        std::thread::sleep(fallback_poll);
        Ok(())
    }
    fn poll(&mut self, budget: PollBudget) -> Result<PollResult>;
    /// Re-emit authoritative state after the Hub temporarily degraded it.
    fn invalidate_state(&mut self) {}
    /// Rewind a failed observation to the last durable checkpoint. Runs on
    /// the serialized worker, after any timed-out poll has finished.
    fn recover_observation(&mut self, checkpoint: &[u8]) -> Result<()> {
        self.restore_checkpoint(checkpoint)?;
        self.invalidate_state();
        Ok(())
    }
    fn actions(&self) -> Vec<ActionDescriptor>;
    fn apply(&mut self, action: ConnectorAction, deadline: Duration) -> Result<ApplyResult>;
    fn reconcile(
        &self,
        outstanding: &[ConversationItemId],
        observed: &[ConversationItemId],
    ) -> Vec<ConnectorMutation>;
    /// Restores the compact connector checkpoint before journal deltas are
    /// replayed. Agent-owned sources remain authoritative; an incompatible
    /// checkpoint is ignored by the connector and it safely re-observes.
    fn restore_checkpoint(&mut self, _checkpoint: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Replays one append-only source delta after restoring the compact base.
    fn apply_checkpoint_delta(&mut self, _delta: &CheckpointDelta) -> Result<()> {
        Ok(())
    }
    /// Full state only for periodic compaction; per-poll deltas are returned by `poll`.
    fn checkpoint_snapshot(&self) -> Result<Vec<u8>>;
}
