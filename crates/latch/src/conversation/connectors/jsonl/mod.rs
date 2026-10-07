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

mod bridge;
mod checkpoint;
mod claude;
mod codex;
mod connector;
mod cursor;
#[cfg(test)]
mod cursor_live_tests;
#[cfg(test)]
mod geometry_tests;
mod questions;
mod records;
mod redaction;
mod requests;
mod screen;
mod source;
#[cfg(test)]
mod tests;
mod tool_outcome;

use bridge::{bridge_permission_decision, BridgeAnswer};
use checkpoint::SourceIdentity;
use claude::claude_text;
use questions::{bridge_questions, claude_question_choices, claude_question_prompt};
use redaction::{sanitize_part, sanitize_summary};
use requests::request_mutation;
use screen::{is_empty_composer, painted_request, visible_choices};
use source::read_bounded;
use tool_outcome::{claude_tool_outcome, safe_tool_summary};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PendingRequest {
    id: String,
    request_type: RequestType,
    prompt: String,
    choices: Vec<String>,
    #[serde(default)]
    questions: Vec<super::super::RequestQuestion>,
    /// A permission hook can arrive a fraction before Claude paints its
    /// prompt. Do not mistake that first empty snapshot for a dismissal.
    #[serde(default)]
    screen_seen: bool,
    /// Claude's transcript and hook sidecar advance independently. This lets
    /// us ignore transcript records that existed before a newly-read hook.
    #[serde(default)]
    announced_at: Option<String>,
    /// Announced by the bridge module under the call's real id. The bridge,
    /// not the transcript, says when such a request closes, and the Hub can
    /// answer it through the bridge by that id.
    #[serde(default)]
    bridge_call: bool,
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
    /// The request clients are shown. The Hub presents the newest pending
    /// request item, so this is always the one surfaced last of those still
    /// open.
    pending_request: Option<PendingRequest>,
    /// Bridge requests waiting behind the shown one; see `RuntimeCheckpoint`.
    queued_requests: Vec<PendingRequest>,
    displaced_requests: Vec<PendingRequest>,
    tools: HashMap<String, (String, String)>,
    tool_summaries: HashMap<String, String>,
    tool_running: bool,
    turn_open: bool,
    hook_observer_version: Option<u32>,
    bridge_version: Option<u32>,
    last_turn_close: Option<String>,
    /// Why the agent closed its newest turn, from the bridge. Cleared when
    /// any turn opens, so it only ever describes the turn that just ended.
    turn_outcome: Option<crate::conversation::TurnOutcome>,
    /// The slash commands the bridge greeting advertised. Dropped with the
    /// bridge: a catalog nobody can vouch for is unknown, not empty.
    commands: Option<Vec<crate::conversation::AdvertisedCommand>>,
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
    fn new(
        id: &'static str,
        home: LatchHome,
        session: SessionId,
        source: Option<PathBuf>,
        agent_session_id: Option<String>,
        control: Option<ConversationControl>,
        live_screen: bool,
    ) -> Self {
        Self {
            id,
            version: "1",
            home,
            session,
            source_identity: source.as_ref().and_then(SourceIdentity::at),
            source,
            agent_session_id,
            offset: 0,
            hook_offset: 0,
            active_chain: Vec::new(),
            chain_items: HashMap::new(),
            malformed_records: 0,
            pending_request: None,
            queued_requests: Vec::new(),
            displaced_requests: Vec::new(),
            tools: HashMap::new(),
            tool_summaries: HashMap::new(),
            tool_running: false,
            turn_open: false,
            hook_observer_version: None,
            bridge_version: None,
            last_turn_close: None,
            turn_outcome: None,
            commands: None,
            last_state: None,
            screen_can_send: None,
            live_screen,
            last_screen_refresh: None,
            control,
            refresh_screen: live_screen,
            #[cfg(test)]
            last_read_bytes: 0,
        }
    }

    pub fn for_session(home: LatchHome, session: SessionId) -> Self {
        let harness = meta::read(&home.session(&session))
            .ok()
            .and_then(|value| value.harness);
        let id = connector_kind(harness.as_deref()).unwrap_or("unknown");
        let (source, agent_session_id) = read_binding(&home, &session, id)
            .map_or((None, None), |(source, agent_session_id)| {
                (Some(source), agent_session_id)
            });
        let control = ConversationControl::open(&home, &session).ok();
        Self::new(id, home, session, source, agent_session_id, control, true)
    }

    #[cfg(test)]
    fn fixture(id: &'static str, source: PathBuf) -> Self {
        Self::new(
            id,
            LatchHome::new("/tmp/latch-connector-fixture"),
            SessionId::parse("ses_fixture").unwrap(),
            Some(source),
            Some("fixture".to_owned()),
            None,
            false,
        )
    }

    fn identity(&self) -> ConnectorIdentity {
        ConnectorIdentity {
            id: self.id.to_owned(),
            version: self.version.to_owned(),
        }
    }

    /// Whether this session's Claude launch is known to emit an authoritative
    /// `Stop` hook. `false` until a hook record has proven otherwise, which
    /// keeps every pre-existing session on the tool/screen inference it
    /// already had instead of fabricating a turn boundary it cannot back.
    fn stop_hook_supported(&self) -> bool {
        self.hook_observer_version
            .is_some_and(|version| version >= crate::observer::STOP_HOOK_MIN_OBSERVER_VERSION)
    }

    fn control(&mut self) -> Result<&mut ConversationControl> {
        if self.control.is_none() {
            self.control = Some(ConversationControl::open(&self.home, &self.session)?);
        }
        Ok(self.control.as_mut().expect("control installed"))
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
        self.queued_requests.clear();
        self.displaced_requests.clear();
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
        let (phase, send_message, resolve_request) = if self.source.is_none()
            && !(self.id == "cursor" && self.turn_open)
        {
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
                if self
                    .pending_request
                    .as_ref()
                    .is_some_and(|r| !r.questions.is_empty())
                    && self.bridge_version.is_none()
                {
                    (
                        false,
                        Some(
                            "the question bridge is no longer live; answer at the terminal".into(),
                        ),
                    )
                } else {
                    (true, None)
                },
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
            // An open turn has no outcome yet; the last one's is history.
            turn_outcome: if self.turn_open {
                None
            } else {
                self.turn_outcome
            },
            commands: if self.bridge_version.is_some() {
                self.commands.clone()
            } else {
                None
            },
        }
    }
}

fn upsert(id: &str, created_at: String, kind: ConversationItemKind) -> ConnectorMutation {
    ConnectorMutation::Upsert(ObservedItem {
        id: ConversationItemId::native(id),
        created_at,
        kind,
    })
}

fn string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn string_value(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
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
