//! What survives a restart: the saved checkpoint, the runtime delta journaled
//! after each poll, and the active-chain bookkeeping both restore.
use super::*;

/// Claude and Codex share safe JSONL mechanics, but their source vocabulary is
/// normalized by `kind`.  The type aliases keep their identities concrete at
/// the connector boundary while preventing protocol/HUB leakage.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedCheckpoint {
    source: Option<PathBuf>,
    #[serde(default)]
    source_identity: Option<SourceIdentity>,
    #[serde(default)]
    agent_session_id: Option<String>,
    offset: u64,
    active_chain: Vec<String>,
    /// The last item each active-chain record emitted. Absent in older
    /// checkpoints, which then fall back to truncating at the record id.
    #[serde(default)]
    chain_items: HashMap<String, String>,
    malformed_records: u64,
    #[serde(default)]
    hook_offset: u64,
    #[serde(default)]
    runtime: RuntimeCheckpoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct SourceIdentity {
    device: u64,
    inode: u64,
}

impl SourceIdentity {
    pub(super) fn at(path: &PathBuf) -> Option<Self> {
        fs::metadata(path).ok().as_ref().map(Self::of)
    }

    pub(super) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct RuntimeCheckpoint {
    pub(super) pending_request: Option<PendingRequest>,
    /// Requests the bridge announced while another was shown, oldest first.
    /// Clients have not been shown any of them yet. Absent in older
    /// checkpoints.
    #[serde(default)]
    pub(super) queued_requests: Vec<PendingRequest>,
    /// Requests clients were shown until the dialog on the screen proved
    /// another was the open one, most recent last. Each is still pending at
    /// the Hub. Absent in older checkpoints.
    #[serde(default)]
    pub(super) displaced_requests: Vec<PendingRequest>,
    pub(super) tools: HashMap<String, (String, String)>,
    /// Sanitized input summary per open call, so the result can say what the
    /// call was as well as how it ended. Absent in older checkpoints.
    #[serde(default)]
    pub(super) tool_summaries: HashMap<String, String>,
    pub(super) tool_running: bool,
    /// True from a real user turn until an authoritative `Stop` hook closes
    /// it. Only ever set once `hook_observer_version` proves this session's
    /// Claude launch actually emits `Stop`; otherwise the pre-existing
    /// tool-running/screen inference is the only signal, unchanged.
    #[serde(default)]
    pub(super) turn_open: bool,
    /// The `latch_observer_version` last stamped on any hook record read for
    /// this session, or `None` before any hook has been observed.
    #[serde(default)]
    pub(super) hook_observer_version: Option<u32>,
    /// Version the session's bridge module announced, while it is believed
    /// to be loaded. `None` keeps every action on the terminal path.
    #[serde(default)]
    pub(super) bridge_version: Option<u32>,
    /// When the agent last reported a turn closed. The hook sidecar is read
    /// before the transcript, so a replay sees every close before the prompts
    /// that preceded them; a prompt older than this must not reopen a turn.
    #[serde(default)]
    pub(super) last_turn_close: Option<String>,
    /// The agent's reason for closing the newest turn. Absent in older
    /// checkpoints.
    #[serde(default)]
    pub(super) turn_outcome: Option<crate::conversation::TurnOutcome>,
    /// The catalog the bridge greeting carried. Absent in older checkpoints.
    #[serde(default)]
    pub(super) commands: Option<Vec<crate::conversation::AdvertisedCommand>>,
    pub(super) last_state: Option<ConversationState>,
    pub(super) screen_can_send: Option<bool>,
}

impl JsonlConnector {
    pub(super) fn runtime_checkpoint(&self) -> RuntimeCheckpoint {
        RuntimeCheckpoint {
            pending_request: self.pending_request.clone(),
            queued_requests: self.queued_requests.clone(),
            displaced_requests: self.displaced_requests.clone(),
            tools: self.tools.clone(),
            tool_summaries: self.tool_summaries.clone(),
            tool_running: self.tool_running,
            turn_open: self.turn_open,
            hook_observer_version: self.hook_observer_version,
            bridge_version: self.bridge_version,
            last_turn_close: self.last_turn_close.clone(),
            turn_outcome: self.turn_outcome,
            commands: self.commands.clone(),
            last_state: self.last_state.clone(),
            screen_can_send: self.screen_can_send,
        }
    }

    pub(super) fn restore_runtime(&mut self, runtime: RuntimeCheckpoint) {
        self.pending_request = runtime.pending_request;
        self.queued_requests = runtime.queued_requests;
        self.displaced_requests = runtime.displaced_requests;
        self.tools = runtime.tools;
        self.tool_summaries = runtime.tool_summaries;
        self.tool_running = runtime.tool_running;
        self.turn_open = runtime.turn_open;
        self.hook_observer_version = runtime.hook_observer_version;
        self.bridge_version = runtime.bridge_version;
        self.last_turn_close = runtime.last_turn_close;
        self.turn_outcome = runtime.turn_outcome;
        self.commands = runtime.commands;
        self.last_state = runtime.last_state;
        self.screen_can_send = runtime.screen_can_send;
    }

