//! Codex live notifications and completed rollout records share item IDs.
//! Raw response items include setup prompts and private reasoning.
use super::*;
use crate::conversation::{
    ActivityStep, ConversationActivity as Activity, SessionUsage, TurnOutcome,
};

const EPOCH: &str = "1970-01-01T00:00:00Z";
const MAX_DIFF_FILES: usize = 16;
const MAX_DIFF_BYTES: usize = 2048;

impl JsonlConnector {
    pub(super) fn codex_conversation_item(
        &mut self,
        record: &Value,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let at = record["timestamp"].as_str().unwrap_or(EPOCH);
        let payload = &record["payload"];
        let parent = record["latchParentSubagentId"].as_str();
        let mut items = match record["method"].as_str() {
            Some("latch/codex/request/open") => {
                let Some(view) = crate::observer::codex::decode_presentation(&record["params"])
                else {
                    return Vec::new();
                };
                self.announce_codex_request(PendingRequest {
                    id: view.id,
                    request_type: if view.metadata.request_kind.as_deref() == Some("question") {
                        RequestType::Question
                    } else {
                        RequestType::Permission
                    },
                    prompt: view.prompt,
                    choices: Vec::new(),
                    questions: view.questions,
                    screen_seen: false,
                    announced_at: Some(at.to_owned()),
                    bridge_call: true,
                    metadata: view.metadata,
                })
            }
            Some("latch/codex/request/closed") => {
                use crate::conversation::ResolutionSource;
                let data = &record["params"];
                let Some(id) = data["id"].as_str() else {
                    return Vec::new();
                };
                let source = match data["source"].as_str() {
                    Some("phone") => ResolutionSource::Phone,
                    Some("terminal") => ResolutionSource::Terminal,
                    Some("timeout") => ResolutionSource::Timeout,
                    _ => return Vec::new(),
                };
                self.settle_request(id, Some(source), data["decision"].as_str())
            }
            Some("item/started" | "item/completed") => {
                let item = &record["params"]["item"];
                self.codex_item(
                    item,
                    record["method"] == "item/completed",
                    at,
                    parent,
                    ordinal,
                )
            }
            Some(method) => self.codex_event(method, &record["params"], at, parent, ordinal, true),
            None => match record["type"].as_str() {
                Some("event_msg") if payload["type"] == "item_completed" => {
                    self.codex_item(&payload["item"], true, at, parent, ordinal)
                }
                Some("event_msg") => self.codex_event(
                    payload["type"].as_str().unwrap_or_default(),
                    payload,
                    at,
                    parent,
                    ordinal,
                    false,
                ),
                Some("compacted") => {
                    self.codex_event("context_compacted", payload, at, parent, ordinal, false)
                }
                Some("token_usage_record") => {
                    self.codex_event("token_count", payload, at, parent, ordinal, false)
                }
                _ => Vec::new(),
            },
        };
        if matches!(
            record["method"]
                .as_str()
                .or_else(|| payload["type"].as_str()),
            Some(
                "turn/started"
                    | "turn/completed"
                    | "task_started"
                    | "task_complete"
                    | "turn_aborted"
                    | "latch/codex/status"
                    | "latch/codex/request/open"
                    | "latch/codex/request/closed"
            )
        ) && parent.is_none()
        {
            self.publish_codex_state(&mut items);
        }
        items
    }

    fn announce_codex_request(&mut self, request: PendingRequest) -> Vec<ConnectorMutation> {
        let mutation = request_mutation(&request, RequestStatus::Pending);
        if let Some(existing) = self
            .pending_request
            .iter_mut()
            .chain(&mut self.displaced_requests)
            .find(|existing| existing.id == request.id)
        {
            *existing = request;
        } else if let Some(previous) = self.pending_request.replace(request) {
            self.displaced_requests.push(previous);
        }
        vec![mutation]
    }

    fn publish_codex_state(&mut self, items: &mut Vec<ConnectorMutation>) {
        let state = self.state();
        if self.last_state.as_ref() != Some(&state) {
            self.last_state = Some(state.clone());
            items.push(ConnectorMutation::State(state));
        }
    }

