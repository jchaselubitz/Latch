//! The connector used for a session no agent adapter claims yet.
//!
//! It exists so the Hub, the WebSocket, and grant enforcement are complete and
//! testable before the Claude and Codex adapters land. It performs no I/O: it
//! observes nothing and refuses every action with a stable reason. Terminal
//! attach remains the fallback for these sessions.

use anyhow::Result;

use super::{
    ActionDescriptor, ApplyResult, CheckpointDelta, Connector, ConnectorAction, ConnectorMutation,
    ConversationItemId, Detection, PollBudget, PollResult, ACTION_RESOLVE_REQUEST,
    ACTION_SEND_MESSAGE,
};
use crate::cli::serve::routes::Grant;

const REASON: &str = "no conversation connector is available for this session yet";

#[derive(Debug, Default)]
pub struct PendingConnector;

impl PendingConnector {
    pub fn new() -> Self {
        Self
    }
}

impl Connector for PendingConnector {
    fn detect(&self) -> Detection {
        Detection::Pending {
            reason: REASON.to_owned(),
        }
    }
    fn poll(&mut self, _budget: PollBudget) -> Result<PollResult> {
        Ok(PollResult {
            mutations: Vec::new(),
            checkpoint_delta: CheckpointDelta {
                source_offsets: Vec::new(),
                active_branch_delta: Vec::new(),
                connector_state: None,
            },
        })
    }
    /// The descriptors are advertised even though they are disabled: the Hub
    /// needs their required grant to refuse an observe-only device correctly.
    fn actions(&self) -> Vec<ActionDescriptor> {
        [ACTION_SEND_MESSAGE, ACTION_RESOLVE_REQUEST]
            .into_iter()
            .map(|id| ActionDescriptor {
                id: id.to_owned(),
                required_grant: Grant::Interact,
                enabled: false,
                reason: Some(REASON.to_owned()),
            })
            .collect()
    }
    fn apply(&mut self, _action: ConnectorAction) -> Result<ApplyResult> {
        Ok(ApplyResult::Refused {
            reason: REASON.to_owned(),
        })
    }
    fn reconcile(
        &self,
        _outstanding: &[ConversationItemId],
        _observed: &[ConversationItemId],
    ) -> Vec<ConnectorMutation> {
        Vec::new()
    }
    fn checkpoint_snapshot(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}
