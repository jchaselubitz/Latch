//! The `Connector` trait surface the Hub drives. Each method validates and
//! delegates; the mechanics live in the sibling modules.
use super::*;

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
            self.read_hook_sidecar(budget.max_records, &mut mutations, &mut delta)?;
        }
        if let Some(source) = self.source.clone() {
            self.read_transcript(source, budget.max_records, &mut mutations, &mut delta)?;
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
                id: crate::conversation::ACTION_CANCEL_TURN.to_owned(),
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
        if action.id == crate::conversation::ACTION_CANCEL_TURN {
            return self.cancel_turn(deadline);
        }
        if action.id == ACTION_RESOLVE_REQUEST && action.payload.get("answers").is_some() {
            return self.answer_structured_questions(&action.payload, deadline);
        }
        if action.id == ACTION_RESOLVE_REQUEST
            && self
                .pending_request
                .as_ref()
                .is_some_and(|r| r.questions.len() > 1)
        {
            return Ok(ApplyResult::Refused {
                reason: "this request requires one answer per question".into(),
            });
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
                match self.pending_request.as_ref() {
                    // The bridge answers a question by its call id, so the
                    // answer cannot land on another prompt and need not be
                    // one of the offered labels: free text and comma-joined
                    // multi-select answers are the tool's own. Multi-question
                    // requests require the structured answer map handled above.
                    Some(request)
                        if request.request_type == RequestType::Question
                            && !request.prompt.is_empty()
                            && (request.questions.len() == 1 || !request.prompt.contains('\n')) =>
                    {
                        payload.insert("tool_use_id".to_owned(), Value::from(request.id.clone()));
                        payload.insert(
                            "answers".to_owned(),
                            serde_json::json!({ request.prompt.clone(): text }),
                        );
                        Some("answer_question")
                    }
                    // A permission the bridge announced is answered by its
                    // call id with the two decisions the bridge can carry
                    // out. A label that also writes a rule ("Yes, and don't
                    // ask again") is the dialog's own and stays on the key path.
                    Some(request)
                        if request.request_type == RequestType::Permission
                            && request.bridge_call =>
                    {
                        bridge_permission_decision(text).map(|decision| {
                            payload
                                .insert("tool_use_id".to_owned(), Value::from(request.id.clone()));
                            payload.insert("decision".to_owned(), Value::from(decision));
                            "answer_permission"
                        })
                    }
                    _ => None,
                }
            };
            if let Some(kind) = command {
                match self.ask_bridge(kind, payload, remaining()?)? {
                    BridgeAnswer::Queued => return Ok(ApplyResult::Queued { correlation: None }),
                    BridgeAnswer::Accepted => {
                        if let Some(answered) = (action.id == ACTION_RESOLVE_REQUEST)
                            .then(|| self.pending_request.as_ref().map(|r| r.id.clone()))
                            .flatten()
                        {
                            self.close_request(&answered);
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
                self.turn_outcome = None;
            } else {
                self.control()?.submit(text, remaining()?)?;
            }
        } else {
            let key = match self.terminal_choice_key(&screen, text) {
                Ok(key) => key,
                Err(reason) => return Ok(ApplyResult::Refused { reason }),
            };
            self.control()?.key(&[key], remaining()?)?;
            let answered = self
                .pending_request
                .as_ref()
                .expect("checked above")
                .id
                .clone();
            self.close_request(&answered);
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
        self.restore_saved_checkpoint(checkpoint)
    }

    fn apply_checkpoint_delta(&mut self, delta: &CheckpointDelta) -> Result<()> {
        self.merge_checkpoint_delta(delta)
    }

    fn checkpoint_snapshot(&self) -> Result<Vec<u8>> {
        self.saved_checkpoint()
    }
}
