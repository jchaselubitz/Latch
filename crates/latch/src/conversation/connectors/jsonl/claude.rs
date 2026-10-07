//! Claude transcript and hook records: the branch graph, tool calls and
//! results, permission hooks, and authoritative turn boundaries.
use super::*;

impl JsonlConnector {
    /// Normalizes the real Claude JSONL vocabulary. It intentionally lives at
    /// this boundary: Hub/protocol types never see Claude fields or its branch
    /// graph. The first item for a source UUID owns that UUID so a later rewind
    /// can target it with `TruncateAfter` without a synthetic timeline item.
    pub(super) fn claude_record(
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
            // Likewise while a request the bridge announced is shown: the
            // hook's generic prompt must not replace it, the bridge says when
            // it closes, and a further dialog of the same batch is announced
            // by the bridge, which queues it behind the shown one.
            if self
                .pending_request
                .as_ref()
                .is_some_and(|request| request.bridge_call)
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
                    .any(|block| string_value(block, "type").as_deref() == Some("tool_result"))
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
            && self.pending_request.as_ref().is_some_and(|request| {
                request.request_type == RequestType::Question || request.bridge_call
            });
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
                        self.turn_outcome = None;
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
                    if string_value(block, "type").as_deref() != Some("tool_result") {
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
                    match string_value(block, "type").as_deref() {
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
                                    questions: Vec::new(),
                                    screen_seen: false,
                                    announced_at: None,
                                    bridge_call: false,
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

    pub(super) fn claude_permission(
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
            questions: Vec::new(),
            screen_seen: false,
            announced_at: string(object, "timestamp"),
            bridge_call: false,
        };
        let mut mutations = Vec::new();
        // The hook names no call, so a later dialog cannot wait behind this
        // one; it replaces it, and the earlier item must not linger pending.
        if let Some(previous) = self
            .pending_request
            .replace(request.clone())
            .filter(|previous| previous.id != request.id)
        {
            mutations.push(request_mutation(&previous, RequestStatus::Dismissed));
        }
        mutations.push(request_mutation(&request, RequestStatus::Pending));
        mutations
    }
}

/// The record Claude writes when it compacts: a new physical root that names
/// the branch it continues.
fn claude_compaction_boundary(object: &serde_json::Map<String, Value>, event: &str) -> bool {
    object.contains_key("logicalParentUuid")
        || (event == "system" && string(object, "subtype").as_deref() == Some("compact_boundary"))
}

pub(super) fn claude_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| string_value(block, "type").as_deref() == Some("text"))
            .filter_map(|block| string_value(block, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Whether a turn close stamped `close` covers a prompt stamped `prompt`.
/// Bridge records carry milliseconds, like the transcript; the `Stop` hook
/// carries whole seconds, where only a strictly earlier second is certain.
pub(super) fn turn_closed_at_or_after(close: &str, prompt: &str) -> bool {
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
