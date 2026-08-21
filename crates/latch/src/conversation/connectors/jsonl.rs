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
use crate::engine::{self, PasteMessageRequest};
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId};

use super::super::{
    ActionDescriptor, ApplyResult, CheckpointDelta, Connector, ConnectorAction, ConnectorIdentity,
    ConnectorMutation, ConversationItemId, ConversationItemKind, ConversationPhase,
    ConversationState, Detection, MessageRole, MessageStatus, ObservedItem, PollBudget, PollResult,
    RequestStatus, RequestType, ToolStatus, ACTION_RESOLVE_REQUEST, ACTION_SEND_MESSAGE,
};

const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_READ_BYTES: usize = 2 * 1024 * 1024;

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
    match harness.as_deref() {
        Some("claude") | Some("codex") => Box::new(JsonlConnector::for_session(home, session)),
        _ => Box::new(super::super::PendingConnector::new()),
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct RuntimeCheckpoint {
    pending_request: Option<PendingRequest>,
    tools: HashMap<String, (String, String)>,
    tool_running: bool,
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
    malformed_records: u64,
    pending_request: Option<PendingRequest>,
    tools: HashMap<String, (String, String)>,
    tool_running: bool,
    last_state: Option<ConversationState>,
    screen_can_send: Option<bool>,
    live_screen: bool,
    last_screen_refresh: Option<Instant>,
    #[cfg(test)]
    last_read_bytes: usize,
}

impl JsonlConnector {
    pub fn for_session(home: LatchHome, session: SessionId) -> Self {
        let harness = meta::read(&home.session(&session))
            .ok()
            .and_then(|value| value.harness)
            .unwrap_or_default();
        let (id, version) = match harness.as_str() {
            "claude" => ("claude", "1"),
            "codex" => ("codex", "1"),
            _ => ("unknown", "1"),
        };
        let binding = read_binding(&home, &session, id);
        let source = binding.as_ref().map(|binding| binding.0.clone());
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
            malformed_records: 0,
            pending_request: None,
            tools: HashMap::new(),
            tool_running: false,
            last_state: None,
            screen_can_send: None,
            live_screen: true,
            last_screen_refresh: None,
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
            malformed_records: 0,
            pending_request: None,
            tools: HashMap::new(),
            tool_running: false,
            last_state: None,
            screen_can_send: None,
            live_screen: false,
            last_screen_refresh: None,
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
            tool_running: self.tool_running,
            last_state: self.last_state.clone(),
            screen_can_send: self.screen_can_send,
        }
    }

    fn restore_runtime(&mut self, runtime: RuntimeCheckpoint) {
        self.pending_request = runtime.pending_request;
        self.tools = runtime.tools;
        self.tool_running = runtime.tool_running;
        self.last_state = runtime.last_state;
        self.screen_can_send = runtime.screen_can_send;
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
        self.source_identity = SourceIdentity::at(&source);
        self.source = Some(source);
        self.agent_session_id = agent_session_id;
        self.offset = 0;
        self.active_chain.clear();
        self.pending_request = None;
        self.tools.clear();
        self.tool_running = false;
        self.last_state = None;
        self.screen_can_send = None;
        self.last_screen_refresh = None;
        replacing
    }

    fn state(&self) -> ConversationState {
        let (phase, send_message, resolve_request) = if self.source.is_none() {
            (
                ConversationPhase::Starting,
                (
                    false,
                    Some("waiting for the agent's authoritative source binding".to_owned()),
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
        } else if self.tool_running {
            (
                ConversationPhase::Working,
                (false, Some("agent is working".to_owned())),
                (false, Some("no pending request".to_owned())),
            )
        } else if self.screen_can_send == Some(false) {
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
            pending_request: self
                .pending_request
                .as_ref()
                .map(|request| request.id.clone()),
            connector: Some(self.identity()),
        }
    }

    fn observe_screen(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        let mut mutations = Vec::new();
        if let Some(request) = self.pending_request.as_ref() {
            if !screen_contains_request(screen, request) {
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
                text: string(object, "text")
                    .or_else(|| string(object, "message"))
                    .unwrap_or_default(),
                status: MessageStatus::Observed,
            }),
            "assistant_message" | "assistant" => Some(ConversationItemKind::Message {
                role: MessageRole::Assistant,
                text: string(object, "text")
                    .or_else(|| string(object, "message"))
                    .unwrap_or_default(),
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
                    summary: string(object, "summary").unwrap_or_default(),
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
                    summary: string(object, "summary").unwrap_or_default(),
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
        if event == "permission_request"
            || string(object, "hook_event_name").as_deref() == Some("PermissionRequest")
        {
            return self.claude_permission(object, ordinal);
        }
        if string(object, "hook_event_name").is_some() {
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
        let mut mutations = Vec::new();
        if let Some(parent) = parent {
            if self.active_chain.last() != Some(&parent) {
                if let Some(index) = self.active_chain.iter().position(|id| id == &parent) {
                    self.active_chain.truncate(index + 1);
                    mutations.push(ConnectorMutation::TruncateAfter(
                        ConversationItemId::native(parent),
                    ));
                } else {
                    self.active_chain.clear();
                    return vec![ConnectorMutation::Rebuild {
                        reason: "Claude source parent is outside the active branch".to_owned(),
                    }];
                }
            }
        } else if !self.active_chain.is_empty() {
            self.active_chain.clear();
            return vec![ConnectorMutation::Rebuild {
                reason: "Claude source started an incompatible root".to_owned(),
            }];
        }

        // Any authoritative main-chain progress dismisses a request that was
        // not itself re-announced. Claude emits no explicit resolution record.
        if let Some(request) = self.pending_request.take() {
            mutations.push(request_mutation(&request, RequestStatus::Dismissed));
        }
        let at = string(object, "timestamp").unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
        match event {
            "user" => {
                let content = object
                    .get("message")
                    .and_then(|message| message.get("content"));
                let text = claude_text(content);
                if !text.is_empty() {
                    mutations.push(upsert(
                        &uuid,
                        at.clone(),
                        ConversationItemKind::Message {
                            role: MessageRole::User,
                            text,
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
                        mutations.push(upsert(
                            &item_id,
                            at.clone(),
                            ConversationItemKind::Tool {
                                name,
                                summary: "completed".to_owned(),
                                status: ToolStatus::Succeeded,
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
                                mutations.push(upsert(
                                    &item_id,
                                    at.clone(),
                                    ConversationItemKind::Message {
                                        role: MessageRole::Assistant,
                                        text,
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
                            self.tools
                                .insert(call_id.clone(), (name.clone(), item_id.clone()));
                            mutations.push(upsert(
                                &item_id,
                                at.clone(),
                                ConversationItemKind::Tool {
                                    name: name.clone(),
                                    summary: safe_tool_summary(&name, block.get("input")),
                                    status: ToolStatus::Running,
                                    parent_message_id: Some(ConversationItemId::native(
                                        uuid.clone(),
                                    )),
                                },
                            ));
                            if name == "AskUserQuestion" {
                                let request = PendingRequest {
                                    id: call_id,
                                    request_type: RequestType::Question,
                                    prompt: claude_question_prompt(block.get("input")),
                                    choices: claude_question_choices(block.get("input")),
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
            choices: vec!["Allow once".to_owned(), "Deny".to_owned()],
        };
        self.pending_request = Some(request.clone());
        vec![request_mutation(&request, RequestStatus::Pending)]
    }
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
fn safe_tool_summary(name: &str, input: Option<&Value>) -> String {
    if name == "Bash" {
        input
            .and_then(|input| input.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("Bash command")
            .to_owned()
    } else {
        input
            .and_then(|input| input.get("description"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}
fn is_empty_composer(connector: &str, line: &str) -> bool {
    let line = line.trim_start();
    let markers: &[char] = if connector == "claude" {
        &['❯']
    } else {
        &['›', '>']
    };
    markers.iter().any(|marker| {
        line.strip_prefix(*marker)
            .is_some_and(|rest| rest.trim().is_empty())
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
fn visible_choice_key(screen: &str, choice: &str) -> Option<String> {
    screen.lines().find_map(|line| {
        let line = line
            .trim_start()
            .trim_start_matches(['❯', '>'])
            .trim_start();
        let (number, label) = line.split_once('.')?;
        (label.trim().eq_ignore_ascii_case(choice)
            && matches!(
                number.trim(),
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
            ))
        .then(|| number.trim().to_owned())
    })
}

impl Connector for JsonlConnector {
    fn detect(&self) -> Detection {
        // The agent itself is supported even before it emits SessionStart. The
        // first poll advertises `starting` until its source binding arrives.
        Detection::Supported(self.identity())
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
        if self.id == "claude" {
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
                self.active_chain.clear();
                self.pending_request = None;
                self.tools.clear();
                self.tool_running = false;
                mutations.push(ConnectorMutation::Rebuild {
                    reason: "authoritative source file was replaced".to_owned(),
                });
            }
            self.source_identity = identity;
            let length = metadata.map(|meta| meta.len()).unwrap_or(0);
            if length < self.offset {
                self.offset = 0;
                self.active_chain.clear();
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
                            let parent = if self.id == "claude" {
                                value.as_object().and_then(|object| {
                                    string(object, "parentUuid")
                                        .or_else(|| string(object, "parent_uuid"))
                                })
                            } else {
                                None
                            };
                            let record_mutations =
                                self.record(value, self.offset + consumed as u64);
                            for mutation in record_mutations {
                                mutations.push(mutation);
                            }
                            if self.active_chain.len() > before
                                || self.active_chain.last().cloned() != before_tail
                            {
                                let source_id =
                                    self.active_chain.last().cloned().unwrap_or_default();
                                delta.active_branch_delta.push(super::super::BranchEntry {
                                    source_id,
                                    parent_id: parent,
                                });
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
        let refresh_screen = self.live_screen
            && self.source.is_some()
            && (!delta.source_offsets.is_empty()
                || self
                    .last_screen_refresh
                    .map(|last| last.elapsed() >= Duration::from_millis(1_500))
                    .unwrap_or(true));
        if refresh_screen {
            let screen = engine::capture_pane(&self.home, &self.session)?;
            mutations.extend(self.observe_screen(&screen));
            self.last_screen_refresh = Some(Instant::now());
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
        // touching tmux; pushed `ConversationState` is the UI availability.
        vec![
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

    fn apply(&mut self, action: ConnectorAction) -> Result<ApplyResult> {
        let text = match action.id.as_str() {
            ACTION_SEND_MESSAGE
                if self.source.is_some()
                    && self.pending_request.is_none()
                    && !self.tool_running =>
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
        // validate the live pane immediately before affecting tmux.
        let screen = engine::capture_pane(&self.home, &self.session)?;
        if action.id == ACTION_SEND_MESSAGE {
            if !screen.lines().any(|line| is_empty_composer(self.id, line)) {
                return Ok(ApplyResult::Refused {
                    reason: format!("the {} composer is no longer empty", self.id),
                });
            }
            engine::paste_message(PasteMessageRequest {
                home: &self.home,
                id: &self.session,
                message: text.as_bytes(),
            })?;
        } else {
            let request = self.pending_request.as_ref().expect("checked above");
            if !screen_contains_request(&screen, request) {
                return Ok(ApplyResult::Refused {
                    reason: "the requested Claude prompt is no longer visible".to_owned(),
                });
            }
            let Some(key) = visible_choice_key(&screen, text) else {
                return Ok(ApplyResult::Refused {
                    reason: "the requested choice is not identifiable on the current screen"
                        .to_owned(),
                });
            };
            engine::send_keys(engine::SendKeysRequest {
                home: &self.home,
                id: &self.session,
                keys: &[key],
            })?;
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
            if let Some(parent) = entry.parent_id.as_ref() {
                if let Some(index) = self.active_chain.iter().position(|id| id == parent) {
                    self.active_chain.truncate(index + 1);
                }
            }
            if self.active_chain.last() != Some(&entry.source_id) {
                self.active_chain.push(entry.source_id.clone());
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

    fn corpus(agent: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/conversation")
            .join(agent)
            .join("source-corpus.jsonl")
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
    fn claude_conforms_to_the_connector_suite() {
        conformance("claude");
    }
    #[test]
    fn codex_conforms_to_the_connector_suite() {
        conformance("codex");
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
}
