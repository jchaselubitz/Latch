use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::serve::routes::Grant;
use crate::engine::{ConversationControl, ConversationWake};
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId};

use super::super::{
    ActionDescriptor, ApplyResult, CheckpointDelta, Connector, ConnectorAction, ConnectorIdentity,
    ConnectorMutation, ConversationItemId, ConversationItemKind, ConversationPhase,
    ConversationState, Detection, MessageRole, MessageStatus, ObservedItem, PollBudget, PollResult,
    RequestStatus, RequestType, ToolStatus, ACTION_RESOLVE_REQUEST, ACTION_SEND_MESSAGE,
    MAX_CONVERSATION_ITEM_BYTES, MAX_MESSAGE_TEXT_BYTES,
};

const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_READ_BYTES: usize = 2 * 1024 * 1024;

mod cursor;
#[cfg(test)]
mod cursor_live_tests;
#[cfg(test)]
mod geometry_tests;

/// The connector a session's persisted harness marker selects, or `None` when
/// the session has no conversation connector and never will.
///
/// This is the single answer to "does this session have a connector". The
/// session list reports it and the Conversation Hub builds from it, so the two
/// cannot drift apart into contradicting each other about the same session.
pub fn connector_kind(harness: Option<&str>) -> Option<&'static str> {
    match harness {
        Some("claude") => Some("claude"),
        Some("codex") => Some("codex"),
        Some("cursor") => Some("cursor"),
        _ => None,
    }
}

/// Builds exactly one agent adapter from the session's persisted launch marker.
/// The factory never searches a working directory or selects a recently changed
/// transcript: an adapter stays pending until its own agent supplied a binding.
pub fn connector_for_session(
    home: LatchHome,
    conversation: &super::super::ConversationId,
) -> Box<dyn Connector> {
    let Ok(session) = SessionId::parse(conversation.as_str()) else {
        return Box::new(super::super::PendingConnector::new());
    };
    let harness = meta::read(&home.session(&session))
        .ok()
        .and_then(|meta| meta.harness);
    match connector_kind(harness.as_deref()) {
        Some(_) => Box::new(JsonlConnector::for_session(home, session)),
        None => Box::new(super::super::PendingConnector::new()),
    }
}

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
struct SourceIdentity {
    device: u64,
    inode: u64,
}

