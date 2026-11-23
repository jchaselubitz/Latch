//! Claude's activity beyond messages and tools: subagents (with their own tool
//! calls nested beneath them), the task list, notices (compaction, a failed
//! turn, a model switch), and session state (model, effort, permission mode,
//! context usage).
//!
//! Sources, all optional, so older launches simply show less:
//! - observer hooks (v4): `SubagentStart`, `SubagentStop`, `TaskCreated`,
//!   `TaskCompleted`, `PreCompact`, `PostCompact`, `StopFailure`,
//!   `PostModelSwitch`, and `permission_mode` / `effort` on every hook;
//! - the transcript: the Agent call and its `toolUseResult`, `TaskCreate`,
//!   `TaskUpdate` and `TodoWrite` calls, `task-notification` records, the
//!   model on assistant records and `permissionMode` on user records;
//! - each subagent's own transcript, `<session>/subagents/agent-<id>.jsonl`
//!   beside the main one, read incrementally and bounded;
//! - the bridge's `session.usage` record.
use super::*;
use crate::conversation::{ActivityStep, ConversationActivity, RateLimitUsage, SessionUsage};

/// Subagents remembered at once; the oldest finished ones are forgotten first.
const MAX_SUBAGENTS: usize = 32;
/// Tool calls shown under one subagent. Later calls are counted, not shown.
const MAX_SUBAGENT_TOOLS: u32 = 60;
/// Agent calls remembered for linking a subagent to the call that started it.
const MAX_AGENT_CALLS: usize = 64;
const MAX_TASKS: usize = 100;
const MAX_TASK_TITLE_CHARS: usize = 200;
const MAX_SUMMARY_BYTES: usize = 2 * 1024;
const MAX_NOTICE_DETAIL_CHARS: usize = 300;
const MAX_META_BYTES: u64 = 64 * 1024;

