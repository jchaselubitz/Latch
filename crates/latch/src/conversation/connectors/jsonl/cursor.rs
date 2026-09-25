//! Cursor's native JSONL transcript contains role + message.content records.
//! Native hooks carry tool ids and turn boundaries separately. Keep these
//! vocabularies here; never interpret Cursor terminal output as a transcript.
use super::*;

impl JsonlConnector {
    pub(super) fn cursor_record(
        &mut self,
        object: &serde_json::Map<String, Value>,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        if let Some(event) = string(object, "hook_event_name") {
            // Subagent hooks are not part of the main conversation.
            if object.get("is_subagent").and_then(Value::as_bool) == Some(true)
                || object
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
            {
                return Vec::new();
            }
            match event.as_str() {
                "beforeSubmitPrompt" => self.turn_open = true,
                "stop" | "sessionEnd" => {
                    self.turn_open = false;
                    self.tool_running = false;
                    return self
                        .tools
                        .drain()
                        .map(|(call_id, (name, _))| {
                            ConnectorMutation::Upsert(ObservedItem {
                                id: ConversationItemId::derived("cursor", "tool", &call_id),
                                created_at: string(object, "timestamp")
                                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                                kind: ConversationItemKind::Tool {
                                    name,
                                    summary: "Turn stopped before a tool result was observed"
                                        .to_owned(),
                                    status: ToolStatus::Failed,
                                    parent_message_id: None,
                                },
                            })
                        })
                        .collect();
                }
                "preToolUse" | "postToolUse" | "postToolUseFailure" => {
                    self.turn_open = true;
                    let Some(call_id) = string(object, "tool_use_id").filter(|id| !id.is_empty())
                    else {
                        return Vec::new();
                    };
                    let name = string(object, "tool_name").unwrap_or_else(|| "tool".to_owned());
                    let running = event == "preToolUse";
                    if running {
                        self.tools
                            .insert(call_id.clone(), (name.clone(), String::new()));
                    } else {
                        self.tools.remove(&call_id);
                    }
                    self.tool_running = !self.tools.is_empty();
                    let summary = object
                        .get("tool_input")
                        .and_then(|input| input.get("command").or_else(|| input.get("path")))
                        .and_then(Value::as_str)
                        .unwrap_or(&name);
                    let summary = sanitize_summary(summary);
                    return vec![ConnectorMutation::Upsert(ObservedItem {
                        id: ConversationItemId::derived("cursor", "tool", &call_id),
                        created_at: string(object, "timestamp")
                            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                        kind: ConversationItemKind::Tool {
                            name,
                            summary,
                            status: if running {
                                ToolStatus::Running
                            } else if event == "postToolUseFailure" {
                                ToolStatus::Failed
                            } else {
                                ToolStatus::Succeeded
                            },
                            parent_message_id: None,
                        },
                    })];
                }
                _ => {}
            }
            return Vec::new();
        }
        let role = match string(object, "role").as_deref() {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            _ => return Vec::new(),
        };
        let text = claude_text(
            object
                .get("message")
                .and_then(|message| message.get("content")),
        );
        if text.trim().is_empty() {
            return Vec::new();
        }
        // Cursor has no native message id in this file. Byte offsets are
        // stable across append/restart; source replacement resets the Hub.
        let id = ConversationItemId::derived("cursor", "message", &ordinal.to_string());
        vec![ConnectorMutation::Upsert(ObservedItem {
            id,
            created_at: string(object, "timestamp")
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            kind: ConversationItemKind::Message {
                role,
                text: bounded_message_text(text),
                status: MessageStatus::Observed,
            },
        })]
    }
}