    fn codex_event(
        &mut self,
        event: &str,
        data: &Value,
        at: &str,
        parent: Option<&str>,
        ordinal: u64,
        live: bool,
    ) -> Vec<ConnectorMutation> {
        let turn_id = data["turnId"]
            .as_str()
            .or_else(|| data["turn_id"].as_str())
            .or_else(|| data["turn"]["id"].as_str());
        let notice = |key: &str, text: &str| {
            let id = turn_id.map_or_else(|| ordinal.to_string(), str::to_owned);
            activity(
                format!("codex:{key}:{id}"),
                at,
                parent,
                Activity::Notice {
                    notice_kind: key.to_owned(),
                    text: sanitize_part(text, 640),
                },
            )
        };
        match event {
            "turn/started" | "task_started" if parent.is_none() => {
                self.turn_open = true;
                self.turn_outcome = None;
            }
            "turn/completed" | "task_complete" | "turn_aborted" if parent.is_none() => {
                self.turn_open = false;
                let outcome = match data["turn"]["status"].as_str().unwrap_or(event) {
                    "interrupted" | "turn_aborted" => TurnOutcome::Aborted,
                    "failed" => TurnOutcome::Error,
                    _ => TurnOutcome::Answer,
                };
                self.turn_outcome = Some(outcome);
                let mut items = self.finish_codex_tools(at, None);
                match outcome {
                    TurnOutcome::Aborted => {
                        items.push(notice("interrupted", "Codex turn interrupted"))
                    }
                    TurnOutcome::Error => items.push(notice(
                        error_kind(&data["turn"]["error"]),
                        data["turn"]["error"]["message"]
                            .as_str()
                            .unwrap_or("Codex turn failed"),
                    )),
                    _ => {}
                }
                return items;
            }
            "turn/completed" | "task_complete" | "turn_aborted" => {
                return self.finish_codex_tools(at, parent);
            }
            "latch/codex/status" if parent.is_none() => match data["status"]["type"].as_str() {
                Some("active") => {
                    if !self.turn_open {
                        self.turn_outcome = None;
                    }
                    self.turn_open = true;
                }
                Some("idle" | "systemError" | "notLoaded") => {
                    self.turn_open = false;
                    return self.finish_codex_tools(at, None);
                }
                _ => {}
            },
            "thread/started" => {
                let thread = &data["thread"];
                if let Some(id) = thread["id"].as_str().filter(|_| {
                    thread["parentThreadId"].is_string()
                        || thread.pointer("/source/subAgent").is_some()
                }) {
                    return vec![subagent(
                        id,
                        at,
                        parent,
                        "running",
                        thread["agentRole"].as_str().unwrap_or("Codex"),
                        thread["agentNickname"].as_str().unwrap_or(""),
                    )];
                }
            }
            "turn/plan/updated" | "plan_update" => {
                let id = turn_id.map_or_else(|| ordinal.to_string(), str::to_owned);
                return vec![activity(
                    format!("codex:plan:{id}"),
                    at,
                    parent,
                    Activity::PlanUpdate {
                        steps: plan_steps(&data["plan"]),
                    },
                )];
            }
            "item/fileChange/patchUpdated" => {
                if let Some(id) = data["itemId"].as_str() {
                    return file_changes(id, at, parent, &data["changes"]);
                }
            }
            "model/rerouted" | "model_reroute" => {
                let model = field(data, "toModel", "to_model").and_then(Value::as_str);
                let text = model.map_or_else(
                    || "Codex changed models".to_owned(),
                    |model| format!("Codex switched to {model}"),
                );
                return vec![notice("model_reroute", &text)];
            }
            "error" => {
                let error = if live { &data["error"] } else { data };
                return vec![notice(
                    error_kind(error),
                    error["message"]
                        .as_str()
                        .unwrap_or("Codex reported an error"),
                )];
            }
            "thread/compacted" | "context_compacted" => {
                return vec![notice(
                    "compaction",
                    "Codex compacted the conversation context",
                )];
            }
            "thread/tokenUsage/updated" | "token_count" if parent.is_none() => {
                let usage = if live {
                    &data["tokenUsage"]
                } else {
                    data.get("info").unwrap_or(data)
                };
                let context_tokens = usage
                    .pointer("/last/totalTokens")
                    .or_else(|| usage.pointer("/last_token_usage/total_tokens"))
                    .or_else(|| usage.get("total_tokens"))
                    .and_then(Value::as_u64);
                let context_window = field(usage, "modelContextWindow", "model_context_window")
                    .and_then(Value::as_u64);
                if context_tokens.is_some() || context_window.is_some() {
                    let context_percent = context_tokens
                        .zip(context_window)
                        .filter(|(_, window)| *window > 0)
                        .map(|(used, window)| ((used.saturating_mul(100) / window).min(100)) as u8);
                    self.activity.set_usage(SessionUsage {
                        context_tokens,
                        context_window,
                        context_percent,
                        ..SessionUsage::default()
                    });
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn codex_item(
        &mut self,
        item: &Value,
        completed: bool,
        at: &str,
        parent: Option<&str>,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let kind = item["type"].as_str().unwrap_or_default();
        let id = item["id"]
            .as_str()
            .filter(|id| id.len() <= 128)
            .map_or_else(|| format!("codex:item:{ordinal}"), str::to_owned);
        let tool = match kind {
            "commandExecution" | "CommandExecution" => "exec_command".to_owned(),
            "fileChange" | "FileChange" => "apply_patch".to_owned(),
            "mcpToolCall" | "McpToolCall" => {
                let server = item["server"].as_str().unwrap_or("MCP");
                let tool = item["tool"].as_str().unwrap_or("tool");
                sanitize_part(&format!("{server}/{tool}"), 128)
            }
            "dynamicToolCall" => sanitize_part(item["tool"].as_str().unwrap_or("Tool"), 128),
            "webSearch" | "WebSearch" => "web_search".to_owned(),
            "Extension"
                if item["kind"] == "web_search"
                    || item.get("query").is_some()
                    || item.to_string().contains("web_search") =>
            {
                "web_search".to_owned()
            }
            "imageView" | "ImageView" => "view_image".to_owned(),
            "imageGeneration" | "ImageGeneration" => "generate_image".to_owned(),
            _ if !completed => return Vec::new(),
            "userMessage" | "UserMessage" | "agentMessage" | "AgentMessage" => {
                let text = item["text"].as_str().map(str::to_owned).unwrap_or_else(|| {
                    joined_text(&item["content"], |part| {
                        matches!(part["type"].as_str(), Some("text" | "Text"))
                            .then(|| part["text"].as_str())
                            .flatten()
                    })
                });
                if text.is_empty() {
                    return Vec::new();
                }
                let role = if matches!(kind, "userMessage" | "UserMessage") {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                };
                return vec![observed(
                    id,
                    at,
                    parent,
                    ConversationItemKind::Message {
                        role,
                        text: bounded_message_text(text),
                        status: MessageStatus::Observed,
                    },
                )];
            }
            "reasoning" | "Reasoning" => {
                let summary = field(item, "summary", "summary_text").unwrap_or(&Value::Null);
                let text = summary.as_str().map(str::to_owned).unwrap_or_else(|| {
                    joined_text(summary, |part| {
                        part.as_str().or_else(|| part["text"].as_str())
                    })
                });
                if text.is_empty() {
                    return Vec::new();
                }
                return vec![activity(
                    id,
                    at,
                    parent,
                    Activity::ReasoningSummary {
                        text: bounded_message_text(text),
                    },
                )];
            }
            "plan" | "Plan" => {
                let steps = plan_steps(&item["plan"]);
                let steps = if steps.is_empty() {
                    vec![ActivityStep {
                        id: "1".to_owned(),
                        title: sanitize_part(item["text"].as_str().unwrap_or_default(), 512),
                        status: "pending".to_owned(),
                    }]
                } else {
                    steps
                };
                return vec![activity(id, at, parent, Activity::PlanUpdate { steps })];
            }
            "contextCompaction" | "ContextCompaction" => {
                return vec![activity(
                    id,
                    at,
                    parent,
                    Activity::Notice {
                        notice_kind: "compaction".to_owned(),
                        text: "Codex compacted the conversation context".to_owned(),
                    },
                )];
            }
            "collabAgentToolCall" | "CollabAgentToolCall" => {
                let states = field(item, "agentsStates", "agents_states");
                return field(item, "receiverThreadIds", "receiver_thread_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(32)
                    .filter_map(Value::as_str)
                    .map(|child| {
                        let state = states.and_then(|states| states.get(child));
                        subagent(
                            child,
                            at,
                            parent,
                            state
                                .and_then(|state| state["status"].as_str())
                                .unwrap_or("running"),
                            "Codex",
                            state
                                .and_then(|state| state["message"].as_str())
                                .unwrap_or(""),
                        )
                    })
                    .collect();
            }
            "subAgentActivity" | "SubAgentActivity" => {
                return field(item, "agentThreadId", "agent_thread_id")
                    .and_then(Value::as_str)
                    .map(|child| {
                        subagent(
                            child,
                            at,
                            parent,
                            item["kind"].as_str().unwrap_or("running"),
                            "Codex",
                            field(item, "agentPath", "agent_path")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )
                    })
                    .into_iter()
                    .collect();
            }
            _ => return Vec::new(),
        };
        let status = if !completed {
            ToolStatus::Running
        } else if matches!(
            item["status"].as_str(),
            Some("failed" | "declined" | "cancelled" | "canceled" | "interrupted")
        ) || field(item, "exitCode", "exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
            || item["success"] == false
        {
            ToolStatus::Failed
        } else {
            ToolStatus::Succeeded
        };
        let summary = tool_summary(item, &tool);
        if completed {
            self.tools.remove(&id);
            self.tool_summaries.remove(&id);
        } else if self.tools.len() < 32 || self.tools.contains_key(&id) {
            self.tools.insert(
                id.clone(),
                (tool.clone(), parent.unwrap_or_default().to_owned()),
            );
            self.tool_summaries.insert(id.clone(), summary.clone());
        } else {
            return Vec::new();
        }
        let mut items = vec![observed(
            id.clone(),
            at,
            parent,
            ConversationItemKind::Tool {
                name: tool,
                summary,
                status,
                parent_message_id: None,
            },
        )];
        if matches!(kind, "fileChange" | "FileChange") {
            items.extend(file_changes(&id, at, parent, &item["changes"]));
        }
        items
    }

    fn finish_codex_tools(&mut self, at: &str, parent: Option<&str>) -> Vec<ConnectorMutation> {
        let parent_key = parent.unwrap_or_default();
        let mut ids: Vec<_> = self
            .tools
            .iter()
            .filter(|(_, (_, owner))| owner == parent_key)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let (name, _) = self.tools.remove(&id)?;
                let summary = self.tool_summaries.remove(&id).unwrap_or_default();
                Some(observed(
                    id,
                    at,
                    parent,
                    ConversationItemKind::Tool {
                        name,
                        summary,
                        status: ToolStatus::Failed,
                        parent_message_id: None,
                    },
                ))
            })
            .collect()
    }
}

fn field<'a>(data: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    data.get(camel).or_else(|| data.get(snake))
}

fn joined_text(value: &Value, text: impl Fn(&Value) -> Option<&str>) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_kind(error: &Value) -> &'static str {
    let info = field(error, "codexErrorInfo", "codex_error_info");
    if info.is_some_and(|info| info == "usageLimitExceeded" || info.get("rateLimit").is_some())
        || error["message"].as_str().is_some_and(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("rate limit") || message.contains("usage limit")
        })
    {
        "rate_limit"
    } else {
        "error"
    }
}

fn observed(
    id: String,
    at: &str,
    parent: Option<&str>,
    kind: ConversationItemKind,
) -> ConnectorMutation {
    ConnectorMutation::Upsert(ObservedItem {
        id: ConversationItemId::native(id),
        parent_subagent_id: parent.map(subagent_id),
        created_at: at.to_owned(),
        kind,
    })
}

fn activity(id: String, at: &str, parent: Option<&str>, item: Activity) -> ConnectorMutation {
    observed(id, at, parent, ConversationItemKind::Activity(item))
}

fn subagent_id(id: &str) -> ConversationItemId {
    ConversationItemId::native(format!("codex:subagent:{id}"))
}

fn subagent(
    id: &str,
    at: &str,
    parent: Option<&str>,
    status: &str,
    agent: &str,
    summary: &str,
) -> ConnectorMutation {
    activity(
        subagent_id(id).as_str().to_owned(),
        at,
        parent.filter(|parent| *parent != id),
        Activity::Subagent {
            subagent_id: id.to_owned(),
            agent_type: sanitize_part(agent, 64),
            status: match status {
                "started" | "interacted" | "pendingInit" => "running",
                "interrupted" | "shutdown" => "stopped",
                other => other,
            }
            .chars()
            .take(32)
            .collect(),
            summary: sanitize_summary(summary),
            parent_tool_id: None,
        },
    )
}

fn plan_steps(plan: &Value) -> Vec<ActivityStep> {
    plan.as_array()
        .into_iter()
        .flatten()
        .take(32)
        .enumerate()
        .filter_map(|(index, step)| {
            Some(ActivityStep {
                id: index.to_string(),
                title: sanitize_part(
                    step["step"].as_str().or_else(|| step["title"].as_str())?,
                    256,
                ),
                status: match step["status"].as_str() {
                    Some("inProgress" | "in_progress") => "in_progress",
                    Some("completed") => "completed",
                    _ => "pending",
                }
                .to_owned(),
            })
        })
        .collect()
}

fn tool_summary(item: &Value, name: &str) -> String {
    let mut parts = Vec::new();
    for key in ["command", "cwd", "query", "path", "savedPath", "saved_path"] {
        if let Some(value) = item[key].as_str() {
            let value = sanitize_part(value, 150);
            parts.push(if key == "cwd" {
                format!("in {value}")
            } else {
                value
            });
        }
    }
    if let Some(code) = field(item, "exitCode", "exit_code").and_then(Value::as_i64) {
        parts.push(format!("exit {code}"));
    } else if let Some(status) = item["status"].as_str() {
        parts.push(sanitize_part(status, 32));
    }
    if let Some(output) = field(item, "aggregatedOutput", "aggregated_output")
        .or_else(|| item.get("stdout"))
        .and_then(Value::as_str)
        .filter(|output| !output.is_empty())
    {
        parts.push(sanitize_part(output, 112));
    }
    if parts.is_empty() {
        return name.to_owned();
    }
    sanitize_part(&parts.join(" · "), 560)
}

fn file_changes(
    id: &str,
    at: &str,
    parent: Option<&str>,
    changes: &Value,
) -> Vec<ConnectorMutation> {
    let file = |path: &str, change: &Value| {
        let path_id = ConversationItemId::derived("codex", id, path);
        let diff = field(change, "diff", "unified_diff")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut diff_summary = String::new();
        for line in diff.lines() {
            if diff_summary.len() >= MAX_DIFF_BYTES {
                diff_summary.push_str("\n…");
                break;
            }
            let (prefix, content) = match line.as_bytes().first() {
                Some(b' ') => (" ", &line[1..]),
                _ => ("", line),
            };
            let sanitized = sanitize_part(content, 256);
            if !diff_summary.is_empty() {
                diff_summary.push('\n');
            }
            diff_summary.push_str(prefix);
            diff_summary.push_str(&sanitized);
        }
        if diff_summary.len() > MAX_DIFF_BYTES {
            let mut end = MAX_DIFF_BYTES - '…'.len_utf8();
            while !diff_summary.is_char_boundary(end) {
                end -= 1;
            }
            diff_summary.truncate(end);
            diff_summary.push('…');
        }
        activity(
            path_id.as_str().to_owned(),
            at,
            parent,
            Activity::FileChange {
                path: sanitize_part(path, 256),
                diff_summary,
                tool_item_id: Some(ConversationItemId::native(id)),
            },
        )
    };
    if let Some(changes) = changes.as_array() {
        return changes
            .iter()
            .take(MAX_DIFF_FILES)
            .filter_map(|change| Some(file(change["path"].as_str()?, change)))
            .collect();
    }
    changes
        .as_object()
        .into_iter()
        .flatten()
        .take(MAX_DIFF_FILES)
        .map(|(path, change)| file(path, change))
        .collect()
}