/// A value and when its source said so. Hooks are read before the transcript,
/// so a replayed older record must not overwrite a newer hook's value.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Stamped {
    value: String,
    at: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct SubagentTrack {
    agent_id: String,
    agent_type: String,
    description: String,
    status: String,
    /// The final answer or failure, once the subagent stopped.
    result: Option<String>,
    /// Tool uses, tokens and duration from the Agent call's result.
    stats: Option<String>,
    /// The Agent call that started it, from the subagent's meta file.
    tool_use_id: Option<String>,
    /// The Agent call's conversation item, once that call has been read.
    parent_tool_item: Option<String>,
    /// The subagent item of the subagent whose Agent call started this one.
    parent_subagent: Option<String>,
    created_at: String,
    /// The item has been sent; until then its transcript is not read either,
    /// so the subagent always precedes its own tool calls.
    emitted: bool,
    meta_read: bool,
    offset: u64,
    shown_tools: u32,
    hidden_tools: u32,
    /// Open tool calls: call id, item id, name, sanitized input summary.
    open_tools: Vec<(String, String, String, String)>,
    /// The transcript has been read to its end after the subagent finished.
    drained: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct AgentCall {
    call_id: String,
    item_id: String,
    agent_type: Option<String>,
    description: Option<String>,
    parent_subagent: Option<String>,
    linked: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct TaskEntry {
    id: String,
    title: String,
    status: String,
    at: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct ClaudeActivity {
    #[serde(default)]
    subagents: Vec<SubagentTrack>,
    #[serde(default)]
    agent_calls: Vec<AgentCall>,
    #[serde(default)]
    tasks: Vec<TaskEntry>,
    /// The task-list item of the current turn; each turn gets a fresh one so
    /// the list shows where it changed, not where it was first made.
    #[serde(default)]
    task_item: Option<(String, String)>,
    /// The compaction notice waiting for its `PostCompact`.
    #[serde(default)]
    compaction: Option<String>,
    #[serde(default)]
    model: Option<Stamped>,
    #[serde(default)]
    effort: Option<Stamped>,
    #[serde(default)]
    permission_mode: Option<Stamped>,
    #[serde(default)]
    usage: Option<SessionUsage>,
    /// Code Mode scripts, for their progress and in-script calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) scripts: Vec<super::code_mode::CodeScript>,
}

impl ClaudeActivity {
    /// The bound transcript changed: everything derived from it is replayed.
    pub(super) fn forget_source(&mut self) {
        let usage = self.usage.take();
        *self = Self::default();
        self.usage = usage;
    }

    pub(super) fn model(&self) -> Option<String> {
        self.model.as_ref().map(|value| value.value.clone())
    }

    pub(super) fn effort(&self) -> Option<String> {
        self.effort.as_ref().map(|value| value.value.clone())
    }

    pub(super) fn permission_mode(&self) -> Option<String> {
        self.permission_mode
            .as_ref()
            .map(|value| value.value.clone())
    }

    pub(super) fn usage(&self) -> Option<SessionUsage> {
        self.usage.clone()
    }

    pub(super) fn set_usage(&mut self, usage: SessionUsage) {
        self.usage = Some(usage);
    }
}

fn stamp(slot: &mut Option<Stamped>, value: &str, at: &str) {
    if value.is_empty() || value.len() > 128 {
        return;
    }
    let newer = slot.as_ref().is_none_or(|current| {
        at.get(..19).unwrap_or(at) >= current.at.get(..19).unwrap_or(&current.at)
    });
    if newer {
        *slot = Some(Stamped {
            value: value.to_owned(),
            at: at.to_owned(),
        });
    }
}

fn upsert_under(
    id: &str,
    parent_subagent: Option<&str>,
    created_at: String,
    kind: ConversationItemKind,
) -> ConnectorMutation {
    ConnectorMutation::Upsert(ObservedItem {
        parent_subagent_id: parent_subagent.map(ConversationItemId::native),
        id: ConversationItemId::native(id),
        created_at,
        kind,
    })
}

fn subagent_item_id(agent_id: &str) -> String {
    format!("subagent:{agent_id}")
}

/// Claude's agent ids are short hex; anything else is not used in a path.
fn safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    let text = text.trim();
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn clamp_percent(value: Option<&Value>) -> Option<u8> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(0.0, 100.0) as u8)
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(text[start..end].trim())
}

impl JsonlConnector {
    /// Facts every hook record and transcript record may carry about the
    /// session: its permission mode, effort and model. Only the main loop's
    /// say what the session is set to; a subagent's are its own.
    pub(super) fn observe_session_facts(
        &mut self,
        object: &serde_json::Map<String, Value>,
        event: &str,
        is_hook: bool,
    ) {
        let at = string(object, "timestamp").unwrap_or_default();
        if at.is_empty() || object.contains_key("agent_id") {
            return;
        }
        if is_hook {
            if let Some(mode) = object.get("permission_mode").and_then(Value::as_str) {
                stamp(&mut self.activity.permission_mode, mode, &at);
            }
            if let Some(level) = object
                .get("effort")
                .and_then(|effort| effort.get("level"))
                .and_then(Value::as_str)
            {
                stamp(&mut self.activity.effort, level, &at);
            }
            // `SessionStart` names the model the session starts or resumes on.
            if let Some(model) = object.get("model").and_then(Value::as_str) {
                stamp(&mut self.activity.model, model, &at);
            }
            return;
        }
        if object
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return;
        }
        match event {
            "user" => {
                if let Some(mode) = object.get("permissionMode").and_then(Value::as_str) {
                    stamp(&mut self.activity.permission_mode, mode, &at);
                }
            }
            "assistant" => {
                if let Some(model) = object
                    .get("message")
                    .and_then(|message| message.get("model"))
                    .and_then(Value::as_str)
                    .filter(|model| !model.starts_with('<'))
                {
                    stamp(&mut self.activity.model, model, &at);
                }
            }
            _ => {}
        }
    }

    /// The observer's activity hooks. `None` for any other hook.
    pub(super) fn claude_activity_hook(
        &mut self,
        object: &serde_json::Map<String, Value>,
        hook_event_name: &str,
        ordinal: u64,
    ) -> Option<Vec<ConnectorMutation>> {
        let at = string(object, "timestamp").unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
        let mutations = match hook_event_name {
            "SubagentStart" => self.subagent_started(object, &at),
            "SubagentStop" => self.subagent_stopped(object, &at),
            "TaskCreated" | "TaskCompleted" => {
                let id = string(object, "task_id")?;
                let title = string(object, "task_subject").unwrap_or_default();
                let status = if hook_event_name == "TaskCreated" {
                    "pending"
                } else {
                    "completed"
                };
                self.update_task(&id, Some(&title), Some(status), &at, Some(ordinal))
            }
            "PreCompact" => {
                let id = format!("notice:compact:{ordinal}");
                self.activity.compaction = Some(id.clone());
                let text = if string(object, "trigger").as_deref() == Some("auto") {
                    "Compacting the conversation automatically…"
                } else {
                    "Compacting the conversation…"
                };
                vec![notice(&id, at, "compaction", text.to_owned())]
            }
            "PostCompact" => {
                let id = self
                    .activity
                    .compaction
                    .take()
                    .unwrap_or_else(|| format!("notice:compact:{ordinal}"));
                let text = if string(object, "trigger").as_deref() == Some("auto") {
                    "Conversation compacted automatically"
                } else {
                    "Conversation compacted"
                };
                vec![notice(&id, at, "compaction", text.to_owned())]
            }
            "StopFailure" => {
                // The turn ended in an API error; no `Stop` follows it.
                self.turn_open = false;
                self.tool_running = false;
                self.last_turn_close = Some(at.clone());
                self.turn_outcome = Some(crate::conversation::TurnOutcome::Error);
                let error = string(object, "error").unwrap_or_else(|| "unknown".to_owned());
                let (kind, headline) = match error.as_str() {
                    "rate_limit" => ("rate_limit", "Rate limit reached".to_owned()),
                    "billing_error" => ("billing_error", "Billing error".to_owned()),
                    "authentication_failed" => ("api_error", "Authentication failed".to_owned()),
                    "server_error" => ("api_error", "Claude's API had a server error".to_owned()),
                    "max_output_tokens" => (
                        "api_error",
                        "The response reached its output limit".to_owned(),
                    ),
                    "model_not_found" => ("api_error", "The model is not available".to_owned()),
                    _ => ("api_error", "The turn failed".to_owned()),
                };
                // The engine's own words for it, when it gave any.
                let detail = ["error_details", "last_assistant_message"]
                    .into_iter()
                    .find_map(|key| object.get(key).and_then(Value::as_str))
                    .map(|detail| sanitize_part(detail, MAX_NOTICE_DETAIL_CHARS))
                    .filter(|detail| !detail.is_empty());
                let text = match detail {
                    Some(detail) => format!("{headline}: {detail}"),
                    None => headline,
                };
                let mut mutations = self.dismiss_terminal_only();
                mutations.push(notice(
                    &format!("notice:stop-failure:{ordinal}"),
                    at,
                    kind,
                    text,
                ));
                mutations
            }
            "PostModelSwitch" => {
                let to = string(object, "to_model")?;
                stamp(&mut self.activity.model, &to, &at);
                let text = match string(object, "from_model").filter(|from| from != &to) {
                    Some(from) => format!(
                        "Switched model to {} from {}",
                        sanitize_part(&to, 64),
                        sanitize_part(&from, 64)
                    ),
                    None => format!("Switched model to {}", sanitize_part(&to, 64)),
                };
                vec![notice(
                    &format!("notice:model:{ordinal}"),
                    at,
                    "model_switch",
                    text,
                )]
            }
            _ => return None,
        };
        Some(mutations)
    }

    /// The bridge's `session.usage` record: the status line's figures.
    pub(super) fn bridge_usage(&mut self, object: &serde_json::Map<String, Value>) {
        if let (Some(model), Some(at)) = (string(object, "model"), string(object, "timestamp")) {
            stamp(&mut self.activity.model, &model, &at);
        }
        let rate_limits = object
            .get("rate_limits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(8)
            .filter_map(|limit| {
                Some(RateLimitUsage {
                    kind: string_value(limit, "kind")
                        .filter(|kind| !kind.is_empty() && kind.len() <= 64)?,
                    percent_used: clamp_percent(limit.get("percent_used"))?,
                    resets_at: string_value(limit, "resets_at").filter(|at| at.len() <= 64),
                })
            })
            .collect();
        self.activity.usage = Some(SessionUsage {
            context_tokens: object.get("context_tokens").and_then(Value::as_u64),
            context_window: object.get("context_window").and_then(Value::as_u64),
            context_percent: clamp_percent(object.get("context_percent")),
            rate_limits,
            cost_cents: object
                .get("cost_usd")
                .and_then(Value::as_f64)
                .filter(|usd| usd.is_finite() && *usd >= 0.0)
                .map(|usd| (usd * 100.0).round() as u64),
        });
    }

    /// A real prompt starts a turn: its task list is a fresh item.
    pub(super) fn activity_turn_started(&mut self) {
        self.activity.task_item = None;
    }

    /// A tool call read from the main transcript or a subagent's.
    pub(super) fn activity_tool_use(
        &mut self,
        name: &str,
        call_id: &str,
        item_id: &str,
        input: Option<&Value>,
        at: &str,
        parent_subagent: Option<&str>,
    ) -> Vec<ConnectorMutation> {
        match name {
            "Agent" | "Task" => {
                if self.activity.agent_calls.len() >= MAX_AGENT_CALLS {
                    self.activity.agent_calls.remove(0);
                }
                self.activity.agent_calls.push(AgentCall {
                    call_id: call_id.to_owned(),
                    item_id: item_id.to_owned(),
                    agent_type: input.and_then(|input| string_value(input, "subagent_type")),
                    description: input.and_then(|input| string_value(input, "description")),
                    parent_subagent: parent_subagent.map(str::to_owned),
                    linked: false,
                });
                self.link_subagents()
            }
            // Subagents keep their own lists; only the main loop's is shown.
            _ if parent_subagent.is_some() => Vec::new(),
            "TaskUpdate" => {
                let Some(id) = input.and_then(|input| {
                    string_value(input, "taskId").or_else(|| string_value(input, "task_id"))
                }) else {
                    return Vec::new();
                };
                let title = input.and_then(|input| string_value(input, "subject"));
                let status = input.and_then(|input| string_value(input, "status"));
                self.update_task(&id, title.as_deref(), status.as_deref(), at, None)
            }
            "TodoWrite" => {
                let Some(todos) = input
                    .and_then(|input| input.get("todos"))
                    .and_then(Value::as_array)
                else {
                    return Vec::new();
                };
                self.activity.tasks = todos
                    .iter()
                    .take(MAX_TASKS)
                    .enumerate()
                    .filter_map(|(index, todo)| {
                        Some(TaskEntry {
                            id: string_value(todo, "id")
                                .unwrap_or_else(|| format!("todo-{}", index + 1)),
                            title: sanitize_part(
                                &string_value(todo, "content")?,
                                MAX_TASK_TITLE_CHARS,
                            ),
                            status: string_value(todo, "status")
                                .unwrap_or_else(|| "pending".to_owned()),
                            at: at.to_owned(),
                        })
                    })
                    .collect();
                self.task_list_mutation(at)
            }
            _ => Vec::new(),
        }
    }

    /// A main-transcript tool result: `record` is the whole user record,
    /// whose `toolUseResult` describes the result when it holds only one.
    pub(super) fn activity_tool_result(
        &mut self,
        name: &str,
        record: &serde_json::Map<String, Value>,
        is_error: bool,
        at: &str,
    ) -> Vec<ConnectorMutation> {
        let result = record.get("toolUseResult");
        match name {
            "TaskCreate" => {
                let Some(task) = result.and_then(|result| result.get("task")) else {
                    return Vec::new();
                };
                let Some(id) = string_value(task, "id") else {
                    return Vec::new();
                };
                let title = string_value(task, "subject");
                self.update_task(&id, title.as_deref(), Some("pending"), at, None)
            }
            "Agent" | "Task" => {
                let Some(agent_id) = result.and_then(|result| string_value(result, "agentId"))
                else {
                    return Vec::new();
                };
                let status = result.and_then(|result| string_value(result, "status"));
                let Some(track) = self
                    .activity
                    .subagents
                    .iter_mut()
                    .find(|track| track.agent_id == agent_id)
                else {
                    return Vec::new();
                };
                let mut stats = Vec::new();
                if let Some(count) = result
                    .and_then(|result| result.get("totalToolUseCount"))
                    .and_then(Value::as_u64)
                {
                    stats.push(format!(
                        "{count} tool use{}",
                        if count == 1 { "" } else { "s" }
                    ));
                }
                if let Some(tokens) = result
                    .and_then(|result| result.get("totalTokens"))
                    .and_then(Value::as_u64)
                {
                    stats.push(if tokens >= 1000 {
                        format!("{:.1}k tokens", tokens as f64 / 1000.0)
                    } else {
                        format!("{tokens} tokens")
                    });
                }
                if let Some(ms) = result
                    .and_then(|result| result.get("totalDurationMs"))
                    .and_then(Value::as_u64)
                {
                    stats.push(format!("{}s", ms.div_ceil(1000)));
                }
                if !stats.is_empty() {
                    track.stats = Some(stats.join(" · "));
                }
                if is_error {
                    track.status = "failed".to_owned();
                } else if status.as_deref() == Some("completed") {
                    track.status = "completed".to_owned();
                }
                self.subagent_mutation(&agent_id)
            }
            _ => Vec::new(),
        }
    }

    /// A `task-notification` user record: the engine telling the model a
    /// background task ended. It is not something the person typed.
    pub(super) fn is_task_notification(
        object: &serde_json::Map<String, Value>,
        text: &str,
    ) -> bool {
        object
            .get("origin")
            .and_then(|origin| origin.get("kind"))
            .and_then(Value::as_str)
            == Some("task-notification")
            || text.trim_start().starts_with("<task-notification>")
    }

    pub(super) fn task_notification(&mut self, text: &str) -> Vec<ConnectorMutation> {
        let Some(id) = tag(text, "task-id") else {
            return Vec::new();
        };
        let status = match tag(text, "status") {
            Some("completed") => "completed",
            Some("failed") => "failed",
            Some("killed") | Some("stopped") | Some("cancelled") => "cancelled",
            _ => return Vec::new(),
        };
        let id = id.to_owned();
        let Some(track) = self
            .activity
            .subagents
            .iter_mut()
            .find(|track| track.agent_id == id)
        else {
            return Vec::new();
        };
        track.status = status.to_owned();
        self.subagent_mutation(&id)
    }

    fn subagent_started(
        &mut self,
        object: &serde_json::Map<String, Value>,
        at: &str,
    ) -> Vec<ConnectorMutation> {
        let Some(agent_id) = self.track_subagent(object, at) else {
            return Vec::new();
        };
        let track = self
            .activity
            .subagents
            .iter_mut()
            .find(|track| track.agent_id == agent_id)
            .expect("tracked above");
        // A background subagent resumed by a message runs again.
        track.status = "running".to_owned();
        track.drained = false;
        let mut mutations = self.link_subagents();
        mutations.extend(self.subagent_mutation(&agent_id));
        mutations
    }

    /// The id of the subagent a hook names, tracking it if it is new.
    fn track_subagent(
        &mut self,
        object: &serde_json::Map<String, Value>,
        at: &str,
    ) -> Option<String> {
        let agent_id = string(object, "agent_id").filter(|id| safe_agent_id(id))?;
        if self
            .activity
            .subagents
            .iter()
            .any(|track| track.agent_id == agent_id)
        {
            return Some(agent_id);
        }
        if self.activity.subagents.len() >= MAX_SUBAGENTS {
            let oldest_finished = self
                .activity
                .subagents
                .iter()
                .position(|track| track.status != "running")
                .unwrap_or(0);
            self.activity.subagents.remove(oldest_finished);
        }
        self.activity.subagents.push(SubagentTrack {
            agent_type: string(object, "agent_type").unwrap_or_default(),
            agent_id: agent_id.clone(),
            status: "running".to_owned(),
            created_at: at.to_owned(),
            ..SubagentTrack::default()
        });
        Some(agent_id)
    }

    fn subagent_stopped(
        &mut self,
        object: &serde_json::Map<String, Value>,
        at: &str,
    ) -> Vec<ConnectorMutation> {
        // Its start may predate this observer, or have been forgotten. An
        // untyped stop with no start is the engine's own fork (compaction),
        // not a subagent the model started.
        let known = string(object, "agent_id").is_some_and(|id| {
            self.activity
                .subagents
                .iter()
                .any(|track| track.agent_id == id)
        });
        if !known && string(object, "agent_type").unwrap_or_default().is_empty() {
            return Vec::new();
        }
        let Some(agent_id) = self.track_subagent(object, at) else {
            return Vec::new();
        };
        let Some(track) = self
            .activity
            .subagents
            .iter_mut()
            .find(|track| track.agent_id == agent_id)
        else {
            return Vec::new();
        };
        if track.status == "running" {
            track.status = "completed".to_owned();
        }
        if let Some(message) = string(object, "last_assistant_message").filter(|m| !m.is_empty()) {
            track.result = Some(bounded_text(&message, MAX_SUMMARY_BYTES));
        }
        // Not sent before its Agent call is: on a replay every hook is read
        // before the transcript. One whose call is never read is sent once
        // the sources are caught up (`read_subagent_transcripts`).
        let mut mutations = self.link_subagents();
        mutations.extend(self.subagent_mutation(&agent_id));
        mutations
    }

    /// Joins started subagents to the Agent calls that started them, and
    /// sends each subagent once it has found its call.
    fn link_subagents(&mut self) -> Vec<ConnectorMutation> {
        let source_dir = self.subagents_dir();
        let mut ready = Vec::new();
        for index in 0..self.activity.subagents.len() {
            if self.activity.subagents[index].parent_tool_item.is_some() {
                continue;
            }
            if !self.activity.subagents[index].meta_read {
                if let Some(dir) = source_dir.as_ref() {
                    let agent_id = self.activity.subagents[index].agent_id.clone();
                    if let Some(meta) = read_meta(dir, &agent_id) {
                        let track = &mut self.activity.subagents[index];
                        track.meta_read = true;
                        track.tool_use_id = string_value(&meta, "toolUseId");
                        if let Some(description) = string_value(&meta, "description") {
                            track.description = sanitize_part(&description, MAX_TASK_TITLE_CHARS);
                        }
                        if track.agent_type.is_empty() {
                            track.agent_type = string_value(&meta, "agentType").unwrap_or_default();
                        }
                    }
                }
            }
            let track = &self.activity.subagents[index];
            let call = match track.tool_use_id.as_deref() {
                Some(call_id) => self
                    .activity
                    .agent_calls
                    .iter()
                    .position(|call| call.call_id == call_id),
                // Without a meta file: the oldest unclaimed call of its type.
                None => self.activity.agent_calls.iter().position(|call| {
                    !call.linked
                        && call
                            .agent_type
                            .as_deref()
                            .is_none_or(|kind| kind == track.agent_type)
                }),
            };
            let Some(call) = call else { continue };
            let call = &mut self.activity.agent_calls[call];
            call.linked = true;
            let (item_id, parent_subagent, description) = (
                call.item_id.clone(),
                call.parent_subagent.clone(),
                call.description.clone(),
            );
            let track = &mut self.activity.subagents[index];
            track.parent_tool_item = Some(item_id);
            track.parent_subagent = parent_subagent;
            if track.description.is_empty() {
                if let Some(description) = description {
                    track.description = sanitize_part(&description, MAX_TASK_TITLE_CHARS);
                }
            }
            track.emitted = true;
            ready.push(track.agent_id.clone());
        }
        ready
            .into_iter()
            .flat_map(|agent_id| self.subagent_mutation(&agent_id))
            .collect()
    }

    fn subagent_mutation(&self, agent_id: &str) -> Vec<ConnectorMutation> {
        let Some(track) = self
            .activity
            .subagents
            .iter()
            .find(|track| track.agent_id == agent_id && track.emitted)
        else {
            return Vec::new();
        };
        let mut summary = match track.result.as_deref() {
            Some(result) if !track.description.is_empty() => {
                format!("{}\n\n{result}", track.description)
            }
            Some(result) => result.to_owned(),
            None => track.description.clone(),
        };
        if let Some(stats) = &track.stats {
            summary = format!("{summary}\n\n{stats}");
        }
        if track.hidden_tools > 0 {
            summary = format!(
                "{summary}\n\n{} more tool call{} not shown",
                track.hidden_tools,
                if track.hidden_tools == 1 { "" } else { "s" }
            );
        }
        vec![upsert_under(
            &subagent_item_id(&track.agent_id),
            track.parent_subagent.as_deref(),
            track.created_at.clone(),
            ConversationItemKind::Activity(ConversationActivity::Subagent {
                subagent_id: track.agent_id.clone(),
                agent_type: track.agent_type.clone(),
                status: track.status.clone(),
                summary: summary.trim().to_owned(),
                parent_tool_id: track
                    .parent_tool_item
                    .as_deref()
                    .map(ConversationItemId::native),
            }),
        )]
    }

    /// `<transcript dir>/<session id>/subagents`, beside the bound transcript.
    fn subagents_dir(&self) -> Option<PathBuf> {
        let source = self.source.as_ref()?;
        let stem = source.file_stem()?;
        Some(source.parent()?.join(stem).join("subagents"))
    }

    /// Reads what each shown, still-active subagent's transcript gained since
    /// the last poll, within the shared budget.
    pub(super) fn read_subagent_transcripts(
        &mut self,
        budget: &mut super::source::SourceBudget,
        mutations: &mut Vec<ConnectorMutation>,
    ) {
        let Some(dir) = self.subagents_dir() else {
            return;
        };
        if !budget.has_more {
            // Caught up, and still no Agent call names it: show it alone.
            let orphans: Vec<String> = self
                .activity
                .subagents
                .iter_mut()
                .filter(|track| !track.emitted && track.status != "running")
                .map(|track| {
                    track.emitted = true;
                    track.agent_id.clone()
                })
                .collect();
            for agent_id in orphans {
                mutations.extend(self.subagent_mutation(&agent_id));
            }
        }
        for index in 0..self.activity.subagents.len() {
            let track = &self.activity.subagents[index];
            if !track.emitted || track.drained {
                continue;
            }
            let path = dir.join(format!("agent-{}.jsonl", track.agent_id));
            let length = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            let finished = track.status != "running";
            if length <= track.offset {
                if finished {
                    self.activity.subagents[index].drained = true;
                }
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let Ok(bytes) = read_bounded(file, track.offset) else {
                continue;
            };
            let complete = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1);
            let hidden = self.activity.subagents[index].hidden_tools;
            let mut consumed = 0usize;
            for line in bytes[..complete].split_inclusive(|byte| *byte == b'\n') {
                if !budget.take(line.len()) {
                    break;
                }
                consumed += line.len();
                if line.len() > MAX_RECORD_BYTES {
                    continue;
                }
                if let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(line) {
                    mutations.extend(self.subagent_record(index, &record));
                }
            }
            if self.activity.subagents[index].hidden_tools != hidden {
                let agent_id = self.activity.subagents[index].agent_id.clone();
                mutations.extend(self.subagent_mutation(&agent_id));
            }
            budget.note_read(
                consumed,
                complete,
                bytes.len(),
                length - self.activity.subagents[index].offset,
            );
            self.activity.subagents[index].offset += consumed as u64;
        }
    }

    /// One record of a subagent's own transcript: its tool calls and their
    /// results, nested beneath the subagent's item.
    fn subagent_record(
        &mut self,
        index: usize,
        record: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        let event = string(record, "type").unwrap_or_default();
        let Some(uuid) = string(record, "uuid") else {
            return Vec::new();
        };
        // A Code Mode script's calls are told by its Code item.
        if super::code_mode::is_script_record(record) {
            return Vec::new();
        }
        let at = string(record, "timestamp").unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
        let parent = subagent_item_id(&self.activity.subagents[index].agent_id);
        let blocks = record
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut mutations = Vec::new();
        for (block_index, block) in blocks.iter().enumerate() {
            match (event.as_str(), string_value(block, "type").as_deref()) {
                ("assistant", Some("tool_use")) => {
                    let track = &mut self.activity.subagents[index];
                    if track.shown_tools >= MAX_SUBAGENT_TOOLS {
                        track.hidden_tools += 1;
                        continue;
                    }
                    track.shown_tools += 1;
                    let item_id = format!("{parent}:{uuid}:{block_index}");
                    let call_id = string_value(block, "id").unwrap_or_else(|| item_id.clone());
                    let mut name = string_value(block, "name").unwrap_or_else(|| "tool".to_owned());
                    let summary = if name == super::code_mode::SCRIPT_TOOL {
                        name = super::code_mode::CODE_ITEM_NAME.to_owned();
                        super::code_mode::script_description(block.get("input"))
                    } else {
                        safe_tool_summary(&name, block.get("input"))
                    };
                    track.open_tools.push((
                        call_id.clone(),
                        item_id.clone(),
                        name.clone(),
                        summary.clone(),
                    ));
                    mutations.push(upsert_under(
                        &item_id,
                        Some(&parent),
                        at.clone(),
                        ConversationItemKind::Tool {
                            name: name.clone(),
                            summary,
                            status: ToolStatus::Running,
                            parent_message_id: None,
                        },
                    ));
                    mutations.extend(self.activity_tool_use(
                        &name,
                        &call_id,
                        &item_id,
                        block.get("input"),
                        &at,
                        Some(&parent),
                    ));
                }
                ("user", Some("tool_result")) => {
                    let Some(call_id) = string_value(block, "tool_use_id") else {
                        continue;
                    };
                    let track = &mut self.activity.subagents[index];
                    let Some(position) = track
                        .open_tools
                        .iter()
                        .position(|(open, ..)| open == &call_id)
                    else {
                        continue;
                    };
                    let (_, item_id, name, input) = track.open_tools.remove(position);
                    let (status, summary) = if name == super::code_mode::CODE_ITEM_NAME {
                        super::code_mode::script_outcome(&input, block, record.get("toolUseResult"))
                    } else {
                        claude_tool_outcome(&input, block)
                    };
                    mutations.push(upsert_under(
                        &item_id,
                        Some(&parent),
                        at.clone(),
                        ConversationItemKind::Tool {
                            name,
                            summary,
                            status,
                            parent_message_id: None,
                        },
                    ));
                }
                _ => {}
            }
        }
        mutations
    }

    /// Applies one task change, if it is not older than what is known.
    ///
    /// A hook (`hook_ordinal`) only refreshes a list the transcript already
    /// placed in this turn: hooks are read ahead of the transcript, so a list
    /// a hook started would sit above the prompt that asked for it.
    fn update_task(
        &mut self,
        id: &str,
        title: Option<&str>,
        status: Option<&str>,
        at: &str,
        hook_ordinal: Option<u64>,
    ) -> Vec<ConnectorMutation> {
        let changed = self.apply_task(id, title, status, at);
        let placed = self.activity.task_item.is_some();
        match hook_ordinal {
            Some(_) if changed && placed => self.task_list_mutation(at),
            Some(_) => Vec::new(),
            None if changed || !placed => self.task_list_mutation(at),
            None => Vec::new(),
        }
    }

    /// Whether the change altered the list.
    fn apply_task(
        &mut self,
        id: &str,
        title: Option<&str>,
        status: Option<&str>,
        at: &str,
    ) -> bool {
        let at_second = at.get(..19).unwrap_or(at);
        if status == Some("deleted") {
            let before = self.activity.tasks.len();
            self.activity.tasks.retain(|task| task.id != id);
            return self.activity.tasks.len() != before;
        }
        let position = self.activity.tasks.iter().position(|task| task.id == id);
        let is_new = position.is_none();
        let position = match position {
            Some(position) => {
                let task = &self.activity.tasks[position];
                if at_second < task.at.get(..19).unwrap_or(&task.at) {
                    return false;
                }
                position
            }
            None => {
                if self.activity.tasks.len() >= MAX_TASKS {
                    return false;
                }
                self.activity.tasks.push(TaskEntry {
                    id: id.to_owned(),
                    title: format!("Task {id}"),
                    status: "pending".to_owned(),
                    at: at.to_owned(),
                });
                self.activity.tasks.len() - 1
            }
        };
        let task = &mut self.activity.tasks[position];
        let before = task.clone();
        if let Some(title) = title.filter(|title| !title.is_empty()) {
            task.title = sanitize_part(title, MAX_TASK_TITLE_CHARS);
        }
        if let Some(status) = status.filter(|status| !status.is_empty() && status.len() <= 32) {
            // A completion is not undone by the creation it follows.
            if !(task.status == "completed" && status == "pending") {
                task.status = status.to_owned();
            }
        }
        task.at = at.to_owned();
        is_new || task.title != before.title || task.status != before.status
    }

    fn task_list_mutation(&mut self, at: &str) -> Vec<ConnectorMutation> {
        let (id, created_at) = match &self.activity.task_item {
            Some(item) => item.clone(),
            None => {
                let item = (format!("tasks:{at}"), at.to_owned());
                self.activity.task_item = Some(item.clone());
                item
            }
        };
        vec![upsert_under(
            &id,
            None,
            created_at,
            ConversationItemKind::Activity(ConversationActivity::TaskList {
                tasks: self
                    .activity
                    .tasks
                    .iter()
                    .map(|task| ActivityStep {
                        id: task.id.clone(),
                        title: task.title.clone(),
                        status: task.status.clone(),
                    })
                    .collect(),
            }),
        )]
    }
}

fn notice(id: &str, at: String, kind: &str, text: String) -> ConnectorMutation {
    upsert_under(
        id,
        None,
        at,
        ConversationItemKind::Activity(ConversationActivity::Notice {
            notice_kind: kind.to_owned(),
            text,
        }),
    )
}

/// The subagent's meta file, which names the Agent call that started it.
fn read_meta(dir: &std::path::Path, agent_id: &str) -> Option<Value> {
    let path = dir.join(format!("agent-{agent_id}.meta.json"));
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_META_BYTES).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}