pub(super) fn is_empty_composer(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix('→') else {
        return false;
    };
    matches!(
        rest.trim(),
        "" | "Plan, search, build anything" | "Add a follow-up"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(connector: &mut JsonlConnector, event: &str) {
        connector.record(json!({"hook_event_name": event}), 1);
    }

    #[test]
    fn cursor_messages_ignore_tool_blocks_and_internal_roles() {
        let mut connector = JsonlConnector::fixture("cursor", PathBuf::from("unused"));
        for role in ["user", "assistant"] {
            let result = connector.record(
                json!({"role": role, "message": {"content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "tool_use", "name": "Shell", "input": {"command": "secret"}}
                ]}}),
                10,
            );
            assert!(
                matches!(&result[0], ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Message { text, .. }, ..
            }) if text == "Hello")
            );
        }
        assert!(connector
            .record(
                json!({"role": "system", "message": {"content": "private"}}),
                20
            )
            .is_empty());
        assert!(connector
            .record(json!({"type": "turn_ended", "status": "success"}), 30)
            .is_empty());
    }

    #[test]
    fn cursor_turn_and_tool_state_survive_restart() {
        let mut connector = JsonlConnector::fixture("cursor", PathBuf::from("unused"));
        hook(&mut connector, "beforeSubmitPrompt");
        let call = json!({"hook_event_name": "preToolUse", "tool_use_id": "call-1", "tool_name": "Shell", "tool_input": {"command": "echo ok"}});
        let start = connector.record(call.clone(), 1);
        let checkpoint = connector.checkpoint_snapshot().unwrap();
        let mut restored = JsonlConnector::fixture("cursor", PathBuf::from("unused"));
        restored.restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(restored.state().phase, ConversationPhase::Working);
        let mut end = call;
        end["hook_event_name"] = json!("postToolUseFailure");
        let finish = restored.record(end, 2);
        match (&start[0], &finish[0]) {
            (ConnectorMutation::Upsert(a), ConnectorMutation::Upsert(b)) => {
                assert_eq!(a.id, b.id);
                assert!(matches!(
                    b.kind,
                    ConversationItemKind::Tool {
                        status: ToolStatus::Failed,
                        ..
                    }
                ));
            }
            _ => panic!("expected tool upserts"),
        }
        assert_eq!(restored.state().phase, ConversationPhase::Working);
        hook(&mut restored, "stop");
        restored.observe_screen("  → Add a follow-up  ");
        assert!(restored.state().send_message.enabled);
    }

    #[test]
    fn cursor_startup_requires_its_real_empty_composer() {
        let mut connector = JsonlConnector::fixture("cursor", PathBuf::from("unused"));
        connector.source = None;
        assert!(!connector.state().send_message.enabled);
        connector.observe_screen("  → Plan, search, build anything  ");
        assert!(connector.state().send_message.enabled);
        hook(&mut connector, "beforeSubmitPrompt");
        assert!(!connector.state().send_message.enabled);
        assert_eq!(connector.state().phase, ConversationPhase::Working);
        for text in [
            "→ draft",
            ">",
            "›",
            "→ Run a command",
            "text → Add a follow-up",
        ] {
            assert!(!is_empty_composer(text), "{text}");
        }
    }

    #[test]
    fn cursor_stop_closes_unfinished_tools() {
        let mut connector = JsonlConnector::fixture("cursor", PathBuf::from("unused"));
        connector.record(json!({"hook_event_name": "preToolUse", "tool_use_id": "unfinished", "tool_name": "Shell"}), 1);
        let result = connector.record(json!({"hook_event_name": "stop", "status": "aborted"}), 2);
        assert!(matches!(
            &result[0],
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Tool {
                    status: ToolStatus::Failed,
                    ..
                },
                ..
            })
        ));
        assert!(!connector.tool_running);
        assert!(!connector.turn_open);
    }

    #[test]
    fn cursor_poll_checkpoints_transcript_and_hooks_independently() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let home = LatchHome::new(temp.path());
        let session = SessionId::parse("ses_cursorfixture").unwrap();
        let paths = home.session(&session);
        paths.ensure().unwrap();
        let source = temp.path().join("cursor.jsonl");
        fs::write(&source, "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Hello\"}]}}\n").unwrap();
        fs::write(
            paths.conversation_source_binding(),
            serde_json::to_vec(
                &json!({"connector":"cursor","source":source,"agentSessionId":"fixture"}),
            )
            .unwrap(),
        )
        .unwrap();
        fs::write(
            paths.conversation_source_hooks(),
            "{\"hook_event_name\":\"beforeSubmitPrompt\"}\n",
        )
        .unwrap();
        let mut connector = JsonlConnector::fixture("cursor", source.clone());
        connector.home = home.clone();
        connector.session = session.clone();
        let budget = PollBudget {
            max_records: 100,
            deadline: Duration::from_secs(1),
        };
        let first = connector.poll(budget.clone()).unwrap();
        assert_eq!(first.checkpoint_delta.source_offsets.len(), 2);
        assert_eq!(connector.state().phase, ConversationPhase::Working);
        let checkpoint = connector.checkpoint_snapshot().unwrap();
        let mut restored = JsonlConnector::fixture("cursor", source);
        restored.home = home;
        restored.session = session;
        restored.restore_checkpoint(&checkpoint).unwrap();
        assert!(restored.poll(budget.clone()).unwrap().mutations.is_empty());
        let mut hooks = fs::OpenOptions::new()
            .append(true)
            .open(paths.conversation_source_hooks())
            .unwrap();
        writeln!(hooks, "{{\"hook_event_name\":\"stop\"}}").unwrap();
        let next = restored.poll(budget).unwrap();
        assert_eq!(next.checkpoint_delta.source_offsets.len(), 1);
        assert_eq!(restored.state().phase, ConversationPhase::Idle);
    }

    #[test]
    fn cursor_first_source_binding_keeps_the_active_turn_open() {
        let temp = tempfile::tempdir().unwrap();
        let mut connector = JsonlConnector::fixture("cursor", temp.path().join("transcript.jsonl"));
        connector.home = LatchHome::new(temp.path());
        let paths = connector.home.session(&connector.session);
        paths.ensure().unwrap();
        fs::write(
            paths.conversation_source_binding(),
            serde_json::to_vec(&json!({
                "connector": "cursor", "source": connector.source,
                "agentSessionId": "fixture"
            }))
            .unwrap(),
        )
        .unwrap();
        connector.source = None;
        hook(&mut connector, "beforeSubmitPrompt");
        assert!(!connector.refresh_binding());
        assert_eq!(connector.state().phase, ConversationPhase::Working);
    }
}
