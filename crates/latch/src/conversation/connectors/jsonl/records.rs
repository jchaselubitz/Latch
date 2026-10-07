//! Generic JSONL record vocabulary, and the dispatch that hands each record
//! to its agent's own normalizer.
use super::*;

impl JsonlConnector {
    pub(super) fn record(&mut self, value: Value, ordinal: u64) -> Vec<ConnectorMutation> {
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
                    questions: Vec::new(),
                    screen_seen: false,
                    announced_at: None,
                    bridge_call: false,
                });
                Some(ConversationItemKind::Request {
                    request_id,
                    request_type: self.pending_request.as_ref().unwrap().request_type.clone(),
                    prompt: self.pending_request.as_ref().unwrap().prompt.clone(),
                    choices: self.pending_request.as_ref().unwrap().choices.clone(),
                    questions: Vec::new(),
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
                    questions: Vec::new(),
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