    pub(super) fn forget_chain(&mut self) {
        self.active_chain.clear();
        self.chain_items.clear();
    }

    /// Drops every active-chain record after `index` and reports the visible
    /// consequence. The Hub truncates after an item, so the target is the
    /// nearest surviving record that owns one: the rewind's parent is often a
    /// record that never produced an item.
    pub(super) fn rewind_chain_to(&mut self, index: usize) -> Vec<ConnectorMutation> {
        let parent = self.active_chain[index].clone();
        let removed = self.active_chain.split_off(index + 1);
        if self.chain_items.is_empty() {
            // Restored from a checkpoint written before items were tracked.
            return vec![ConnectorMutation::TruncateAfter(
                ConversationItemId::native(parent),
            )];
        }
        let mut removed_item = false;
        for id in &removed {
            removed_item |= self.chain_items.remove(id).is_some();
        }
        if !removed_item {
            return Vec::new();
        }
        match self
            .active_chain
            .iter()
            .rev()
            .find_map(|id| self.chain_items.get(id))
        {
            Some(item) => vec![ConnectorMutation::TruncateAfter(
                ConversationItemId::native(item.clone()),
            )],
            None => vec![ConnectorMutation::Rebuild {
                reason: "Claude source rewound before its first item".to_owned(),
            }],
        }
    }

    /// Adopts a saved checkpoint when it was written for the current binding.
    pub(super) fn restore_saved_checkpoint(&mut self, checkpoint: &[u8]) -> Result<()> {
        // The action connector is built when the conversation is first
        // watched, which can precede the agent's SessionStart binding when a
        // phone opens Chat right after creating the session. Only the
        // observation connector polls for the binding, so adopt it here:
        // every action restores the latest checkpoint first, and without the
        // source that checkpoint never matches and every send is refused
        // while the pushed state says sending is available.
        if self.source.is_none() {
            self.refresh_binding();
        }
        if checkpoint.is_empty() {
            return Ok(());
        }
        let checkpoint: SavedCheckpoint = serde_json::from_slice(checkpoint)
            .context("incompatible conversation connector checkpoint")?;
        // A checkpoint is valid only for the same authoritative binding. A
        // changed binding deliberately replays from offset zero instead.
        if checkpoint.source == self.source && checkpoint.agent_session_id == self.agent_session_id
        {
            self.offset = checkpoint.offset;
            self.source_identity = checkpoint.source_identity;
            self.hook_offset = checkpoint.hook_offset;
            self.active_chain = checkpoint.active_chain;
            self.chain_items = checkpoint.chain_items;
            self.malformed_records = checkpoint.malformed_records;
            self.restore_runtime(checkpoint.runtime);
        }
        Ok(())
    }

    pub(super) fn merge_checkpoint_delta(&mut self, delta: &CheckpointDelta) -> Result<()> {
        if let Some(runtime) = delta.connector_state.as_ref() {
            self.restore_runtime(
                serde_json::from_slice(runtime)
                    .context("incompatible conversation connector runtime delta")?,
            );
        }
        for offset in &delta.source_offsets {
            if self
                .source
                .as_ref()
                .is_some_and(|source| source.display().to_string() == offset.source)
            {
                self.offset = self.offset.max(offset.offset);
            } else if self
                .home
                .session(&self.session)
                .conversation_source_hooks()
                .display()
                .to_string()
                == offset.source
            {
                self.hook_offset = self.hook_offset.max(offset.offset);
            }
        }
        for entry in &delta.active_branch_delta {
            if entry.source_id.is_empty() {
                continue;
            }
            if let Some(parent) = entry.parent_id.as_ref() {
                if let Some(index) = self.active_chain.iter().position(|id| id == parent) {
                    for removed in self.active_chain.split_off(index + 1) {
                        self.chain_items.remove(&removed);
                    }
                }
            }
            if self.active_chain.last() != Some(&entry.source_id) {
                self.active_chain.push(entry.source_id.clone());
            }
            if let Some(item) = entry.item_id.as_ref() {
                self.chain_items
                    .insert(entry.source_id.clone(), item.clone());
            }
        }
        Ok(())
    }

    pub(super) fn saved_checkpoint(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&SavedCheckpoint {
            source: self.source.clone(),
            source_identity: self.source_identity,
            agent_session_id: self.agent_session_id.clone(),
            offset: self.offset,
            active_chain: self.active_chain.clone(),
            chain_items: self.chain_items.clone(),
            malformed_records: self.malformed_records,
            hook_offset: self.hook_offset,
            runtime: self.runtime_checkpoint(),
        })?)
    }
}