impl SourceIdentity {
    fn at(path: &PathBuf) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PendingRequest {
    id: String,
    request_type: RequestType,
    prompt: String,
    choices: Vec<String>,
    /// A permission hook can arrive a fraction before Claude paints its
    /// prompt. Do not mistake that first empty snapshot for a dismissal.
    #[serde(default)]
    screen_seen: bool,
    /// Claude's transcript and hook sidecar advance independently. This lets
    /// us ignore transcript records that existed before a newly-read hook.
    #[serde(default)]
    announced_at: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct RuntimeCheckpoint {
    pending_request: Option<PendingRequest>,
    tools: HashMap<String, (String, String)>,
    /// Sanitized input summary per open call, so the result can say what the
    /// call was as well as how it ended. Absent in older checkpoints.
    #[serde(default)]
    tool_summaries: HashMap<String, String>,
    tool_running: bool,
    /// True from a real user turn until an authoritative `Stop` hook closes
    /// it. Only ever set once `hook_observer_version` proves this session's
    /// Claude launch actually emits `Stop`; otherwise the pre-existing
    /// tool-running/screen inference is the only signal, unchanged.
    #[serde(default)]
    turn_open: bool,
    /// The `latch_observer_version` last stamped on any hook record read for
    /// this session, or `None` before any hook has been observed.
    #[serde(default)]
    hook_observer_version: Option<u32>,
    /// Version the session's bridge module announced, while it is believed
    /// to be loaded. `None` keeps every action on the terminal path.
    #[serde(default)]
    bridge_version: Option<u32>,
    /// When the agent last reported a turn closed. The hook sidecar is read
    /// before the transcript, so a replay sees every close before the prompts
    /// that preceded them; a prompt older than this must not reopen a turn.
    #[serde(default)]
    last_turn_close: Option<String>,
    last_state: Option<ConversationState>,
    screen_can_send: Option<bool>,
}

#[derive(Debug)]
pub struct JsonlConnector {
    id: &'static str,
    version: &'static str,
    home: LatchHome,
    session: SessionId,
    source: Option<PathBuf>,
    source_identity: Option<SourceIdentity>,
    agent_session_id: Option<String>,
    offset: u64,
    hook_offset: u64,
    active_chain: Vec<String>,
    /// Source record id to the last conversation item that record emitted.
    /// A rewind truncates after an item, and most source records own none.
    chain_items: HashMap<String, String>,
    malformed_records: u64,
    pending_request: Option<PendingRequest>,
    tools: HashMap<String, (String, String)>,
    tool_summaries: HashMap<String, String>,
    tool_running: bool,
    turn_open: bool,
    hook_observer_version: Option<u32>,
    bridge_version: Option<u32>,
    last_turn_close: Option<String>,
    last_state: Option<ConversationState>,
    screen_can_send: Option<bool>,
    live_screen: bool,
    last_screen_refresh: Option<Instant>,
    control: Option<ConversationControl>,
    refresh_screen: bool,
    #[cfg(test)]
    last_read_bytes: usize,
}

impl JsonlConnector {
    pub fn for_session(home: LatchHome, session: SessionId) -> Self {
        let harness = meta::read(&home.session(&session))
            .ok()
            .and_then(|value| value.harness);
        let id = connector_kind(harness.as_deref()).unwrap_or("unknown");
        let version = "1";
        let binding = read_binding(&home, &session, id);
        let source = binding.as_ref().map(|binding| binding.0.clone());
        let control = ConversationControl::open(&home, &session).ok();
        Self {
            id,
            version,
            home,
            session,
            source_identity: source.as_ref().and_then(SourceIdentity::at),
            source,
            agent_session_id: binding.and_then(|binding| binding.1),
            offset: 0,
            hook_offset: 0,
            active_chain: Vec::new(),
            chain_items: HashMap::new(),
            malformed_records: 0,
            pending_request: None,
            tools: HashMap::new(),
            tool_summaries: HashMap::new(),
            tool_running: false,
            turn_open: false,
            hook_observer_version: None,
            bridge_version: None,
            last_turn_close: None,
            last_state: None,
            screen_can_send: None,
            live_screen: true,
            last_screen_refresh: None,
            control,
            refresh_screen: true,
            #[cfg(test)]
            last_read_bytes: 0,
        }
    }

    #[cfg(test)]
    fn fixture(id: &'static str, source: PathBuf) -> Self {
        let home = LatchHome::new("/tmp/latch-connector-fixture");
        Self {
            id,
            version: "1",
            home,
            session: SessionId::parse("ses_fixture").unwrap(),
            source_identity: SourceIdentity::at(&source),
            source: Some(source),
            agent_session_id: Some("fixture".to_owned()),
            offset: 0,
            hook_offset: 0,
            active_chain: Vec::new(),
            chain_items: HashMap::new(),
            malformed_records: 0,
            pending_request: None,
            tools: HashMap::new(),
            tool_summaries: HashMap::new(),
            tool_running: false,
            turn_open: false,
            hook_observer_version: None,
            bridge_version: None,
            last_turn_close: None,
            last_state: None,
            screen_can_send: None,
            live_screen: false,
            last_screen_refresh: None,
            control: None,
            refresh_screen: false,
            #[cfg(test)]
            last_read_bytes: 0,
        }
    }

    fn identity(&self) -> ConnectorIdentity {
        ConnectorIdentity {
            id: self.id.to_owned(),
            version: self.version.to_owned(),
        }
    }

    fn runtime_checkpoint(&self) -> RuntimeCheckpoint {
        RuntimeCheckpoint {
            pending_request: self.pending_request.clone(),
            tools: self.tools.clone(),
            tool_summaries: self.tool_summaries.clone(),
            tool_running: self.tool_running,
            turn_open: self.turn_open,
            hook_observer_version: self.hook_observer_version,
            bridge_version: self.bridge_version,
            last_turn_close: self.last_turn_close.clone(),
            last_state: self.last_state.clone(),
            screen_can_send: self.screen_can_send,
        }
    }

    fn restore_runtime(&mut self, runtime: RuntimeCheckpoint) {
        self.pending_request = runtime.pending_request;
        self.tools = runtime.tools;
        self.tool_summaries = runtime.tool_summaries;
        self.tool_running = runtime.tool_running;
        self.turn_open = runtime.turn_open;
        self.hook_observer_version = runtime.hook_observer_version;
        self.bridge_version = runtime.bridge_version;
        self.last_turn_close = runtime.last_turn_close;
        self.last_state = runtime.last_state;
        self.screen_can_send = runtime.screen_can_send;
    }

    /// Whether this session's Claude launch is known to emit an authoritative
    /// `Stop` hook. `false` until a hook record has proven otherwise, which
    /// keeps every pre-existing session on the tool/screen inference it
    /// already had instead of fabricating a turn boundary it cannot back.
    fn stop_hook_supported(&self) -> bool {
        self.hook_observer_version
            .is_some_and(|version| version >= crate::observer::STOP_HOOK_MIN_OBSERVER_VERSION)
    }

    fn forget_chain(&mut self) {
        self.active_chain.clear();
        self.chain_items.clear();
    }

    /// Drops every active-chain record after `index` and reports the visible
    /// consequence. The Hub truncates after an item, so the target is the
    /// nearest surviving record that owns one: the rewind's parent is often a
    /// record that never produced an item.
    fn rewind_chain_to(&mut self, index: usize) -> Vec<ConnectorMutation> {
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

    fn control(&mut self) -> Result<&mut ConversationControl> {
        if self.control.is_none() {
            self.control = Some(ConversationControl::open(&self.home, &self.session)?);
        }
        Ok(self.control.as_mut().expect("control installed"))
    }

    fn current_screen(&mut self, deadline: Duration) -> Result<String> {
        let snapshot = self.control()?.snapshot(deadline)?;
        if self.id == "cursor" {
            // An old empty composer in scrollback is not an input target.
            return Ok(snapshot.lines.join("\n"));
        }
        Ok(snapshot
            .history
            .into_iter()
            .chain(snapshot.lines)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn refresh_binding(&mut self) -> bool {
        let Some((source, agent_session_id)) = read_binding(&self.home, &self.session, self.id)
        else {
            return false;
        };
        if self.source.as_ref() == Some(&source) && self.agent_session_id == agent_session_id {
            return false;
        }
        let replacing = self.source.is_some();
        let cursor_turn_open = self.id == "cursor" && !replacing && self.turn_open;
        self.source_identity = SourceIdentity::at(&source);
        self.source = Some(source);
        self.agent_session_id = agent_session_id;
        self.offset = 0;
        self.forget_chain();
        self.pending_request = None;
        self.tools.clear();
        self.tool_summaries.clear();
        self.tool_running = false;
        // `hook_observer_version` deliberately survives a rebind, exactly
        // like `hook_offset`: both describe the one continuous hook sidecar
        // for this Latch session, not the specific source file currently
        // bound.
        self.turn_open = cursor_turn_open;
        self.last_state = None;
        self.screen_can_send = None;
        self.last_screen_refresh = None;
        replacing
    }

    fn state(&self) -> ConversationState {
        let (phase, send_message, resolve_request) =
            if self.source.is_none() && !(self.id == "cursor" && self.turn_open) {
                // Codex creates its thread on the first prompt, so its
                // SessionStart hook cannot bind a rollout before that prompt.
                // The visible empty composer is enough to safely submit the
                // first message without taking the terminal surface.
                let can_start = matches!(self.id, "codex" | "cursor")
                    && self.screen_can_send == Some(true)
                    && !self.turn_open;
                (
                    ConversationPhase::Starting,
                    (
                        can_start,
                        (!can_start).then(|| "waiting for the agent's empty composer".to_owned()),
                    ),
                    (
                        false,
                        Some("waiting for the agent's authoritative source binding".to_owned()),
                    ),
                )
            } else if self.pending_request.is_some() {
                (
                    ConversationPhase::AwaitingInput,
                    (false, Some("resolve the pending request first".to_owned())),
                    (true, None),
                )
            } else if self.tool_running || self.turn_open {
                (
                    ConversationPhase::Working,
                    (
                        self.bridge_version.is_some(),
                        self.bridge_version
                            .is_none()
                            .then(|| "agent is working".to_owned()),
                    ),
                    (false, Some("no pending request".to_owned())),
                )
            } else if self.screen_can_send == Some(false) && self.bridge_version.is_none() {
                // Only the terminal path types into the composer. The bridge
                // submits through the agent itself and leaves a draft alone.
                (
                    ConversationPhase::Idle,
                    (false, Some("the agent composer is not empty".to_owned())),
                    (false, Some("no pending request".to_owned())),
                )
            } else {
                (
                    ConversationPhase::Idle,
                    (true, None),
                    (false, Some("no pending request".to_owned())),
                )
            };
        ConversationState {
            phase,
            send_message: super::super::Availability {
                enabled: send_message.0,
                reason: send_message.1,
            },
            resolve_request: super::super::Availability {
                enabled: resolve_request.0,
                reason: resolve_request.1,
            },
            cancel_turn: super::super::Availability {
                enabled: self.id == "claude" && self.bridge_version.is_some() && self.turn_open,
                reason: (!(self.bridge_version.is_some() && self.turn_open))
                    .then(|| "no running turn with a live bridge".to_owned()),
            },
            pending_request: self
                .pending_request
                .as_ref()
                .map(|request| request.id.clone()),
            connector: Some(self.identity()),
        }
    }

    fn observe_screen(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        let mut mutations = Vec::new();
        if let Some(request) = self.pending_request.as_mut() {
            if screen_contains_request(screen, request) {
                request.screen_seen = true;
                let choices = visible_choices(screen, &request.prompt);
                if !choices.is_empty() && request.choices != choices {
                    request.choices = choices;
                    mutations.push(request_mutation(request, RequestStatus::Pending));
                }
            } else if request.screen_seen {
                let request = self.pending_request.take().expect("request was present");
                mutations.push(request_mutation(&request, RequestStatus::Dismissed));
            }
        }
        self.screen_can_send = Some(
            self.pending_request.is_none()
                && !self.tool_running
                && screen.lines().any(|line| is_empty_composer(self.id, line)),
        );
        mutations
    }

    fn record(&mut self, value: Value, ordinal: u64) -> Vec<ConnectorMutation> {
        let object = match value.as_object() {
            Some(value) => value,
            None => {
                self.malformed_records += 1;
                return Vec::new();
            }
        };
        let event = string(object, "event")
            .or_else(|| string(object, "type"))
            .unwrap_or_default();
        if self.id == "claude" {
            return self.claude_record(object, &event, ordinal);
        }
        if self.id == "cursor" {
            return self.cursor_record(object, ordinal);
        }
        if self.id == "codex" && event == "response_item" {
            // Raw user response items can contain injected AGENTS.md and
            // environment context. Codex's completed conversation items
            // distinguish the actual user message from that setup material.
            return Vec::new();
        }
        if self.id == "codex" && event == "event_msg" {
            return self.codex_conversation_item(object, ordinal);
        }
        if matches!(event.as_str(), "branch_rewrite" | "branch_replace") {
            let parent = string(object, "parent_id").or_else(|| string(object, "parent_uuid"));
            if let Some(parent) = parent {
                if self.active_chain.contains(&parent) {
                    self.active_chain.truncate(
                        self.active_chain
                            .iter()
                            .position(|id| id == &parent)
                            .unwrap()
                            + 1,
                    );
                    // A request from the removed suffix cannot stay pending.
                    // A later source record may explicitly re-open it.
                    self.pending_request = None;
                    return vec![ConnectorMutation::TruncateAfter(
                        ConversationItemId::native(parent),
                    )];
                }
            }
            // An unclassifiable rewind is the one safe reason to rebuild: it
            // prevents a guessed branch from being presented as authoritative.
            return vec![ConnectorMutation::Rebuild {
                reason: "source branch cannot be classified".to_owned(),
            }];
        }

        let native = record_id(object, &event).unwrap_or_else(|| format!("record-{ordinal}"));
        let id = ConversationItemId::native(native.clone());
        let created_at = string(object, "created_at")
            .or_else(|| string(object, "timestamp"))
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
        let kind = match event.as_str() {
            "user_message" | "user" | "terminal_input" => Some(ConversationItemKind::Message {
                role: MessageRole::User,
                text: bounded_message_text(
                    string(object, "text")
                        .or_else(|| string(object, "message"))
                        .unwrap_or_default(),
                ),
                status: MessageStatus::Observed,
            }),
            "assistant_message" | "assistant" => Some(ConversationItemKind::Message {
                role: MessageRole::Assistant,
                text: bounded_message_text(
                    string(object, "text")
                        .or_else(|| string(object, "message"))
                        .unwrap_or_default(),
                ),
                status: message_status(object),
            }),
            "tool_call" | "tool_use" => {
                self.tool_running = !matches!(
                    string(object, "state")
                        .or_else(|| string(object, "status"))
                        .as_deref(),
                    Some("completed" | "succeeded" | "failed")
                );
                Some(ConversationItemKind::Tool {
                    name: string(object, "tool")
                        .or_else(|| string(object, "name"))
                        .unwrap_or_else(|| "tool".to_owned()),
                    summary: sanitize_summary(&string(object, "summary").unwrap_or_default()),
                    status: tool_status(object),
                    parent_message_id: string(object, "parent_id")
                        .or_else(|| string(object, "parent_uuid"))
                        .map(ConversationItemId::native),
                })
            }
            "tool_result" => {
                self.tool_running = false;
                Some(ConversationItemKind::Tool {
                    name: string(object, "tool")
                        .or_else(|| string(object, "name"))
                        .unwrap_or_else(|| "tool".to_owned()),
                    summary: sanitize_summary(&string(object, "summary").unwrap_or_default()),
                    status: tool_status(object),
                    parent_message_id: None,
                })
            }
            "approval_request" | "permission_request" | "question_request" => {
                let request_id = string(object, "request_id").unwrap_or_else(|| native.clone());
                self.pending_request = Some(PendingRequest {
                    id: request_id.clone(),
                    request_type: if event == "question_request" {
                        RequestType::Question
                    } else {
                        RequestType::Permission
                    },
                    prompt: string(object, "prompt").unwrap_or_default(),
                    choices: object
                        .get("choices")
                        .and_then(Value::as_array)
                        .map(|v| {
                            v.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    screen_seen: false,
                    announced_at: None,
                });
                Some(ConversationItemKind::Request {
                    request_id,
                    request_type: self.pending_request.as_ref().unwrap().request_type.clone(),
                    prompt: self.pending_request.as_ref().unwrap().prompt.clone(),
                    choices: self.pending_request.as_ref().unwrap().choices.clone(),
                    status: RequestStatus::Pending,
                })
            }
            "request_resolved" | "approval_resolved" | "permission_resolved" => {
                let request_id = string(object, "request_id").unwrap_or_else(|| native.clone());
                if self
                    .pending_request
                    .as_ref()
                    .map(|request| request.id.as_str())
                    == Some(request_id.as_str())
                {
                    self.pending_request = None;
                }
                Some(ConversationItemKind::Request {
                    request_id,
                    request_type: RequestType::Permission,
                    prompt: string(object, "prompt").unwrap_or_default(),
                    choices: Vec::new(),
                    status: RequestStatus::Resolved,
                })
            }
            _ => None,
        };
        let Some(kind) = kind else {
            return Vec::new();
        };
        self.active_chain.push(native);
        vec![ConnectorMutation::Upsert(ObservedItem {
            id,
            created_at,
            kind,
        })]
    }

    /// Codex's completed conversation item is the user-visible source. Raw
    /// response items also contain injected setup context and must stay out.
    fn codex_conversation_item(
        &mut self,
        object: &serde_json::Map<String, Value>,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            return Vec::new();
        };
        if string(payload, "type").as_deref() != Some("item_completed") {
            return Vec::new();
        }
        let Some(item) = payload.get("item").and_then(Value::as_object) else {
            return Vec::new();
        };
        let role = match string(item, "type").as_deref() {
            Some("UserMessage") => MessageRole::User,
            Some("AgentMessage") => MessageRole::Assistant,
            _ => return Vec::new(),
        };
        let text = item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| {
                let part = part.as_object()?;
                matches!(string(part, "type").as_deref(), Some("text" | "Text"))
                    .then(|| string(part, "text"))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return Vec::new();
        }
        vec![ConnectorMutation::Upsert(ObservedItem {
            id: item
                .get("id")
                .and_then(Value::as_str)
                .map(ConversationItemId::native)
                .unwrap_or_else(|| {
                    ConversationItemId::derived("codex", "message", &ordinal.to_string())
                }),
            created_at: string(object, "timestamp")
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            kind: ConversationItemKind::Message {
                role,
                text: bounded_message_text(text),
                status: MessageStatus::Observed,
            },
        })]
    }

    /// Normalizes the real Claude JSONL vocabulary. It intentionally lives at
    /// this boundary: Hub/protocol types never see Claude fields or its branch
    /// graph. The first item for a source UUID owns that UUID so a later rewind
    /// can target it with `TruncateAfter` without a synthetic timeline item.
    fn claude_record(
        &mut self,
        object: &serde_json::Map<String, Value>,
        event: &str,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let hook_event_name = string(object, "hook_event_name");
        if let Some(version) = object.get("latch_observer_version").and_then(Value::as_u64) {
            self.hook_observer_version = Some(version as u32);
        }
        if event == "permission_request" || hook_event_name.as_deref() == Some("PermissionRequest")
        {
            // Claude raises its permission hook for a question too. The
            // bridge has already announced that question under its call id,
            // with its real text; a generic permission must not replace it.
            let question_is_open = self
                .pending_request
                .as_ref()
                .is_some_and(|request| request.request_type == RequestType::Question);
            if question_is_open && string(object, "tool_name").as_deref() == Some("AskUserQuestion")
            {
                return Vec::new();
            }
            return self.claude_permission(object, ordinal);
        }
        if hook_event_name.as_deref() == Some(crate::observer::CLAUDE_BRIDGE_EVENT) {
            return self.claude_bridge_record(object);
        }
        if hook_event_name.as_deref() == Some("Stop") {
            // Authoritative turn boundary: the agent itself reported that it
            // stopped responding, so the turn this session opened is closed
            // regardless of what the transcript or screen otherwise suggest.
            self.turn_open = false;
            self.last_turn_close = string(object, "timestamp").or(self.last_turn_close.take());
            return Vec::new();
        }
        if hook_event_name.is_some() {
            return Vec::new();
        }
        let uuid = string(object, "uuid");
        if object
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Vec::new();
        }
        let Some(uuid) = uuid else { return Vec::new() };
        let parent = string(object, "parentUuid").or_else(|| string(object, "parent_uuid"));
        let content = object
            .get("message")
            .and_then(|message| message.get("content"));
        // Claude files each result of a parallel tool batch under the
        // assistant record that made that call, not under the newest record.
        // That is a sibling of the active branch, never a rewind of it.
        let carries_tool_result = event == "user"
            && content.and_then(Value::as_array).is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| string_map(block, "type").as_deref() == Some("tool_result"))
            });
        let mut mutations = Vec::new();
        match parent {
            Some(parent) if self.active_chain.last() != Some(&parent) => {
                match self.active_chain.iter().position(|id| id == &parent) {
                    Some(_) if carries_tool_result => {}
                    Some(index) => mutations.extend(self.rewind_chain_to(index)),
                    // Nothing observed yet contradicts this record, so it
                    // simply becomes the start of the observed branch.
                    None if self.active_chain.is_empty() || carries_tool_result => {}
                    None => {
                        // Rebuild once, then adopt this record below. Leaving
                        // the chain empty would make every later record
                        // unclassifiable and rebuild again without end.
                        self.forget_chain();
                        mutations.push(ConnectorMutation::Rebuild {
                            reason: "Claude source parent is outside the active branch".to_owned(),
                        });
                    }
                }
            }
            Some(_) => {}
            // Compaction starts a new physical root that continues the same
            // conversation, so the settled history stays.
            None if self.active_chain.is_empty() || claude_compaction_boundary(object, event) => {}
            None => {
                self.forget_chain();
                mutations.push(ConnectorMutation::Rebuild {
                    reason: "Claude source started an incompatible root".to_owned(),
                });
            }
        }

        let at = string(object, "timestamp").unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
        // Any authoritative main-chain progress after the request dismisses
        // it when it was not itself re-announced. Hooks are read before the
        // transcript, however, so replaying an older transcript record must
        // not instantly dismiss a permission prompt that is still on screen.
        // A question the bridge announced is closed by the bridge: other
        // calls of the same response may finish while it is still open.
        let bridge_owns_request = self.bridge_version.is_some()
            && self
                .pending_request
                .as_ref()
                .is_some_and(|request| request.request_type == RequestType::Question);
        if !bridge_owns_request
            && self.pending_request.as_ref().is_some_and(|request| {
                request
                    .announced_at
                    .as_deref()
                    .is_none_or(|announced_at| timestamp_is_after(&at, announced_at))
            })
        {
            let request = self.pending_request.take().expect("request was present");
            mutations.push(request_mutation(&request, RequestStatus::Dismissed));
        }
        match event {
            "user" => {
                // Skill bodies, reminders, and compaction summaries are
                // user-role rows the person never typed.
                let injected = ["isMeta", "isCompactSummary"]
                    .into_iter()
                    .any(|key| object.get(key).and_then(Value::as_bool).unwrap_or(false));
                let text = if injected {
                    String::new()
                } else {
                    claude_text(content)
                };
                if !text.is_empty() {
                    self.chain_items.insert(uuid.clone(), uuid.clone());
                    // A real user turn starts here. It only stays open on the
                    // authority of a later `Stop` hook when this session is
                    // known to emit one; otherwise this flag never turns on
                    // and the pre-existing tool/screen inference is unchanged.
                    if self.stop_hook_supported()
                        && !self
                            .last_turn_close
                            .as_deref()
                            .is_some_and(|close| turn_closed_at_or_after(close, &at))
                    {
                        self.turn_open = true;
                    }
                    mutations.push(upsert(
                        &uuid,
                        at.clone(),
                        ConversationItemKind::Message {
                            role: MessageRole::User,
                            text: bounded_message_text(text),
                            status: MessageStatus::Observed,
                        },
                    ));
                }
                for block in content.and_then(Value::as_array).into_iter().flatten() {
                    if string_map(block, "type").as_deref() != Some("tool_result") {
                        continue;
                    }
                    let Some(call_id) = string_value(block, "tool_use_id") else {
                        continue;
                    };
                    if let Some((name, item_id)) = self.tools.remove(&call_id) {
                        let input = self.tool_summaries.remove(&call_id).unwrap_or_default();
                        let (status, summary) = claude_tool_outcome(&input, block);
                        mutations.push(upsert(
                            &item_id,
                            at.clone(),
                            ConversationItemKind::Tool {
                                name,
                                summary,
                                status,
                                parent_message_id: None,
                            },
                        ));
                        self.tool_running = false;
                    }
                }
            }
            "assistant" => {
                let blocks = object
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (index, block) in blocks.iter().enumerate() {
                    let item_id = if index == 0 {
                        uuid.clone()
                    } else {
                        format!("{uuid}:block:{index}")
                    };
                    match string_map(block, "type").as_deref() {
                        Some("text") => {
                            if let Some(text) =
                                string_value(block, "text").filter(|text| !text.is_empty())
                            {
                                self.chain_items.insert(uuid.clone(), item_id.clone());
                                mutations.push(upsert(
                                    &item_id,
                                    at.clone(),
                                    ConversationItemKind::Message {
                                        role: MessageRole::Assistant,
                                        text: bounded_message_text(text),
                                        status: MessageStatus::Complete,
                                    },
                                ));
                            }
                        }
                        Some("tool_use") => {
                            let call_id =
                                string_value(block, "id").unwrap_or_else(|| item_id.clone());
                            let name =
                                string_value(block, "name").unwrap_or_else(|| "tool".to_owned());
                            let summary = safe_tool_summary(&name, block.get("input"));
                            self.tools
                                .insert(call_id.clone(), (name.clone(), item_id.clone()));
                            self.tool_summaries.insert(call_id.clone(), summary.clone());
                            // The prior fallback signal for `Working`: no
                            // hook tells us a tool started, so the transcript
                            // record itself is authoritative for this half of
                            // the boundary regardless of Stop-hook support.
                            self.tool_running = true;
                            self.chain_items.insert(uuid.clone(), item_id.clone());
                            mutations.push(upsert(
                                &item_id,
                                at.clone(),
                                ConversationItemKind::Tool {
                                    name: name.clone(),
                                    summary,
                                    status: ToolStatus::Running,
                                    parent_message_id: Some(ConversationItemId::native(
                                        uuid.clone(),
                                    )),
                                },
                            ));
                            // With a bridge the question was announced when it
                            // opened; by the time its call is in the transcript
                            // it has been answered.
                            if name == "AskUserQuestion" && self.bridge_version.is_none() {
                                let request = PendingRequest {
                                    id: call_id,
                                    request_type: RequestType::Question,
                                    prompt: claude_question_prompt(block.get("input")),
                                    choices: claude_question_choices(block.get("input")),
                                    screen_seen: false,
                                    announced_at: None,
                                };
                                self.pending_request = Some(request.clone());
                                mutations.push(request_mutation(&request, RequestStatus::Pending));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        self.active_chain.push(uuid);
        mutations
    }

    /// One record from the session's bridge module: what the engine itself
    /// reported, as opposed to what the transcript or screen imply.
    fn claude_bridge_record(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        match string(object, "bridge_event").as_deref() {
            // The transcript gains a question's call only once it is answered,
            // so the bridge is what knows one is open and under which id.
            Some("question.open") => {
                let Some(id) = string(object, "tool_use_id") else {
                    return Vec::new();
                };
                let input = Value::Object(object.clone());
                let request = PendingRequest {
                    id,
                    request_type: RequestType::Question,
                    prompt: claude_question_prompt(Some(&input)),
                    choices: claude_question_choices(Some(&input)),
                    screen_seen: false,
                    announced_at: string(object, "timestamp"),
                };
                self.pending_request = Some(request.clone());
                return vec![request_mutation(&request, RequestStatus::Pending)];
            }
            Some("question.closed") => {
                let closed = string(object, "tool_use_id");
                if self.pending_request.as_ref().map(|request| &request.id) == closed.as_ref() {
                    let request = self.pending_request.take().expect("request was present");
                    return vec![request_mutation(&request, RequestStatus::Dismissed)];
                }
            }
            Some("hello") => {
                self.bridge_version = object
                    .get("bridge_version")
                    .and_then(Value::as_u64)
                    .and_then(|version| u32::try_from(version).ok());
            }
            Some("turn.start") => self.turn_open = true,
            Some("turn.complete") => {
                // Answered, interrupted, refused, or failed: the main loop
                // has stopped, so nothing of this turn is still running.
                self.turn_open = false;
                self.tool_running = false;
                self.last_turn_close = string(object, "timestamp").or(self.last_turn_close.take());
            }
            // `/clear` ends the conversation and keeps the process, and with
            // it the loaded module. Every other end takes the module along.
            Some("session.end") if string(object, "reason").as_deref() != Some("clear") => {
                self.bridge_version = None;
                self.turn_open = false;
            }
            _ => {}
        }
        Vec::new()
    }

    /// Hands one command to the bridge module and waits for its word on it.
    fn ask_bridge(
        &mut self,
        kind: &str,
        payload: serde_json::Map<String, Value>,
        budget: Duration,
    ) -> Result<BridgeAnswer> {
        let paths = self.home.session(&self.session);
        let sidecar = paths.conversation_source_hooks();
        let results_from = fs::metadata(&sidecar).map(|meta| meta.len()).unwrap_or(0);
        let id = format!(
            "{kind}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let command = crate::observer::queue_claude_bridge_command(&paths, &id, kind, payload)?;
        let started = Instant::now();
        let pause = Duration::from_millis(40);
        while command.exists() {
            if started.elapsed() >= BRIDGE_CLAIM_TIMEOUT.min(budget) {
                // Removing the command first is what makes the terminal
                // fallback safe: a module that wakes later finds nothing.
                if fs::remove_file(&command).is_ok() {
                    return Ok(BridgeAnswer::NotTaken);
                }
                break;
            }
            std::thread::sleep(pause);
        }
        loop {
            if let Some(result) = bridge_command_result(&sidecar, results_from, &id) {
                return Ok(match string(&result, "outcome").as_deref() {
                    Some("accepted") => BridgeAnswer::Accepted,
                    Some("queued") => BridgeAnswer::Queued,
                    _ => BridgeAnswer::Refused(
                        string(&result, "detail")
                            .unwrap_or_else(|| "the agent refused the request".to_owned()),
                    ),
                });
            }
            if started.elapsed() >= budget {
                anyhow::bail!("the agent took the request but did not confirm it");
            }
            std::thread::sleep(pause);
        }
    }

    fn claude_permission(
        &mut self,
        object: &serde_json::Map<String, Value>,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let request = PendingRequest {
            id: string(object, "request_id")
                .or_else(|| string(object, "prompt_id"))
                .unwrap_or_else(|| {
                    format!(
                        "permission:{}:{ordinal}",
                        string(object, "tool_name").unwrap_or_else(|| "tool".to_owned())
                    )
                }),
            request_type: RequestType::Permission,
            prompt: object
                .get("tool_input")
                .or_else(|| object.get("toolInput"))
                .and_then(|input| input.get("description"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Allow {}?",
                        string(object, "tool_name").unwrap_or_else(|| "this tool".to_owned())
                    )
                }),
            // Some providers include choices in the hook payload. Preserve
            // those as a fallback, but replace them with the numbered labels
            // Claude actually paints before presenting the request to a user.
            choices: object
                .get("choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            screen_seen: false,
            announced_at: string(object, "timestamp"),
        };
        self.pending_request = Some(request.clone());
        vec![request_mutation(&request, RequestStatus::Pending)]
    }
}

/// The record Claude writes when it compacts: a new physical root that names
/// the branch it continues.
fn claude_compaction_boundary(object: &serde_json::Map<String, Value>, event: &str) -> bool {
    object.contains_key("logicalParentUuid")
        || (event == "system" && string(object, "subtype").as_deref() == Some("compact_boundary"))
}

fn upsert(id: &str, created_at: String, kind: ConversationItemKind) -> ConnectorMutation {
    ConnectorMutation::Upsert(ObservedItem {
        id: ConversationItemId::native(id),
        created_at,
        kind,
    })
}
fn request_mutation(request: &PendingRequest, status: RequestStatus) -> ConnectorMutation {
    upsert(
        &format!("request:{}", request.id),
        "1970-01-01T00:00:00Z".to_owned(),
        ConversationItemKind::Request {
            request_id: request.id.clone(),
            request_type: request.request_type.clone(),
            prompt: request.prompt.clone(),
            choices: request.choices.clone(),
            status,
        },
    )
}
fn string_value(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn string_map(value: &Value, key: &str) -> Option<String> {
    string_value(value, key)
}
fn claude_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| string_map(block, "type").as_deref() == Some("text"))
            .filter_map(|block| string_value(block, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Agent transcripts can contain a long launch prompt or response. Keep the
/// visible beginning and end within one wire message's text budget so a single
/// source record cannot strand the Hub's first observation on its item limit.
fn bounded_message_text(text: String) -> String {
    const OMITTED: &str = "\n\n[… middle omitted from chat …]\n\n";
    const ENCODED_TEXT_BUDGET: usize = MAX_CONVERSATION_ITEM_BYTES - 4 * 1024;
    if text.len() <= MAX_MESSAGE_TEXT_BYTES
        && serde_json::to_vec(&text).is_ok_and(|encoded| encoded.len() <= ENCODED_TEXT_BUDGET)
    {
        return text;
    }
    let mut available = MAX_MESSAGE_TEXT_BYTES.min(text.len()) - OMITTED.len();
    loop {
        let mut prefix_end = available / 2;
        while !text.is_char_boundary(prefix_end) {
            prefix_end -= 1;
        }
        let mut suffix_start = text.len() - (available - prefix_end);
        while !text.is_char_boundary(suffix_start) {
            suffix_start += 1;
        }
        let shortened = format!(
            "{}{}{}",
            &text[..prefix_end],
            OMITTED,
            &text[suffix_start..]
        );
        if serde_json::to_vec(&shortened).is_ok_and(|encoded| encoded.len() <= ENCODED_TEXT_BUDGET)
        {
            return shortened;
        }
        available /= 2;
    }
}
fn claude_question_prompt(input: Option<&Value>) -> String {
    input
        .and_then(|input| input.get("questions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| {
            string_value(question, "question").or_else(|| string_value(question, "header"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn claude_question_choices(input: Option<&Value>) -> Vec<String> {
    input
        .and_then(|input| input.get("questions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|question| {
            question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|option| string_value(option, "label"))
        .collect()
}
/// Upper bound, in characters, of any tool summary a connector emits. The
/// contract allows 16384 and the Hub rejects items over 32 KiB; a summary is a
/// one-line description, so it stays far below both.
const MAX_TOOL_SUMMARY_CHARS: usize = 320;
/// Upper bound of the input description or failure detail within a summary.
const MAX_SUMMARY_PART_CHARS: usize = 160;
/// Only this much of a provider string is ever scanned for a summary, so an
/// unexpectedly large record costs a bounded amount of work.
const MAX_SUMMARY_SCAN_BYTES: usize = 4096;
const REDACTED: &str = "[redacted]";

/// A safe, human-readable description of a tool call's input. Only named,
/// descriptive fields are used; raw commands, file contents, and arbitrary
/// input objects are never copied.
fn safe_tool_summary(name: &str, input: Option<&Value>) -> String {
    let field = |key: &str| {
        input
            .and_then(|input| input.get(key))
            .and_then(Value::as_str)
            .map(|value| sanitize_part(value, MAX_SUMMARY_PART_CHARS))
            .filter(|value| !value.is_empty())
    };
    field("description")
        .or_else(|| {
            ["file_path", "notebook_path", "path", "pattern", "url"]
                .into_iter()
                .find_map(field)
        })
        .unwrap_or_else(|| {
            if name == "Bash" {
                "Bash command".to_owned()
            } else {
                String::new()
            }
        })
}

/// The status and summary of a finished Claude tool call, from its
/// `tool_result` block. A successful result is described by its shape only:
/// its content can be a file, a command's output, or anything else the tool
/// read, so it never reaches the summary. A failure carries the first line of
/// its error, sanitized, because that line is what the user needs to know.
fn claude_tool_outcome(input: &str, result: &Value) -> (ToolStatus, String) {
    let failed = result.get("is_error").and_then(Value::as_bool) == Some(true);
    let mut text = String::new();
    let mut images = 0usize;
    match result.get("content") {
        Some(Value::String(content)) => text.push_str(content),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match string_map(block, "type").as_deref() {
                    Some("text") => {
                        if let Some(part) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(part);
                        }
                    }
                    Some("image") => images += 1,
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let outcome = if failed {
        let detail = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| sanitize_part(line, MAX_SUMMARY_PART_CHARS))
            .unwrap_or_default();
        if detail.is_empty() {
            "failed".to_owned()
        } else {
            format!("failed: {detail}")
        }
    } else {
        let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
        match (lines, images) {
            (0, 0) => "no output".to_owned(),
            (0, 1) => "returned an image".to_owned(),
            (0, n) => format!("returned {n} images"),
            (1, _) => "returned 1 line".to_owned(),
            (n, _) => format!("returned {n} lines"),
        }
    };
    let summary = if input.is_empty() {
        outcome
    } else {
        format!("{input} · {outcome}")
    };
    let status = if failed {
        ToolStatus::Failed
    } else {
        ToolStatus::Succeeded
    };
    (status, sanitize_summary(&summary))
}

/// Bounds and sanitizes a complete tool summary.
fn sanitize_summary(text: &str) -> String {
    sanitize_part(text, MAX_TOOL_SUMMARY_CHARS)
}

/// Collapses `text` to one line of at most `max_chars` characters with
/// secret-shaped tokens, environment assignments, and home directories
/// redacted. Idempotent, so an already-sanitized part may be sanitized again.
fn sanitize_part(text: &str, max_chars: usize) -> String {
    let mut end = text.len().min(MAX_SUMMARY_SCAN_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::new();
    let mut redact_next = false;
    for token in text[..end].split(|c: char| c.is_whitespace() || c.is_control()) {
        if token.is_empty() {
            continue;
        }
        let token = if redact_next {
            REDACTED.to_owned()
        } else {
            redact_token(token)
        };
        redact_next = token.eq_ignore_ascii_case("bearer") || token.eq_ignore_ascii_case("basic");
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&token);
    }
    if out.chars().count() > max_chars || end < text.len() {
        let mut bounded: String = out.chars().take(max_chars.saturating_sub(1)).collect();
        bounded.push('…');
        return bounded;
    }
    out
}

fn redact_token(token: &str) -> String {
    let token = redact_home(token);
    if let Some((key, value)) = token.split_once(['=', ':']) {
        let key_name = key.trim_start_matches(['-', '"', '\'', '{', '(']);
        let key_name = key_name.trim_end_matches(['"', '\'']);
        if !value.is_empty()
            && value != REDACTED
            && (is_env_name(key_name) || is_secret_key(key_name))
        {
            let separator = &token[key.len()..key.len() + 1];
            return format!("{key}{separator}{REDACTED}");
        }
    }
    let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if is_secret_shaped(bare) {
        return token.replace(bare, REDACTED);
    }
    token
}

/// `/Users/<name>` and `/home/<name>` become `~`, so paths keep their useful
/// tail without naming the account that owns them.
fn redact_home(token: &str) -> String {
    let mut out = token.to_owned();
    for root in ["/Users/", "/home/"] {
        while let Some(start) = out.find(root) {
            let rest = &out[start + root.len()..];
            let user_len = rest.find('/').unwrap_or(rest.len());
            if user_len == 0 {
                break;
            }
            out.replace_range(start..start + root.len() + user_len, "~");
        }
    }
    out
}

/// `NAME=value` with an upper-case shell-variable name.
fn is_env_name(key: &str) -> bool {
    let key = key.strip_prefix('$').unwrap_or(key);
    key.len() >= 2
        && key.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "authorization",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

/// Provider credentials with a recognizable prefix, and long opaque strings
/// that are more likely to be a key than anything a person would read.
fn is_secret_shaped(token: &str) -> bool {
    const PREFIXES: [&str; 13] = [
        "sk-",
        "sk_live_",
        "sk_test_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "glpat-",
        "AKIA",
        "eyJ",
    ];
    if token.len() >= 16 && PREFIXES.iter().any(|prefix| token.starts_with(prefix)) {
        return true;
    }
    token.len() >= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '=' | '.'))
        && token.chars().any(|c| c.is_ascii_digit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}
fn is_empty_composer(connector: &str, line: &str) -> bool {
    if connector == "cursor" {
        return cursor::is_empty_composer(line);
    }
    let line = line.trim_start();
    let markers: &[char] = if connector == "claude" {
        &['❯']
    } else {
        &['›', '>']
    };
    markers.iter().any(|marker| {
        line.strip_prefix(*marker).is_some_and(|rest| {
            let rest = rest.trim();
            rest.is_empty() || (connector == "codex" && rest == "Ask Codex to do anything")
        })
    })
}
fn screen_contains_request(screen: &str, request: &PendingRequest) -> bool {
    let screen = screen.to_lowercase();
    let prompt = request.prompt.to_lowercase();
    (prompt.len() >= 4 && screen.contains(&prompt))
        || request
            .choices
            .iter()
            .any(|choice| choice.len() >= 2 && screen.contains(&choice.to_lowercase()))
}
fn visible_choices_with_keys(screen: &str, prompt: &str) -> Vec<(String, String)> {
    let lines: Vec<_> = screen.lines().collect();
    let prompt = prompt.to_lowercase();
    let Some(prompt_line) = lines
        .iter()
        .rposition(|line| prompt.len() >= 4 && line.to_lowercase().contains(&prompt))
    else {
        return Vec::new();
    };
    lines[prompt_line + 1..]
        .iter()
        .copied()
        .filter_map(|line| {
            let line = line
                .trim_start()
                .trim_start_matches(['❯', '>'])
                .trim_start();
            let (number, label) = line.split_once('.')?;
            let number = number.trim();
            let label = label.trim();
            (!label.is_empty()
                && matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
            .then(|| (number.to_owned(), label.to_owned()))
        })
        .collect()
}
fn visible_choices(screen: &str, prompt: &str) -> Vec<String> {
    visible_choices_with_keys(screen, prompt)
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}
fn visible_choice_key(screen: &str, prompt: &str, choice: &str) -> Option<String> {
    visible_choices_with_keys(screen, prompt)
        .into_iter()
        .find_map(|(key, label)| label.eq_ignore_ascii_case(choice).then_some(key))
}

impl Connector for JsonlConnector {
    fn detect(&self) -> Detection {
        // The agent itself is supported even before it emits SessionStart. The
        // first poll advertises `starting` until its source binding arrives.
        Detection::Supported(self.identity())
    }

    fn wait_for_activity(
        &mut self,
        fallback_poll: Duration,
        event_timeout: Duration,
    ) -> Result<()> {
        if !self.live_screen {
            std::thread::sleep(fallback_poll);
            return Ok(());
        }
        let wake = self
            .control()?
            .wait_for_activity(fallback_poll, event_timeout)?;
        // A timeout still performs bounded source catch-up, covering hooks or
        // transcript writes that did not coincide with terminal output. It
        // does not take a screen snapshot. Every actual event and every event
        // stream reconnect does, which is the resynchronization boundary.
        if !matches!(wake, ConversationWake::Timeout) {
            self.refresh_screen = true;
        }
        Ok(())
    }

    fn poll(&mut self, budget: PollBudget) -> Result<PollResult> {
        let binding_replaced = self.refresh_binding();
        let runtime_before = self.runtime_checkpoint();
        #[cfg(test)]
        {
            self.last_read_bytes = 0;
        }
        let mut mutations = Vec::new();
        if binding_replaced {
            mutations.push(ConnectorMutation::Rebuild {
                reason: "authoritative source binding changed".to_owned(),
            });
        }
        let mut delta = CheckpointDelta {
            source_offsets: Vec::new(),
            active_branch_delta: Vec::new(),
            connector_state: None,
        };
        // Hooks are an independent append-only source. They carry the
        // authoritative SessionStart binding and out-of-band permissions, so
        // never fold them into the transcript offset or re-read either source
        // after an unrelated append.
        if matches!(self.id, "claude" | "cursor") {
            let hooks = self.home.session(&self.session).conversation_source_hooks();
            let hook_length = fs::metadata(&hooks).map(|meta| meta.len()).unwrap_or(0);
            if hook_length < self.hook_offset {
                self.hook_offset = 0;
            }
            if hook_length > self.hook_offset {
                let mut file = File::open(&hooks)
                    .with_context(|| format!("open Claude hook sidecar {}", hooks.display()))?;
                file.seek(SeekFrom::Start(self.hook_offset))?;
                let mut bytes = Vec::new();
                file.take(MAX_READ_BYTES as u64).read_to_end(&mut bytes)?;
                #[cfg(test)]
                {
                    self.last_read_bytes += bytes.len();
                }
                let complete = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let mut consumed = 0usize;
                for line in bytes[..complete]
                    .split_inclusive(|byte| *byte == b'\n')
                    .take(budget.max_records)
                {
                    consumed += line.len();
                    if line.len() > MAX_RECORD_BYTES {
                        self.malformed_records += 1;
                        continue;
                    }
                    match serde_json::from_slice::<Value>(line) {
                        Ok(value) => {
                            mutations.extend(self.record(value, self.hook_offset + consumed as u64))
                        }
                        Err(_) => self.malformed_records += 1,
                    }
                }
                self.hook_offset += consumed as u64;
                delta.source_offsets.push(super::super::SourceOffset {
                    source: hooks.display().to_string(),
                    offset: self.hook_offset,
                });
            }
        }
        if let Some(source) = self.source.clone() {
            let metadata = fs::metadata(&source).ok();
            let identity = metadata.as_ref().map(|metadata| SourceIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            });
            if self.source_identity.is_some()
                && identity.is_some()
                && self.source_identity != identity
            {
                self.offset = 0;
                self.forget_chain();
                self.pending_request = None;
                self.tools.clear();
                self.tool_summaries.clear();
                self.tool_running = false;
                self.turn_open = false;
                mutations.push(ConnectorMutation::Rebuild {
                    reason: "authoritative source file was replaced".to_owned(),
                });
            }
            self.source_identity = identity;
            let length = metadata.map(|meta| meta.len()).unwrap_or(0);
            if length < self.offset {
                self.offset = 0;
                self.forget_chain();
                mutations.push(ConnectorMutation::Rebuild {
                    reason: "authoritative source was truncated".to_owned(),
                });
            }
            if length > self.offset {
                let mut file = File::open(&source)
                    .with_context(|| format!("open authoritative {} source", self.id))?;
                file.seek(SeekFrom::Start(self.offset))?;
                let mut bytes = Vec::new();
                file.take(MAX_READ_BYTES as u64).read_to_end(&mut bytes)?;
                #[cfg(test)]
                {
                    self.last_read_bytes += bytes.len();
                }
                let complete = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(0);
                if complete == 0 && bytes.len() == MAX_READ_BYTES {
                    // A source line can never consume the worker indefinitely.
                    // Skip this bounded malformed fragment and let the next poll
                    // continue after it instead of wedging the conversation.
                    self.offset += bytes.len() as u64;
                    self.malformed_records += 1;
                    delta.source_offsets.push(super::super::SourceOffset {
                        source: source.display().to_string(),
                        offset: self.offset,
                    });
                }
                let mut consumed = 0usize;
                for line in bytes[..complete]
                    .split_inclusive(|byte| *byte == b'\n')
                    .take(budget.max_records)
                {
                    consumed += line.len();
                    if line.len() > MAX_RECORD_BYTES {
                        self.malformed_records += 1;
                        continue;
                    }
                    match serde_json::from_slice::<Value>(line) {
                        Ok(value) => {
                            let before = self.active_chain.len();
                            let before_tail = self.active_chain.last().cloned();
                            let record_mutations =
                                self.record(value, self.offset + consumed as u64);
                            for mutation in record_mutations {
                                mutations.push(mutation);
                            }
                            if self.active_chain.len() > before
                                || self.active_chain.last().cloned() != before_tail
                            {
                                // Journal where the record landed in the chain,
                                // not its raw parent: a parallel tool result's
                                // parent is an earlier record it never rewound to.
                                if let Some(source_id) = self.active_chain.last().cloned() {
                                    let parent_id = (self.id == "claude")
                                        .then(|| self.active_chain.iter().rev().nth(1).cloned())
                                        .flatten();
                                    let item_id = self.chain_items.get(&source_id).cloned();
                                    delta.active_branch_delta.push(super::super::BranchEntry {
                                        source_id,
                                        parent_id,
                                        item_id,
                                    });
                                }
                            }
                        }
                        Err(_) => self.malformed_records += 1,
                    }
                }
                self.offset += consumed as u64;
                delta.source_offsets.push(super::super::SourceOffset {
                    source: source.display().to_string(),
                    offset: self.offset,
                });
            }
        }
        let event_driven = self
            .control
            .as_ref()
            .is_some_and(ConversationControl::is_event_driven);
        let refresh_screen = self.live_screen
            && (self.source.is_some() || matches!(self.id, "codex" | "cursor"))
            && if event_driven {
                self.refresh_screen
            } else {
                !delta.source_offsets.is_empty()
                    || self
                        .last_screen_refresh
                        .map(|last| last.elapsed() >= Duration::from_millis(1_500))
                        .unwrap_or(true)
            };
        if refresh_screen {
            let screen = self.current_screen(budget.deadline)?;
            mutations.extend(self.observe_screen(&screen));
            self.last_screen_refresh = Some(Instant::now());
            self.refresh_screen = false;
        }
        let state = self.state();
        if self.last_state.as_ref() != Some(&state) {
            self.last_state = Some(state.clone());
            mutations.push(ConnectorMutation::State(state));
        }
        let runtime_after = self.runtime_checkpoint();
        if runtime_after != runtime_before {
            delta.connector_state = Some(serde_json::to_vec(&runtime_after)?);
        }
        Ok(PollResult {
            mutations,
            checkpoint_delta: delta,
        })
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        // Descriptors remain static because the Hub caches them when it creates
        // the actor. `apply` repeats the live validation immediately before
        // touching the kernel; pushed `ConversationState` is the UI availability.
        vec![
            ActionDescriptor {
                id: super::super::ACTION_CANCEL_TURN.to_owned(),
                required_grant: Grant::Interact,
                enabled: true,
                reason: None,
            },
            ActionDescriptor {
                id: ACTION_SEND_MESSAGE.to_owned(),
                required_grant: Grant::Interact,
                enabled: true,
                reason: None,
            },
            ActionDescriptor {
                id: ACTION_RESOLVE_REQUEST.to_owned(),
                required_grant: Grant::Interact,
                enabled: true,
                reason: None,
            },
        ]
    }

    fn apply(&mut self, action: ConnectorAction, deadline: Duration) -> Result<ApplyResult> {
        if action.id == super::super::ACTION_CANCEL_TURN {
            if !self.state().cancel_turn.enabled {
                return Ok(ApplyResult::Refused {
                    reason: "no running turn with a live bridge".into(),
                });
            }
            return Ok(
                match self.ask_bridge("abort_turn", serde_json::Map::new(), deadline)? {
                    BridgeAnswer::Accepted => ApplyResult::Accepted { correlation: None },
                    BridgeAnswer::Refused(reason) => ApplyResult::Refused { reason },
                    BridgeAnswer::NotTaken => {
                        self.bridge_version = None;
                        ApplyResult::Refused {
                            reason: "the bridge is no longer live".into(),
                        }
                    }
                    BridgeAnswer::Queued => ApplyResult::Refused {
                        reason: "the bridge did not cancel the turn".into(),
                    },
                },
            );
        }
        let text = match action.id.as_str() {
            ACTION_SEND_MESSAGE
                if (self.source.is_some() || matches!(self.id, "codex" | "cursor"))
                    && self.pending_request.is_none()
                    && (self.bridge_version.is_some()
                        || (!self.tool_running && !self.turn_open)) =>
            {
                action.payload.get("text").and_then(Value::as_str)
            }
            ACTION_RESOLVE_REQUEST
                if self.source.is_some()
                    && self
                        .pending_request
                        .as_ref()
                        .map(|request| request.id.as_str())
                        == action
                            .payload
                            .get("requestId")
                            .or_else(|| action.payload.get("request_id"))
                            .and_then(Value::as_str) =>
            {
                action.payload.get("choice").and_then(Value::as_str)
            }
            _ => {
                return Ok(ApplyResult::Refused {
                    reason: "action is not currently available".to_owned(),
                })
            }
        };
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            return Ok(ApplyResult::Refused {
                reason: "action payload is empty".to_owned(),
            });
        };
        // Pushed state can become stale while the operation is in flight, so
        // validate the live session immediately before affecting the kernel.
        let started = Instant::now();
        let remaining = || {
            deadline
                .checked_sub(started.elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| anyhow::anyhow!("connector action deadline exceeded"))
        };
        if self.id == "claude" && self.bridge_version.is_some() {
            let mut payload = serde_json::Map::new();
            let command = if action.id == ACTION_SEND_MESSAGE {
                // A slash command is typed: the engine runs one only from its
                // own prompt box, and reads a submitted `/name` as plain text.
                (!text.trim_start().starts_with('/')).then(|| {
                    payload.insert("text".to_owned(), Value::from(text));
                    "submit_prompt"
                })
            } else {
                // The bridge answers a question by its call id, so the answer
                // cannot land on another prompt and need not be one of the
                // offered labels: free text and comma-joined multi-select
                // answers are the tool's own. Several questions in one call
                // share a flattened prompt here and stay on the terminal.
                self.pending_request
                    .as_ref()
                    .filter(|request| {
                        request.request_type == RequestType::Question
                            && !request.prompt.is_empty()
                            && !request.prompt.contains('\n')
                    })
                    .map(|request| {
                        payload.insert("tool_use_id".to_owned(), Value::from(request.id.clone()));
                        payload.insert(
                            "answers".to_owned(),
                            serde_json::json!({ request.prompt.clone(): text }),
                        );
                        "answer_question"
                    })
            };
            if let Some(kind) = command {
                match self.ask_bridge(kind, payload, remaining()?)? {
                    BridgeAnswer::Queued => return Ok(ApplyResult::Queued { correlation: None }),
                    BridgeAnswer::Accepted => {
                        if action.id == ACTION_RESOLVE_REQUEST {
                            self.pending_request = None;
                        }
                        self.screen_can_send = Some(false);
                        self.last_screen_refresh = None;
                        return Ok(ApplyResult::Accepted { correlation: None });
                    }
                    BridgeAnswer::Refused(reason) => return Ok(ApplyResult::Refused { reason }),
                    // Nothing reached the agent, so the terminal is still safe.
                    BridgeAnswer::NotTaken => self.bridge_version = None,
                }
            }
        }
        if action.id == ACTION_SEND_MESSAGE && (self.tool_running || self.turn_open) {
            return Ok(ApplyResult::Refused {
                reason: "agent is working; terminal sending is unavailable".into(),
            });
        }
        let screen = self.current_screen(remaining()?)?;
        if action.id == ACTION_SEND_MESSAGE {
            if !screen.lines().any(|line| is_empty_composer(self.id, line)) {
                return Ok(ApplyResult::Refused {
                    reason: format!("the {} composer is no longer empty", self.id),
                });
            }
            if self.id == "cursor" {
                // Cursor coalesces an Enter arriving in the same input batch
                // as bracketed paste into the paste. Give its input handler
                // a separate event-loop turn before sending the submit key.
                let settle = Duration::from_millis(100);
                if remaining()? <= settle {
                    return Ok(ApplyResult::Refused {
                        reason: "not enough time to submit to Cursor".to_owned(),
                    });
                }
                self.control()?.paste(text, remaining()?)?;
                std::thread::sleep(settle);
                self.control()?.key(&["Enter".to_owned()], remaining()?)?;
                self.turn_open = true;
            } else {
                self.control()?.submit(text, remaining()?)?;
            }
        } else {
            let request = self.pending_request.as_ref().expect("checked above");
            if !request
                .choices
                .iter()
                .any(|choice| choice.eq_ignore_ascii_case(text))
            {
                return Ok(ApplyResult::Refused {
                    reason:
                        "the selected decision is not among the choices currently offered by Claude"
                            .to_owned(),
                });
            }
            if !screen_contains_request(&screen, request) {
                return Ok(ApplyResult::Refused {
                    reason: "the requested Claude prompt is no longer visible".to_owned(),
                });
            }
            let Some(key) = visible_choice_key(&screen, &request.prompt, text) else {
                return Ok(ApplyResult::Refused {
                    reason: "the selected decision is no longer identifiable on the current Claude prompt"
                        .to_owned(),
                });
            };
            self.control()?.key(&[key], remaining()?)?;
            self.pending_request = None;
        }
        self.screen_can_send = Some(false);
        self.last_screen_refresh = None;
        Ok(ApplyResult::Accepted { correlation: None })
    }

    fn reconcile(
        &self,
        _outstanding: &[ConversationItemId],
        _observed: &[ConversationItemId],
    ) -> Vec<ConnectorMutation> {
        Vec::new()
    }

    fn restore_checkpoint(&mut self, checkpoint: &[u8]) -> Result<()> {
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

    fn apply_checkpoint_delta(&mut self, delta: &CheckpointDelta) -> Result<()> {
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

    fn checkpoint_snapshot(&self) -> Result<Vec<u8>> {
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

fn read_binding(
    home: &LatchHome,
    session: &SessionId,
    connector: &str,
) -> Option<(PathBuf, Option<String>)> {
    let value: Value = serde_json::from_slice(
        &fs::read(home.session(session).conversation_source_binding()).ok()?,
    )
    .ok()?;
    (value.get("connector")?.as_str()? == connector)
        .then(|| value.get("source")?.as_str().map(PathBuf::from))
        .flatten()
        .filter(|path| path.is_absolute())
        .map(|path| {
            (
                path,
                value
                    .get("agentSessionId")
                    .or_else(|| value.get("session_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        })
}

fn string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}
/// How long the bridge module has to take a queued command. It looks twice a
/// second, so a command still waiting after this long has no one to take it.
const BRIDGE_CLAIM_TIMEOUT: Duration = Duration::from_millis(2_500);

enum BridgeAnswer {
    Accepted,
    Queued,
    Refused(String),
    /// The module never took the command, and it has been withdrawn.
    NotTaken,
}

/// The bridge module's answer to one command, read from the hook sidecar
/// without disturbing the observation connector's own offset into it.
fn bridge_command_result(
    sidecar: &std::path::Path,
    from: u64,
    id: &str,
) -> Option<serde_json::Map<String, Value>> {
    let mut file = File::open(sidecar).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter_map(|value| match value {
            Value::Object(object) => Some(object),
            _ => None,
        })
        .find(|object| {
            string(object, "bridge_event").as_deref() == Some("command.result")
                && string(object, "command_id").as_deref() == Some(id)
        })
}

/// Whether a turn close stamped `close` covers a prompt stamped `prompt`.
/// Bridge records carry milliseconds, like the transcript; the `Stop` hook
/// carries whole seconds, where only a strictly earlier second is certain.
fn turn_closed_at_or_after(close: &str, prompt: &str) -> bool {
    if close.len() > 20 && prompt.len() > 20 {
        close >= prompt
    } else {
        close.get(..19).unwrap_or(close) > prompt.get(..19).unwrap_or(prompt)
    }
}

fn timestamp_is_after(timestamp: &str, reference: &str) -> bool {
    // Permission hooks carry whole-second UTC timestamps while Claude JSONL
    // records normally include fractions. Comparing their raw strings would
    // order `.123Z` before `Z`; keeping whole-second precision avoids treating
    // the same observed prompt as later transcript progress.
    timestamp.get(..19).unwrap_or(timestamp) > reference.get(..19).unwrap_or(reference)
}
fn record_id(object: &serde_json::Map<String, Value>, event: &str) -> Option<String> {
    match event {
        "tool_call" | "tool_result" => {
            string(object, "call_id").or_else(|| string(object, "tool_use_id"))
        }
        "tool_use" => string(object, "tool_use_id"),
        "approval_request" | "permission_request" | "question_request" | "request_resolved" => {
            string(object, "request_id")
        }
        _ => string(object, "id")
            .or_else(|| string(object, "uuid"))
            .or_else(|| string(object, "source_id")),
    }
}
fn tool_status(object: &serde_json::Map<String, Value>) -> ToolStatus {
    match string(object, "state")
        .or_else(|| string(object, "status"))
        .as_deref()
    {
        Some("completed" | "succeeded") => ToolStatus::Succeeded,
        Some("failed") => ToolStatus::Failed,
        _ => ToolStatus::Running,
    }
}
fn message_status(object: &serde_json::Map<String, Value>) -> MessageStatus {
    match string(object, "state")
        .or_else(|| string(object, "status"))
        .as_deref()
    {
        Some("partial") => MessageStatus::Partial,
        Some("failed") => MessageStatus::Failed,
        _ => MessageStatus::Complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_recognized_harness_markers_select_a_connector() {
        assert_eq!(connector_kind(Some("claude")), Some("claude"));
        assert_eq!(connector_kind(Some("codex")), Some("codex"));
        assert_eq!(connector_kind(Some("cursor")), Some("cursor"));
        assert_eq!(connector_kind(Some("bash")), None);
        assert_eq!(connector_kind(None), None);
    }

    #[test]
    fn restoring_a_checkpoint_adopts_a_binding_written_after_the_connector_was_built() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionId::parse("ses_fixture").unwrap();
        fs::create_dir_all(LatchHome::new(dir.path()).session(&session).dir()).unwrap();
        let mut connector = JsonlConnector::fixture("claude", PathBuf::from("unused.jsonl"));
        connector.home = LatchHome::new(dir.path());
        connector.source = None;
        connector.agent_session_id = None;

        // Watched before the agent started: no binding yet, nothing adopted.
        connector.restore_checkpoint(&[]).unwrap();
        assert!(connector.source.is_none());
        assert_eq!(connector.state().phase, ConversationPhase::Starting);

        let source = dir.path().join("transcript.jsonl");
        fs::write(
            connector
                .home
                .session(&session)
                .conversation_source_binding(),
            serde_json::json!({
                "connector": "claude",
                "source": source,
                "agentSessionId": "agent-1",
            })
            .to_string(),
        )
        .unwrap();

        // The next action restores a checkpoint first; that is where the
        // binding written since must be picked up.
        connector.restore_checkpoint(&[]).unwrap();
        assert_eq!(connector.source.as_deref(), Some(source.as_path()));
        assert_eq!(connector.agent_session_id.as_deref(), Some("agent-1"));
        assert_ne!(connector.state().phase, ConversationPhase::Starting);

        // A binding for another connector is not adopted.
        let mut other = JsonlConnector::fixture("codex", PathBuf::from("unused.jsonl"));
        other.home = LatchHome::new(dir.path());
        other.source = None;
        other.restore_checkpoint(&[]).unwrap();
        assert!(other.source.is_none());
    }

    fn corpus(agent: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/conversation")
            .join(agent)
            .join("source-corpus.jsonl")
    }

    fn claude_cases_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/conversation/claude/cases")
    }

    fn poll_all(connector: &mut JsonlConnector) -> Vec<ConnectorMutation> {
        let budget = PollBudget {
            max_records: 512,
            deadline: std::time::Duration::from_secs(5),
        };
        let mut mutations = Vec::new();
        loop {
            let result = connector.poll(budget.clone()).unwrap();
            if result.mutations.is_empty() {
                break;
            }
            mutations.extend(result.mutations);
        }
        mutations
    }

    fn wire_status_message(status: &MessageStatus) -> &'static str {
        match status {
            MessageStatus::Submitted => "submitted",
            MessageStatus::Queued => "queued",
            MessageStatus::Observed => "observed",
            MessageStatus::Partial => "partial",
            MessageStatus::Complete => "complete",
            MessageStatus::Failed => "failed",
        }
    }

    fn wire_item(item: &super::super::super::ConversationItem) -> Value {
        serde_json::json!({
            "id": item.id.as_str(),
            "ordinal": item.ordinal.get(),
            "createdAt": item.created_at,
            "kind": match &item.kind {
                ConversationItemKind::Message { role, text, status } => serde_json::json!({
                    "type": "message",
                    "role": match role {
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    },
                    "text": text,
                    "status": wire_status_message(status),
                }),
                ConversationItemKind::Tool {
                    name,
                    summary,
                    status,
                    parent_message_id,
                } => {
                    let mut kind = serde_json::json!({
                        "type": "tool",
                        "name": name,
                        "summary": summary,
                        "status": match status {
                            ToolStatus::Running => "running",
                            ToolStatus::Succeeded => "succeeded",
                            ToolStatus::Failed => "failed",
                        },
                    });
                    if let Some(parent) = parent_message_id {
                        kind["parentMessageId"] = Value::String(parent.as_str().to_owned());
                    }
                    kind
                }
                ConversationItemKind::Request {
                    request_id,
                    request_type,
                    prompt,
                    choices,
                    status,
                } => serde_json::json!({
                    "type": "request",
                    "requestId": request_id,
                    "requestType": match request_type {
                        RequestType::Permission => "permission",
                        RequestType::Question => "question",
                    },
                    "prompt": prompt,
                    "choices": choices,
                    "status": match status {
                        RequestStatus::Pending => "pending",
                        RequestStatus::Resolved => "resolved",
                        RequestStatus::Dismissed => "dismissed",
                    },
                }),
            },
        })
    }

    fn wire_availability(availability: &super::super::super::Availability) -> Value {
        let mut value = serde_json::json!({ "enabled": availability.enabled });
        if let Some(reason) = &availability.reason {
            value["reason"] = Value::String(reason.clone());
        }
        value
    }

    fn wire_state(state: &ConversationState) -> Value {
        serde_json::json!({
            "phase": match state.phase {
                ConversationPhase::Starting => "starting",
                ConversationPhase::Idle => "idle",
                ConversationPhase::Working => "working",
                ConversationPhase::AwaitingInput => "awaiting_input",
                ConversationPhase::Exited => "exited",
                ConversationPhase::Unavailable => "unavailable",
            },
            "sendMessage": wire_availability(&state.send_message),
            "resolveRequest": wire_availability(&state.resolve_request),
            "pendingRequest": state.pending_request,
            "connector": state.connector.as_ref().map(|connector| serde_json::json!({
                "id": connector.id,
                "version": connector.version,
            })),
        })
    }

    fn project_case(source: PathBuf) -> (Vec<ConnectorMutation>, super::super::super::Projection) {
        let mut connector = JsonlConnector::fixture("claude", source);
        assert!(matches!(connector.detect(), Detection::Supported(_)));
        let mutations = poll_all(&mut connector);
        let mut projection = super::super::super::Projection::new(
            super::super::super::OperationEpoch::new("fixture"),
            ConversationState::starting(Some(connector.identity())),
        );
        for mutation in &mutations {
            // A rewind names the nearest record that minted an item, so a
            // truncation the projection cannot place is a connector defect.
            if let Err(error) = projection.apply_connector(mutation.clone()) {
                panic!("projecting {} failed: {error}", connector.id);
            }
        }
        assert!(connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1)
            })
            .unwrap()
            .mutations
            .is_empty());
        (mutations, projection)
    }

    fn expected_document(
        case: &str,
        mutations: &[ConnectorMutation],
        projection: &super::super::super::Projection,
    ) -> Value {
        let projected_item_count = projection.snapshot(usize::MAX).items.len();
        let (limit, max_bytes) = if case == "long-transcript" {
            (300, 512 * 1024)
        } else {
            (usize::MAX, usize::MAX)
        };
        let snapshot = projection.snapshot_bounded(limit, max_bytes);
        let truncate_after: Vec<String> = mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ConnectorMutation::TruncateAfter(id) => Some(id.as_str().to_owned()),
                _ => None,
            })
            .collect();
        serde_json::json!({
            "snapshot": {
                "generation": snapshot.generation.as_wire(),
                "revision": snapshot.revision.get(),
                "operationEpoch": snapshot.operation_epoch.as_str(),
                "items": snapshot.items.iter().map(wire_item).collect::<Vec<_>>(),
                "state": wire_state(&snapshot.state),
                "hasMoreBefore": snapshot.has_more_before,
                "reason": "initial",
            },
            "projectedItemCount": projected_item_count,
            "truncateAfter": truncate_after,
        })
    }

    fn claude_case_ids() -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(claude_cases_dir())
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry
                    .file_type()
                    .ok()?
                    .is_dir()
                    .then(|| entry.file_name().to_str().unwrap_or_default().to_owned())
            })
            .filter(|name| !name.starts_with('.'))
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn claude_cases_match_checked_in_projections() {
        let update = std::env::var_os("UPDATE_CONVERSATION_FIXTURES").is_some();
        let ids = claude_case_ids();
        assert!(
            !ids.is_empty(),
            "expected captured Claude cases under fixtures/conversation/claude/cases"
        );
        for id in &ids {
            let dir = claude_cases_dir().join(id);
            let source = dir.join("source.jsonl");
            let expected_path = dir.join("expected.json");
            let (mutations, projection) = project_case(source);
            let actual = expected_document(id, &mutations, &projection);
            if update {
                fs::write(&expected_path, serde_json::to_vec_pretty(&actual).unwrap()).unwrap();
            }
            let expected: Value =
                serde_json::from_slice(&fs::read(&expected_path).unwrap_or_else(|_| {
                    panic!(
                        "{}: missing expected.json (run with UPDATE_CONVERSATION_FIXTURES=1)",
                        id
                    )
                }))
                .unwrap();
            assert_eq!(
                actual, expected,
                "{id} projection drifted from expected.json"
            );
        }
    }

    #[test]
    fn claude_case_corpus_covers_required_shapes() {
        let cases = claude_cases_dir();
        let markdown = fs::read_to_string(cases.join("markdown-prose/expected.json")).unwrap();
        assert!(
            markdown.contains("\\n\\n"),
            "markdown-prose must retain multi-paragraph assistant text"
        );

        let fenced: Value =
            serde_json::from_slice(&fs::read(cases.join("fenced-code/expected.json")).unwrap())
                .unwrap();
        let fenced_text = fenced["snapshot"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["kind"]["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(fenced_text.contains("```"), "fenced-code must keep fences");
        assert!(
            fenced_text.lines().any(|line| line.len() > 120),
            "fenced-code must keep a line long enough to scroll horizontally"
        );

        let multi_source = fs::read_to_string(cases.join("multi-tool-turn/source.jsonl")).unwrap();
        assert_eq!(
            multi_source.matches("\"type\":\"tool_use\"").count(),
            3,
            "multi-tool-turn source must keep the three tool_use blocks from one assistant record"
        );
        let multi: Value =
            serde_json::from_slice(&fs::read(cases.join("multi-tool-turn/expected.json")).unwrap())
                .unwrap();
        // Sibling tool_result records all parent the assistant. They are
        // attached to the branch, so all three calls stay in the projection.
        assert!(
            multi["truncateAfter"].as_array().unwrap().is_empty(),
            "parallel tool_result records must not rewind the branch"
        );
        let tools = multi["snapshot"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"]["type"] == "tool")
            .count();
        assert_eq!(tools, 3, "every call of the parallel batch stays visible");

        let failed_source = fs::read_to_string(cases.join("failed-tool/source.jsonl")).unwrap();
        assert!(
            failed_source.contains("\"is_error\":true"),
            "failed-tool source must retain the provider error flag"
        );
        let failed: Value =
            serde_json::from_slice(&fs::read(cases.join("failed-tool/expected.json")).unwrap())
                .unwrap();
        assert!(
            failed["snapshot"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"]["type"] == "tool"
                    && item["kind"]["status"] == "failed"
                    && item["kind"]["summary"]
                        .as_str()
                        .unwrap()
                        .ends_with("failed: Exit code 1")),
            "the is_error tool_result must project as a failed tool carrying its error"
        );

        let permission: Value = serde_json::from_slice(
            &fs::read(cases.join("permission-request/expected.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            permission["snapshot"]["state"]["pendingRequest"],
            "ddc8b841-342d-431b-84a0-076fb535b263"
        );
        assert!(permission["snapshot"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"]["type"] == "request"
                && item["kind"]["requestType"] == "permission"));

        let question: Value = serde_json::from_slice(
            &fs::read(cases.join("ask-user-question/expected.json")).unwrap(),
        )
        .unwrap();
        let request = question["snapshot"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"]["type"] == "request")
            .expect("AskUserQuestion must become a request item");
        assert_eq!(request["kind"]["requestType"], "question");
        assert!(
            request["kind"]["choices"].as_array().unwrap().len() >= 6,
            "option labels from both questions must survive flattening"
        );
        assert!(
            request["kind"]["prompt"].as_str().unwrap().contains('\n'),
            "multiple question prompts are joined with newlines"
        );

        let truncation: Value = serde_json::from_slice(
            &fs::read(cases.join("branch-truncation/expected.json")).unwrap(),
        )
        .unwrap();
        // The capture is a parallel tool batch: a result whose parent is an
        // earlier assistant record. It is a sibling of the branch, so nothing
        // is truncated and every call of the batch stays visible.
        assert!(
            truncation["truncateAfter"].as_array().unwrap().is_empty(),
            "a parallel tool result must not truncate the branch"
        );
        assert_eq!(
            truncation["snapshot"]["items"].as_array().unwrap().len(),
            4,
            "every call of the parallel batch stays visible"
        );

        let interruption = fs::read_to_string(cases.join("interruption/source.jsonl")).unwrap();
        assert!(
            interruption.contains("[Request interrupted by user for tool use]"),
            "interruption source must keep Claude's interrupt marker"
        );

        let long: Value =
            serde_json::from_slice(&fs::read(cases.join("long-transcript/expected.json")).unwrap())
                .unwrap();
        let published = long["snapshot"]["items"].as_array().unwrap().len();
        let projected = long["projectedItemCount"].as_u64().unwrap();
        assert!(
            projected > 300,
            "long-transcript source must project more than the store window, got {projected}"
        );
        assert!(
            published <= 300,
            "long-transcript snapshot is the store window, got {published}"
        );
        assert!(
            long["snapshot"]["hasMoreBefore"].as_bool().unwrap(),
            "a windowed long transcript must report earlier items exist"
        );
    }

    fn conformance(agent: &'static str) {
        let mut connector = JsonlConnector::fixture(agent, corpus(agent));
        assert!(matches!(connector.detect(), Detection::Supported(_)));
        let result = connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1),
            })
            .unwrap();
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Message {
                    role: MessageRole::User,
                    ..
                },
                ..
            })
        )));
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Tool {
                    status: ToolStatus::Succeeded,
                    ..
                },
                ..
            })
        )));
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    status: RequestStatus::Pending,
                    ..
                },
                ..
            })
        )));
        assert!(result
            .mutations
            .iter()
            .any(|mutation| matches!(mutation, ConnectorMutation::TruncateAfter(_))));
        assert!(result.checkpoint_delta.source_offsets[0].offset > 0);
        assert!(
            serde_json::from_slice::<Value>(&connector.checkpoint_snapshot().unwrap()).unwrap()
                ["offset"]
                .as_u64()
                .unwrap()
                > 0
        );
        let mut projection = super::super::super::Projection::new(
            super::super::super::OperationEpoch::new("fixture"),
            ConversationState::starting(Some(connector.identity())),
        );
        for mutation in result.mutations {
            projection.apply_connector(mutation).unwrap();
        }
        assert_eq!(projection.state().phase, ConversationPhase::Idle);
        assert!(connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1)
            })
            .unwrap()
            .mutations
            .is_empty());
    }

    #[test]
    fn codex_conforms_to_the_connector_suite() {
        conformance("codex");
    }

    #[test]
    fn codex_rollout_projects_only_completed_conversation_items() {
        let mut connector = JsonlConnector::fixture("codex", PathBuf::from("unused"));
        let user = serde_json::json!({"timestamp":"2026-09-22T12:00:00Z","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"user-1","content":[{"type":"text","text":"hello"}]}}});
        let assistant = serde_json::json!({"timestamp":"2026-09-22T12:00:01Z","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"assistant-1","content":[{"type":"Text","text":"hi"}]}}});
        let setup = serde_json::json!({"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"AGENTS.md secret"}]}});
        let projected = connector.codex_conversation_item(user.as_object().unwrap(), 1);
        assert!(
            matches!(&projected[0], ConnectorMutation::Upsert(item) if matches!(&item.kind, ConversationItemKind::Message { role: MessageRole::User, text, .. } if text == "hello"))
        );
        let projected = connector.codex_conversation_item(assistant.as_object().unwrap(), 2);
        assert!(
            matches!(&projected[0], ConnectorMutation::Upsert(item) if matches!(&item.kind, ConversationItemKind::Message { role: MessageRole::Assistant, text, .. } if text == "hi"))
        );
        assert!(connector.record(setup, 3).is_empty());
    }

    #[test]
    fn codex_startup_placeholder_is_an_empty_composer() {
        assert!(is_empty_composer("codex", "› Ask Codex to do anything"));
        assert!(!is_empty_composer("codex", "› draft"));
    }

    #[test]
    fn codex_first_send_waits_for_a_confirmed_empty_composer() {
        let mut connector = JsonlConnector::fixture("codex", PathBuf::from("unused"));
        connector.source = None;
        assert_eq!(connector.state().phase, ConversationPhase::Starting);
        assert!(!connector.state().send_message.enabled);
        connector.observe_screen("› Ask Codex to do anything");
        assert!(connector.state().send_message.enabled);
        connector.observe_screen("› draft");
        assert!(!connector.state().send_message.enabled);
    }

    #[test]
    fn long_claude_launch_prompt_stays_within_the_hub_item_budget() {
        let mut connector = JsonlConnector::fixture("claude", PathBuf::from("unused"));
        let long_prompt = format!("Start: {} :End", "😀\n".repeat(12_000));
        let record = serde_json::json!({
            "type": "user",
            "uuid": "prompt-1",
            "parentUuid": null,
            "timestamp": "2026-09-25T08:38:00Z",
            "message": { "content": long_prompt },
        });
        let mutations = connector.record(record, 1);
        let ConnectorMutation::Upsert(item) = &mutations[0] else {
            panic!("expected the launch prompt to be visible");
        };
        let ConversationItemKind::Message { text, .. } = &item.kind else {
            panic!("expected a message");
        };
        assert!(text.starts_with("Start: "));
        assert!(text.ends_with(" :End"));
        assert!(text.contains("middle omitted from chat"));
        assert!(text.len() <= MAX_MESSAGE_TEXT_BYTES);
        assert!(serde_json::to_vec(item).unwrap().len() <= MAX_CONVERSATION_ITEM_BYTES);
    }

    #[test]
    fn hundred_thousand_record_claude_append_reads_only_the_new_range() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("claude.jsonl");
        let mut transcript = String::new();
        for index in 0..100_000u32 {
            let parent = if index == 0 {
                "null".to_owned()
            } else {
                format!("\"u{}\"", index - 1)
            };
            transcript.push_str(&format!(
                "{{\"type\":\"user\",\"uuid\":\"u{index}\",\"parentUuid\":{parent},\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{{\"content\":\"m{index}\"}}}}\n"
            ));
        }
        fs::write(&source, transcript).unwrap();
        let mut connector = JsonlConnector::fixture("claude", source.clone());
        let budget = PollBudget {
            max_records: 100_000,
            deadline: std::time::Duration::from_secs(1),
        };
        while connector.offset < fs::metadata(&source).unwrap().len() {
            connector.poll(budget.clone()).unwrap();
        }
        let appended = b"{\"type\":\"assistant\",\"uuid\":\"tail\",\"parentUuid\":\"u99999\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"tail\"}]}}\n";
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(appended)
            .unwrap();
        let result = connector.poll(budget).unwrap();
        assert_eq!(connector.last_read_bytes, appended.len());
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Message { text, .. },
                ..
            }) if text == "tail"
        )));
    }

    #[test]
    fn replacing_a_source_at_the_same_path_rebuilds_before_reading_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("codex.jsonl");
        fs::write(
            &source,
            b"{\"event\":\"user_message\",\"id\":\"old\",\"text\":\"old\"}\n",
        )
        .unwrap();
        let mut connector = JsonlConnector::fixture("codex", source.clone());
        connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1),
            })
            .unwrap();

        let replacement = temp.path().join("replacement.jsonl");
        fs::write(
            &replacement,
            b"{\"event\":\"user_message\",\"id\":\"new\",\"text\":\"new\"}\n",
        )
        .unwrap();
        fs::rename(replacement, &source).unwrap();

        let result = connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1),
            })
            .unwrap();
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Rebuild { reason } if reason.contains("replaced")
        )));
        assert!(result.mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem { id, .. }) if id.as_str() == "new"
        )));
    }

    #[test]
    fn malformed_middle_record_is_counted_and_does_not_wedge_following_records() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("codex.jsonl");
        fs::write(
            &source,
            b"{\"event\":\"user_message\",\"id\":\"one\",\"text\":\"one\"}\n{broken\n{\"event\":\"assistant_message\",\"id\":\"two\",\"text\":\"two\"}\n",
        )
        .unwrap();
        let mut connector = JsonlConnector::fixture("codex", source);
        let result = connector
            .poll(PollBudget {
                max_records: 64,
                deadline: std::time::Duration::from_secs(1),
            })
            .unwrap();
        assert_eq!(connector.malformed_records, 1);
        let ids: Vec<_> = result
            .mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ConnectorMutation::Upsert(item) => Some(item.id.as_str()),
                _ => None,
            })
            .collect();
        assert!(ids.contains(&"one"));
        assert!(ids.contains(&"two"));
    }

    #[test]
    fn runtime_delta_restores_a_pending_request_at_the_advanced_offset() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("codex.jsonl");
        fs::write(
            &source,
            b"{\"event\":\"user_message\",\"id\":\"u1\",\"text\":\"start\"}\n",
        )
        .unwrap();
        let budget = PollBudget {
            max_records: 64,
            deadline: std::time::Duration::from_secs(1),
        };
        let mut connector = JsonlConnector::fixture("codex", source.clone());
        connector.poll(budget.clone()).unwrap();
        let compact = connector.checkpoint_snapshot().unwrap();

        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(
                b"{\"event\":\"approval_request\",\"request_id\":\"r1\",\"prompt\":\"Allow?\",\"choices\":[\"Allow\",\"Deny\"]}\n",
            )
            .unwrap();
        let appended = connector.poll(budget.clone()).unwrap();
        assert!(appended.checkpoint_delta.connector_state.is_some());

        let mut restored = JsonlConnector::fixture("codex", source);
        restored.restore_checkpoint(&compact).unwrap();
        restored
            .apply_checkpoint_delta(&appended.checkpoint_delta)
            .unwrap();
        assert_eq!(
            restored.pending_request.as_ref().map(|r| r.id.as_str()),
            Some("r1")
        );
        assert_eq!(restored.state().phase, ConversationPhase::AwaitingInput);
        assert!(restored.poll(budget).unwrap().mutations.is_empty());
    }

    #[test]
    fn idle_screen_refresh_dismisses_a_prompt_answered_at_the_computer() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("codex.jsonl");
        fs::write(&source, b"").unwrap();
        let mut connector = JsonlConnector::fixture("codex", source);
        connector.pending_request = Some(PendingRequest {
            id: "r1".to_owned(),
            request_type: RequestType::Question,
            prompt: "Choose a mode".to_owned(),
            choices: vec!["Fast".to_owned(), "Careful".to_owned()],
            screen_seen: true,
            announced_at: None,
        });

        let mutations = connector.observe_screen("finished\n› \n");
        assert!(connector.pending_request.is_none());
        assert_eq!(connector.screen_can_send, Some(true));
        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    status: RequestStatus::Dismissed,
                    ..
                },
                ..
            })
        )));
    }

    #[test]
    fn permission_choices_are_replaced_by_the_visible_numbered_decisions() {
        let mut connector = JsonlConnector::fixture("claude", PathBuf::from("unused-source.jsonl"));
        connector.pending_request = Some(PendingRequest {
            id: "permission-1".to_owned(),
            request_type: RequestType::Permission,
            prompt: "Create empty permission marker file".to_owned(),
            choices: Vec::new(),
            screen_seen: false,
            announced_at: None,
        });

        let mutations = connector.observe_screen(
            "Earlier response\n1. Unrelated\nBash command\nCreate empty permission marker file\n1. Yes\n2. Yes, and don't ask again\n3. No",
        );
        let request = connector
            .pending_request
            .as_ref()
            .expect("request remains pending");
        assert!(request.screen_seen);
        assert_eq!(request.choices, ["Yes", "Yes, and don't ask again", "No"]);
        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request { choices, status: RequestStatus::Pending, .. },
                ..
            }) if choices == &vec!["Yes".to_owned(), "Yes, and don't ask again".to_owned(), "No".to_owned()]
        )));
    }

    #[test]
    fn transcript_records_before_a_permission_hook_do_not_dismiss_it() {
        let mut connector = JsonlConnector::fixture("claude", PathBuf::from("unused-source.jsonl"));
        connector.pending_request = Some(PendingRequest {
            id: "permission-1".to_owned(),
            request_type: RequestType::Permission,
            prompt: "Create permission marker file".to_owned(),
            choices: Vec::new(),
            screen_seen: false,
            announced_at: Some("2026-09-22T07:03:52Z".to_owned()),
        });
        let record = serde_json::json!({
            "type": "user",
            "uuid": "earlier-user-message",
            "timestamp": "2026-09-22T07:03:51Z",
            "message": { "content": "Use the Bash tool." }
        });

        let mutations = connector.claude_record(record.as_object().unwrap(), "user", 1);
        assert_eq!(
            connector
                .pending_request
                .as_ref()
                .map(|request| request.id.as_str()),
            Some("permission-1")
        );
        assert!(!mutations.iter().any(|mutation| matches!(
            mutation,
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    status: RequestStatus::Dismissed,
                    ..
                },
                ..
            })
        )));
    }

    fn claude_tool_round_trip(input: Value, result: Value) -> (ToolStatus, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        let call = serde_json::json!({
            "type": "assistant",
            "uuid": "assistant-1",
            "timestamp": "2026-09-22T08:00:00Z",
            "message": { "content": [
                { "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": input }
            ]}
        });
        connector.claude_record(call.as_object().unwrap(), "assistant", 1);
        let mut block = result;
        block["type"] = "tool_result".into();
        block["tool_use_id"] = "toolu_1".into();
        let record = serde_json::json!({
            "type": "user",
            "uuid": "user-1",
            "parentUuid": "assistant-1",
            "timestamp": "2026-09-22T08:00:01Z",
            "message": { "content": [block] }
        });
        let mutations = connector.claude_record(record.as_object().unwrap(), "user", 2);
        let tool = mutations
            .into_iter()
            .find_map(|mutation| match mutation {
                ConnectorMutation::Upsert(ObservedItem {
                    kind:
                        ConversationItemKind::Tool {
                            status, summary, ..
                        },
                    ..
                }) => Some((status, summary)),
                _ => None,
            })
            .expect("the tool_result updates its call");
        assert!(
            connector.tool_summaries.is_empty(),
            "a finished call's summary is released"
        );
        tool
    }

    fn assert_clean(summary: &str, secrets: &[&str]) {
        assert!(
            summary.chars().count() <= MAX_TOOL_SUMMARY_CHARS,
            "{summary}"
        );
        assert!(!summary.contains('\n'), "{summary}");
        for secret in secrets {
            assert!(!summary.contains(secret), "{secret} leaked into {summary}");
        }
    }

    #[test]
    fn tool_result_error_flag_fails_the_call_with_its_first_error_line() {
        let (status, summary) = claude_tool_round_trip(
            serde_json::json!({ "description": "Run the suite", "command": "cargo test" }),
            serde_json::json!({ "content": "Exit code 101\nthread panicked", "is_error": true }),
        );
        assert_eq!(status, ToolStatus::Failed);
        assert_eq!(summary, "Run the suite · failed: Exit code 101");
    }

    #[test]
    fn tool_result_success_is_described_by_shape_not_content() {
        let (status, summary) = claude_tool_round_trip(
            serde_json::json!({ "description": "Read the config", "command": "cat .env" }),
            serde_json::json!({ "content": [
                { "type": "text", "text": "DATABASE_URL=postgres://u:hunter2@db\n\nMODE=prod" }
            ]}),
        );
        assert_eq!(status, ToolStatus::Succeeded);
        assert_eq!(summary, "Read the config · returned 2 lines");

        let (_, empty) = claude_tool_round_trip(
            serde_json::json!({ "command": "true" }),
            serde_json::json!({ "content": "" }),
        );
        assert_eq!(empty, "Bash command · no output");
    }

    #[test]
    fn tool_input_summary_never_copies_the_raw_command() {
        let summary = safe_tool_summary(
            "Bash",
            Some(&serde_json::json!({ "command": "curl -H 'Authorization: Bearer abc123'" })),
        );
        assert_eq!(summary, "Bash command");
        let read = safe_tool_summary(
            "Read",
            Some(&serde_json::json!({ "file_path": "/Users/alice/work/app/src/main.rs" })),
        );
        assert_eq!(read, "~/work/app/src/main.rs");
    }

    #[test]
    fn tool_error_detail_redacts_secrets_environment_and_home_paths() {
        let (status, summary) = claude_tool_round_trip(
            serde_json::json!({ "description": "Deploy with GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123" }),
            serde_json::json!({
                "content": "error: AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY \"api_key\":\"s3cr3tvalue\" Authorization: Bearer opaque-token-value sk-live-0123456789abcdefghij at /home/bob/.aws/credentials",
                "is_error": true
            }),
        );
        assert_eq!(status, ToolStatus::Failed);
        assert_clean(
            &summary,
            &[
                "ghp_abcdefghijklmnopqrstuvwxyz0123",
                "wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY",
                "s3cr3tvalue",
                "opaque-token-value",
                "sk-live-0123456789abcdefghij",
                "/home/bob",
            ],
        );
        assert!(summary.contains("GITHUB_TOKEN=[redacted]"), "{summary}");
        assert!(summary.contains("~/.aws/credentials"), "{summary}");
    }

    #[test]
    fn unexpectedly_large_tool_records_yield_a_bounded_summary() {
        let huge = format!("{}\n", "x".repeat(900 * 1024));
        let (status, summary) = claude_tool_round_trip(
            serde_json::json!({ "description": "d".repeat(100_000) }),
            serde_json::json!({ "content": huge, "is_error": true }),
        );
        assert_eq!(status, ToolStatus::Failed);
        assert_clean(&summary, &[]);
        assert!(summary.ends_with('…'));

        let (_, many_lines) = claude_tool_round_trip(
            serde_json::json!({ "description": "List everything" }),
            serde_json::json!({ "content": "line\n".repeat(200_000) }),
        );
        assert_eq!(many_lines, "List everything · returned 200000 lines");
    }

    #[test]
    fn generic_connector_summaries_are_bounded_and_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("codex", dir.path().join("source.jsonl"));
        let record = serde_json::json!({
            "event": "tool_result",
            "id": "call-1",
            "status": "failed",
            "summary": format!("OPENAI_API_KEY=sk-proj-abcdefghijklmnop0123 {}", "y ".repeat(10_000)),
        });
        let mutations = connector.record(record, 1);
        let Some(ConnectorMutation::Upsert(ObservedItem {
            kind: ConversationItemKind::Tool {
                status, summary, ..
            },
            ..
        })) = mutations.into_iter().next()
        else {
            panic!("tool_result projects a tool item");
        };
        assert_eq!(status, ToolStatus::Failed);
        assert_clean(&summary, &["sk-proj-abcdefghijklmnop0123"]);
    }

    #[test]
    fn sanitizer_is_idempotent_and_keeps_ordinary_text() {
        let once = sanitize_summary("Read /Users/jake/a.rs with TOKEN=abc · failed: Exit code 1");
        assert_eq!(
            once,
            "Read ~/a.rs with TOKEN=[redacted] · failed: Exit code 1"
        );
        assert_eq!(sanitize_summary(&once), once);
        assert_eq!(
            sanitize_summary("Check https://example.com/docs at 12:30"),
            "Check https://example.com/docs at 12:30"
        );
    }

    fn claude_hook(event: &str, observer_version: u64) -> Value {
        serde_json::json!({
            "hook_event_name": event,
            "latch_observer_version": observer_version,
        })
    }

    fn claude_user_message(uuid: &str, text: &str) -> Value {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": "2026-09-22T09:00:00Z",
            "message": { "content": text },
        })
    }

    fn claude_tool_call(uuid: &str, parent: &str, call_id: &str) -> Value {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": "2026-09-22T09:00:01Z",
            "message": { "content": [
                { "type": "tool_use", "id": call_id, "name": "Bash", "input": { "command": "make build" } }
            ]},
        })
    }

    fn claude_tool_result(uuid: &str, parent: &str, call_id: &str) -> Value {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": "2026-09-22T09:00:02Z",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": call_id, "content": "build ok" }
            ]},
        })
    }

    #[test]
    fn a_stop_hook_capable_session_stays_working_between_tool_calls_until_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));

        connector.claude_record(
            claude_hook("SessionStart", 2).as_object().unwrap(),
            "hook",
            1,
        );
        assert!(connector.stop_hook_supported());

        let user_message = claude_user_message("user-1", "Please run the build");
        connector.claude_record(user_message.as_object().unwrap(), "user", 2);
        assert_eq!(connector.state().phase, ConversationPhase::Working);

        let tool_call = claude_tool_call("assistant-1", "user-1", "toolu_1");
        connector.claude_record(tool_call.as_object().unwrap(), "assistant", 3);
        assert_eq!(connector.state().phase, ConversationPhase::Working);

        let tool_result = claude_tool_result("user-2", "assistant-1", "toolu_1");
        connector.claude_record(tool_result.as_object().unwrap(), "user", 4);
        // Between tool calls the agent is still mid-turn. Before the Stop
        // hook, the phase inference (tool_running alone) would incorrectly
        // report Idle right here even though nothing has actually finished.
        assert!(!connector.tool_running);
        assert_eq!(connector.state().phase, ConversationPhase::Working);

        connector.claude_record(claude_hook("Stop", 2).as_object().unwrap(), "hook", 5);
        assert_eq!(connector.state().phase, ConversationPhase::Idle);
    }

    #[test]
    fn a_session_on_an_older_observer_keeps_the_prior_tool_running_inference() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        // No hook ever names an observer version at or above
        // STOP_HOOK_MIN_OBSERVER_VERSION: this session's Claude process was
        // launched with the older plugin directory and will never emit Stop.
        assert!(!connector.stop_hook_supported());

        let user_message = claude_user_message("user-1", "Please run the build");
        connector.claude_record(user_message.as_object().unwrap(), "user", 1);
        assert!(!connector.turn_open);

        let tool_call = claude_tool_call("assistant-1", "user-1", "toolu_1");
        connector.claude_record(tool_call.as_object().unwrap(), "assistant", 2);
        assert_eq!(connector.state().phase, ConversationPhase::Working);

        let tool_result = claude_tool_result("user-2", "assistant-1", "toolu_1");
        connector.claude_record(tool_result.as_object().unwrap(), "user", 3);
        // No capability was ever learned, so the connector must not invent a
        // turn boundary it cannot back: the pre-existing behavior (Idle as
        // soon as no tool is running) stays exactly as it was.
        assert_eq!(connector.state().phase, ConversationPhase::Idle);
    }

    #[test]
    fn a_hook_reporting_an_old_observer_version_does_not_enable_stop_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        connector.claude_record(
            claude_hook("SessionStart", 1).as_object().unwrap(),
            "hook",
            1,
        );
        assert!(!connector.stop_hook_supported());
    }

    #[test]
    fn an_open_turn_survives_a_checkpoint_restore() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        let mut connector = JsonlConnector::fixture("claude", source.clone());
        connector.claude_record(
            claude_hook("SessionStart", 2).as_object().unwrap(),
            "hook",
            1,
        );
        let user_message = claude_user_message("user-1", "Please run the build");
        connector.claude_record(user_message.as_object().unwrap(), "user", 2);
        assert_eq!(connector.state().phase, ConversationPhase::Working);

        let checkpoint = connector.checkpoint_snapshot().unwrap();
        let mut restored = JsonlConnector::fixture("claude", source);
        restored.restore_checkpoint(&checkpoint).unwrap();
        assert!(restored.stop_hook_supported());
        assert_eq!(restored.state().phase, ConversationPhase::Working);

        restored.claude_record(claude_hook("Stop", 2).as_object().unwrap(), "hook", 3);
        assert_eq!(restored.state().phase, ConversationPhase::Idle);
    }

    fn claude_assistant_text(uuid: &str, parent: &str, text: &str) -> Value {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": "2026-09-22T09:00:03Z",
            "message": { "content": [{ "type": "text", "text": text }] },
        })
    }

    fn claude_child(mut record: Value, parent: &str) -> Value {
        record["parentUuid"] = Value::from(parent);
        record
    }

    fn feed(connector: &mut JsonlConnector, records: &[Value]) -> Vec<ConnectorMutation> {
        records
            .iter()
            .enumerate()
            .flat_map(|(index, record)| {
                let object = record.as_object().unwrap();
                let event = string(object, "type").unwrap_or_default();
                connector.claude_record(object, &event, index as u64 + 1)
            })
            .collect()
    }

    fn rebuilds(mutations: &[ConnectorMutation]) -> usize {
        mutations
            .iter()
            .filter(|mutation| matches!(mutation, ConnectorMutation::Rebuild { .. }))
            .count()
    }

    fn truncations(mutations: &[ConnectorMutation]) -> Vec<String> {
        mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ConnectorMutation::TruncateAfter(id) => Some(id.as_str().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Claude files each result of a parallel batch under the assistant record
    /// that made the call. Reading that as a rewind removed the later calls and
    /// then rebuilt the conversation on every following record.
    #[test]
    fn parallel_tool_results_are_siblings_not_a_rewind() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        let mutations = feed(
            &mut connector,
            &[
                claude_user_message("user-1", "Look around"),
                claude_tool_call("call-a", "user-1", "toolu_a"),
                claude_tool_call("call-b", "call-a", "toolu_b"),
                claude_tool_call("call-c", "call-b", "toolu_c"),
                claude_tool_result("result-a", "call-a", "toolu_a"),
                claude_tool_result("result-b", "call-b", "toolu_b"),
                claude_tool_result("result-c", "call-c", "toolu_c"),
                claude_assistant_text("answer", "result-c", "All three ran."),
                claude_child(claude_user_message("user-2", "Thanks"), "answer"),
            ],
        );
        assert_eq!(rebuilds(&mutations), 0);
        assert!(truncations(&mutations).is_empty());
        let succeeded = mutations
            .iter()
            .filter(|mutation| {
                matches!(
                    mutation,
                    ConnectorMutation::Upsert(ObservedItem {
                        kind: ConversationItemKind::Tool {
                            status: ToolStatus::Succeeded,
                            ..
                        },
                        ..
                    })
                )
            })
            .count();
        assert_eq!(succeeded, 3);
        assert!(connector.tools.is_empty());
    }

    #[test]
    fn parallel_tool_results_survive_a_journal_replay() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        let records = [
            claude_user_message("user-1", "Look around"),
            claude_tool_call("call-a", "user-1", "toolu_a"),
            claude_tool_call("call-b", "call-a", "toolu_b"),
            claude_tool_result("result-a", "call-a", "toolu_a"),
            claude_tool_result("result-b", "call-b", "toolu_b"),
        ];
        fs::write(
            &source,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let mut connector = JsonlConnector::fixture("claude", source.clone());
        let poll = connector
            .poll(PollBudget {
                max_records: 64,
                deadline: Duration::from_secs(1),
            })
            .unwrap();
        assert_eq!(rebuilds(&poll.mutations), 0);

        let mut replayed = JsonlConnector::fixture("claude", source);
        replayed
            .apply_checkpoint_delta(&poll.checkpoint_delta)
            .unwrap();
        assert_eq!(replayed.active_chain, connector.active_chain);
        assert_eq!(replayed.chain_items, connector.chain_items);
    }

    /// A rewind's parent is usually a record that never produced an item, so
    /// the truncation has to name the nearest surviving record that did.
    #[test]
    fn a_rewind_truncates_after_the_nearest_surviving_item() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        let note = serde_json::json!({
            "type": "system",
            "uuid": "note-1",
            "parentUuid": "answer-1",
            "timestamp": "2026-09-22T09:00:04Z",
        });
        let mutations = feed(
            &mut connector,
            &[
                claude_user_message("user-1", "First"),
                claude_assistant_text("answer-1", "user-1", "One."),
                note,
                claude_child(claude_user_message("user-2", "Second"), "note-1"),
                claude_assistant_text("answer-2", "user-2", "Two."),
                claude_child(claude_user_message("user-3", "Second, reworded"), "note-1"),
            ],
        );
        assert_eq!(rebuilds(&mutations), 0);
        assert_eq!(truncations(&mutations), ["answer-1"]);
        assert_eq!(
            connector.active_chain,
            ["user-1", "answer-1", "note-1", "user-3"]
        );
    }

    #[test]
    fn an_unclassifiable_parent_rebuilds_once_and_then_continues() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        let mutations = feed(
            &mut connector,
            &[
                claude_user_message("user-1", "First"),
                claude_assistant_text("answer-1", "elsewhere", "From another branch."),
                claude_child(claude_user_message("user-2", "Next"), "answer-1"),
                claude_assistant_text("answer-2", "user-2", "Still here."),
            ],
        );
        assert_eq!(rebuilds(&mutations), 1);
        assert_eq!(connector.active_chain, ["answer-1", "user-2", "answer-2"]);
        assert!(matches!(
            mutations.last(),
            Some(ConnectorMutation::Upsert(ObservedItem { id, .. })) if id.as_str() == "answer-2"
        ));
    }

    #[test]
    fn compaction_keeps_the_settled_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        let boundary = serde_json::json!({
            "type": "system",
            "subtype": "compact_boundary",
            "uuid": "boundary",
            "parentUuid": null,
            "logicalParentUuid": "answer-1",
            "timestamp": "2026-09-22T09:00:05Z",
        });
        let mut summary = claude_child(
            claude_user_message("summary", "Summary of it all"),
            "boundary",
        );
        summary["isCompactSummary"] = Value::Bool(true);
        let mutations = feed(
            &mut connector,
            &[
                claude_user_message("user-1", "First"),
                claude_assistant_text("answer-1", "user-1", "One."),
                boundary,
                summary,
                claude_child(claude_user_message("user-2", "Carry on"), "summary"),
            ],
        );
        assert_eq!(rebuilds(&mutations), 0);
        assert!(truncations(&mutations).is_empty());
        let user_texts: Vec<_> = mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ConnectorMutation::Upsert(ObservedItem {
                    kind:
                        ConversationItemKind::Message {
                            role: MessageRole::User,
                            text,
                            ..
                        },
                    ..
                }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(user_texts, ["First", "Carry on"]);
    }

    #[test]
    fn injected_user_rows_are_not_presented_as_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        connector.claude_record(
            claude_hook("SessionStart", 2).as_object().unwrap(),
            "hook",
            1,
        );
        let mut skill = claude_user_message("skill-body", "Base directory for this skill: /x");
        skill["isMeta"] = Value::Bool(true);
        let mutations = connector.claude_record(skill.as_object().unwrap(), "user", 2);
        assert!(mutations.is_empty());
        assert!(!connector.turn_open);
    }

    fn bridge_record(event: &str, at: &str) -> Value {
        serde_json::json!({
            "hook_event_name": crate::observer::CLAUDE_BRIDGE_EVENT,
            "latch_observer_version": 2,
            "bridge_event": event,
            "bridge_version": 1,
            "timestamp": at,
        })
    }

    fn observe_bridge(connector: &mut JsonlConnector, event: &str, at: &str) {
        connector.claude_record(bridge_record(event, at).as_object().unwrap(), "hook", 1);
    }

    /// The bridge reports the engine's own turn boundaries, so an interrupted
    /// turn closes too: `Stop` never fires for one.
    #[test]
    fn bridge_turn_boundaries_open_and_close_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");
        assert_eq!(connector.bridge_version, Some(1));
        assert_eq!(connector.state().phase, ConversationPhase::Idle);

        observe_bridge(&mut connector, "turn.start", "2026-09-22T09:00:01.000Z");
        assert_eq!(connector.state().phase, ConversationPhase::Working);
        let call = claude_tool_call("assistant-1", "user-1", "toolu_1");
        connector.claude_record(call.as_object().unwrap(), "assistant", 2);
        assert!(connector.tool_running);

        let mut interrupted = bridge_record("turn.complete", "2026-09-22T09:00:05.000Z");
        interrupted["reason"] = Value::from("aborted");
        connector.claude_record(interrupted.as_object().unwrap(), "hook", 3);
        assert_eq!(connector.state().phase, ConversationPhase::Idle);
        assert!(connector.state().send_message.enabled);
    }

    /// Hooks are read before the transcript, so a conversation opened late
    /// sees every close before the prompts that preceded them.
    #[test]
    fn a_replayed_prompt_does_not_reopen_a_turn_that_already_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        observe_bridge(&mut connector, "hello", "2026-09-22T08:59:00.000Z");
        observe_bridge(&mut connector, "turn.start", "2026-09-22T08:59:59.900Z");
        observe_bridge(&mut connector, "turn.complete", "2026-09-22T09:00:30.000Z");

        let earlier = claude_user_message("user-1", "Please run the build");
        connector.claude_record(earlier.as_object().unwrap(), "user", 4);
        assert_eq!(connector.state().phase, ConversationPhase::Idle);

        let mut later = claude_child(claude_user_message("user-2", "And the tests"), "user-1");
        later["timestamp"] = Value::from("2026-09-22T09:01:00.000Z");
        connector.claude_record(later.as_object().unwrap(), "user", 5);
        assert_eq!(connector.state().phase, ConversationPhase::Working);
    }

    #[test]
    fn a_whole_second_stop_only_covers_prompts_from_an_earlier_second() {
        assert!(turn_closed_at_or_after(
            "2026-09-22T09:00:30Z",
            "2026-09-22T09:00:29.900Z"
        ));
        assert!(!turn_closed_at_or_after(
            "2026-09-22T09:00:30Z",
            "2026-09-22T09:00:30.100Z"
        ));
        assert!(turn_closed_at_or_after(
            "2026-09-22T09:00:30.500Z",
            "2026-09-22T09:00:30.100Z"
        ));
        assert!(!turn_closed_at_or_after(
            "2026-09-22T09:00:30.500Z",
            "2026-09-22T09:00:30.900Z"
        ));
    }

    #[test]
    fn the_bridge_survives_a_clear_and_leaves_with_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");
        let mut cleared = bridge_record("session.end", "2026-09-22T09:00:01.000Z");
        cleared["reason"] = Value::from("clear");
        connector.claude_record(cleared.as_object().unwrap(), "hook", 2);
        assert_eq!(connector.bridge_version, Some(1));

        let mut exited = bridge_record("session.end", "2026-09-22T09:00:02.000Z");
        exited["reason"] = Value::from("prompt_input_exit");
        connector.claude_record(exited.as_object().unwrap(), "hook", 3);
        assert_eq!(connector.bridge_version, None);

        let checkpoint = {
            observe_bridge(&mut connector, "hello", "2026-09-22T09:00:03.000Z");
            connector.checkpoint_snapshot().unwrap()
        };
        let mut restored = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        restored.restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(restored.bridge_version, Some(1));
    }

    fn bridged_connector(home: &std::path::Path) -> JsonlConnector {
        let home = LatchHome::new(home);
        let session = SessionId::generate();
        let paths = home.session(&session);
        paths.ensure().unwrap();
        fs::write(paths.meta(), br#"{"harness":"claude"}"#).unwrap();
        let source = home.root().join("source.jsonl");
        fs::write(&source, b"").unwrap();
        let mut connector = JsonlConnector::fixture("claude", source);
        connector.home = home;
        connector.session = session;
        connector.bridge_version = Some(1);
        connector
    }

    /// Plays the bridge module: takes the queued command and answers it.
    fn answer_next_bridge_command(
        connector: &JsonlConnector,
        outcome: &'static str,
    ) -> std::thread::JoinHandle<Value> {
        let paths = connector.home.session(&connector.session);
        std::thread::spawn(move || {
            let inbox = paths.conversation_bridge_inbox();
            let command = loop {
                // A command is in the inbox once it carries its final name.
                let waiting = fs::read_dir(&inbox).ok().and_then(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .find(|entry| entry.path().extension().is_some_and(|kind| kind == "json"))
                });
                if let Some(entry) = waiting {
                    let raw = fs::read(entry.path()).unwrap();
                    fs::remove_file(entry.path()).unwrap();
                    break serde_json::from_slice::<Value>(&raw).unwrap();
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let result = serde_json::json!({
                "hook_event_name": crate::observer::CLAUDE_BRIDGE_EVENT,
                "bridge_event": "command.result",
                "command_id": command["id"],
                "outcome": outcome,
                "detail": "the agent said no",
            });
            let mut sidecar = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(paths.conversation_source_hooks())
                .unwrap();
            std::io::Write::write_all(&mut sidecar, format!("{result}\n").as_bytes()).unwrap();
            command
        })
    }

    fn send(text: &str) -> ConnectorAction {
        ConnectorAction {
            id: ACTION_SEND_MESSAGE.to_owned(),
            payload: serde_json::json!({ "text": text }),
        }
    }

    #[test]
    fn live_bridge_queues_a_send_while_tools_and_a_turn_are_running() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        connector.turn_open = true;
        connector.tool_running = true;
        assert!(connector.state().send_message.enabled);
        assert!(connector.state().cancel_turn.enabled);
        let module = answer_next_bridge_command(&connector, "queued");
        assert_eq!(
            connector
                .apply(send("next task"), Duration::from_secs(5))
                .unwrap(),
            ApplyResult::Queued { correlation: None }
        );
        assert_eq!(module.join().unwrap()["kind"], "submit_prompt");
        connector.bridge_version = None;
        assert!(!connector.state().send_message.enabled);
        assert!(!connector.state().cancel_turn.enabled);
        assert!(matches!(
            connector
                .apply(send("next task"), Duration::from_secs(1))
                .unwrap(),
            ApplyResult::Refused { .. }
        ));
    }

    #[test]
    fn stop_requires_a_live_bridge_and_open_turn_and_never_uses_the_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        let stop = || ConnectorAction {
            id: super::super::super::ACTION_CANCEL_TURN.into(),
            payload: serde_json::json!({}),
        };
        assert!(!connector.state().cancel_turn.enabled);
        assert!(matches!(
            connector.apply(stop(), Duration::from_secs(1)).unwrap(),
            ApplyResult::Refused { .. }
        ));
        connector.turn_open = true;
        let module = answer_next_bridge_command(&connector, "accepted");
        assert_eq!(
            connector.apply(stop(), Duration::from_secs(5)).unwrap(),
            ApplyResult::Accepted { correlation: None }
        );
        assert_eq!(module.join().unwrap()["kind"], "abort_turn");
        // An acknowledgement alone does not close the turn; the hook does.
        assert!(connector.turn_open);
        assert!(matches!(
            connector.apply(stop(), Duration::from_millis(50)).unwrap(),
            ApplyResult::Refused { .. }
        ));
        assert!(connector.bridge_version.is_none());
        assert!(!connector.state().cancel_turn.enabled);
        assert!(matches!(
            connector.apply(stop(), Duration::from_secs(1)).unwrap(),
            ApplyResult::Refused { .. }
        ));
    }

    #[test]
    fn slash_commands_are_not_typed_into_a_running_turn() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        connector.turn_open = true;
        assert!(matches!(
            connector
                .apply(send("/clear"), Duration::from_secs(1))
                .unwrap(),
            ApplyResult::Refused { .. }
        ));
    }

    #[test]
    fn a_send_goes_through_the_bridge_without_touching_the_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        // No kernel exists for this session: reaching for the terminal fails.
        let module = answer_next_bridge_command(&connector, "accepted");
        let result = connector
            .apply(send("run the tests"), Duration::from_secs(5))
            .unwrap();
        assert_eq!(result, ApplyResult::Accepted { correlation: None });
        let command = module.join().unwrap();
        assert_eq!(command["kind"], "submit_prompt");
        assert_eq!(command["text"], "run the tests");
    }

    #[test]
    fn a_bridge_refusal_is_reported_with_the_agents_reason() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        let module = answer_next_bridge_command(&connector, "refused");
        let result = connector
            .apply(send("run the tests"), Duration::from_secs(5))
            .unwrap();
        module.join().unwrap();
        assert_eq!(
            result,
            ApplyResult::Refused {
                reason: "the agent said no".to_owned()
            }
        );
    }

    #[test]
    fn an_untaken_bridge_command_is_withdrawn_before_the_terminal_is_tried() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        let inbox = connector
            .home
            .session(&connector.session)
            .conversation_bridge_inbox();
        // Nobody plays the module. The terminal path then fails for want of a
        // kernel, which is the proof it was the path taken.
        assert!(connector
            .apply(send("run the tests"), Duration::from_secs(5))
            .is_err());
        assert_eq!(connector.bridge_version, None);
        assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
    }

    /// Diagnostic, not a regression test: projects the Claude transcript named
    /// by `LATCH_REPLAY_TRANSCRIPT` and reports what a client would be shown.
    /// `cargo test -p latch --lib replays_a_named_transcript -- --ignored --nocapture`
    #[test]
    #[ignore = "needs LATCH_REPLAY_TRANSCRIPT"]
    fn replays_a_named_transcript() {
        let source = PathBuf::from(
            std::env::var_os("LATCH_REPLAY_TRANSCRIPT").expect("LATCH_REPLAY_TRANSCRIPT is set"),
        );
        let (mutations, projection) = project_case(source);
        let items = projection.snapshot(usize::MAX).items;
        let count = |wanted: fn(&ConversationItemKind) -> bool| {
            items.iter().filter(|item| wanted(&item.kind)).count()
        };
        println!(
            "mutations={} rebuilds={} truncations={} items={} messages={} tools={} open_tools={}",
            mutations.len(),
            rebuilds(&mutations),
            truncations(&mutations).len(),
            items.len(),
            count(|kind| matches!(kind, ConversationItemKind::Message { .. })),
            count(|kind| matches!(kind, ConversationItemKind::Tool { .. })),
            count(|kind| matches!(
                kind,
                ConversationItemKind::Tool {
                    status: ToolStatus::Running,
                    ..
                }
            )),
        );
        assert_eq!(rebuilds(&mutations), 0);
    }

    #[test]
    fn a_question_is_answered_through_the_bridge_by_its_call_id_with_free_text() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        connector.pending_request = Some(PendingRequest {
            id: "toolu_q".to_owned(),
            request_type: RequestType::Question,
            prompt: "Which color?".to_owned(),
            choices: vec!["Red".to_owned(), "Blue".to_owned()],
            screen_seen: false,
            announced_at: None,
        });
        let module = answer_next_bridge_command(&connector, "accepted");
        let result = connector
            .apply(
                ConnectorAction {
                    id: ACTION_RESOLVE_REQUEST.to_owned(),
                    payload: serde_json::json!({ "requestId": "toolu_q", "choice": "Chartreuse" }),
                },
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result, ApplyResult::Accepted { correlation: None });
        assert!(connector.pending_request.is_none());
        let command = module.join().unwrap();
        assert_eq!(command["kind"], "answer_question");
        assert_eq!(command["tool_use_id"], "toolu_q");
        assert_eq!(
            command["answers"],
            serde_json::json!({ "Which color?": "Chartreuse" })
        );
    }

    #[test]
    fn a_slash_command_and_a_permission_stay_on_the_terminal_path() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = bridged_connector(temp.path());
        let inbox = connector
            .home
            .session(&connector.session)
            .conversation_bridge_inbox();
        // No kernel exists, so the terminal path errors; the bridge is not asked.
        assert!(connector
            .apply(send("/compact"), Duration::from_secs(2))
            .is_err());
        assert!(!inbox.exists());

        connector.pending_request = Some(PendingRequest {
            id: "permission-1".to_owned(),
            request_type: RequestType::Permission,
            prompt: "Allow Bash?".to_owned(),
            choices: vec!["Yes".to_owned(), "No".to_owned()],
            screen_seen: true,
            announced_at: None,
        });
        assert!(connector
            .apply(
                ConnectorAction {
                    id: ACTION_RESOLVE_REQUEST.to_owned(),
                    payload: serde_json::json!({ "requestId": "permission-1", "choice": "Yes" }),
                },
                Duration::from_secs(2),
            )
            .is_err());
        assert!(!inbox.exists());
        assert_eq!(connector.bridge_version, Some(1));
    }

    /// While a question is open the transcript does not hold its call yet.
    /// The bridge announces it, and Claude's own permission hook for the same
    /// call must not replace it with a generic prompt.
    #[test]
    fn the_bridge_announces_an_open_question_under_its_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
        observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");

        let mut open = bridge_record("question.open", "2026-09-22T09:00:01.000Z");
        open["tool_use_id"] = Value::from("toolu_q");
        open["questions"] = serde_json::json!([{
            "question": "Which color?",
            "header": "Color",
            "multi_select": false,
            "options": [{ "label": "Red" }, { "label": "Blue" }],
        }]);
        let mutations = connector.claude_record(open.as_object().unwrap(), "hook", 2);
        assert!(matches!(
            mutations.as_slice(),
            [ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    request_id,
                    request_type: RequestType::Question,
                    prompt,
                    choices,
                    status: RequestStatus::Pending,
                },
                ..
            })] if request_id == "toolu_q" && prompt == "Which color?" && choices == &["Red", "Blue"]
        ));
        assert_eq!(connector.state().phase, ConversationPhase::AwaitingInput);

        let permission = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "latch_observer_version": 2,
            "tool_name": "AskUserQuestion",
            "prompt_id": "prompt-1",
            "timestamp": "2026-09-22T09:00:01Z",
        });
        assert!(connector
            .claude_record(permission.as_object().unwrap(), "hook", 3)
            .is_empty());
        assert_eq!(connector.pending_request.as_ref().unwrap().id, "toolu_q");

        let mut closed = bridge_record("question.closed", "2026-09-22T09:00:09.000Z");
        closed["tool_use_id"] = Value::from("toolu_q");
        let mutations = connector.claude_record(closed.as_object().unwrap(), "hook", 4);
        assert!(matches!(
            mutations.as_slice(),
            [ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    status: RequestStatus::Dismissed,
                    ..
                },
                ..
            })]
        ));
        assert!(connector.pending_request.is_none());
    }
}
