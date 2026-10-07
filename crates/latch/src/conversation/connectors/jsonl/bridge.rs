//! The session's bridge module: what the engine itself reports (requests under
//! their call ids, turn boundaries, the command catalog), and the commands
//! Latch hands it in place of typing into the terminal.
use super::*;

impl JsonlConnector {
    /// One record from the session's bridge module: what the engine itself
    /// reported, as opposed to what the transcript or screen imply.
    pub(super) fn claude_bridge_record(
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
                return self.announce_bridge_request(PendingRequest {
                    id,
                    request_type: RequestType::Question,
                    prompt: claude_question_prompt(Some(&input)),
                    choices: claude_question_choices(Some(&input)),
                    questions: bridge_questions(&input),
                    screen_seen: false,
                    announced_at: string(object, "timestamp"),
                    bridge_call: true,
                });
            }
            // The engine's permission hook carries no call id. The bridge
            // matches the dialog to the running call and announces it under
            // that call's id, which is what the Hub answers by.
            Some("permission.open") => {
                let Some(id) = string(object, "tool_use_id") else {
                    return Vec::new();
                };
                let tool = string(object, "tool").unwrap_or_else(|| "this tool".to_owned());
                return self.announce_bridge_request(PendingRequest {
                    id,
                    request_type: RequestType::Permission,
                    // The same words the permission hook would give it, so
                    // the dialog on the screen is found by either.
                    prompt: object
                        .get("input")
                        .and_then(|input| input.get("description"))
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Allow {tool}?")),
                    // What the bridge can carry out. The labels Claude paints
                    // replace these once the dialog is on the screen.
                    choices: vec!["Yes".to_owned(), "No".to_owned()],
                    questions: Vec::new(),
                    screen_seen: false,
                    announced_at: string(object, "timestamp"),
                    bridge_call: true,
                });
            }
            // Answered at the terminal, from Latch, or by nobody: the next
            // dialog of a parallel batch is shown once the shown one closes.
            Some("question.closed") | Some("permission.closed") => {
                if let Some(closed) = string(object, "tool_use_id") {
                    return self.close_request(&closed);
                }
            }
            Some("hello") => {
                self.bridge_version = object
                    .get("bridge_version")
                    .and_then(Value::as_u64)
                    .and_then(|version| u32::try_from(version).ok());
                // A greeting that names no commands leaves the catalog
                // unknown rather than empty.
                self.commands = object
                    .get("commands")
                    .and_then(Value::as_array)
                    .map(|list| bridge_commands(list));
            }
            Some("turn.start") => {
                self.turn_open = true;
                self.turn_outcome = None;
            }
            Some("turn.complete") => {
                // Answered, interrupted, refused, or failed: the main loop
                // has stopped, so nothing of this turn is still running.
                self.turn_open = false;
                self.tool_running = false;
                self.last_turn_close = string(object, "timestamp").or(self.last_turn_close.take());
                self.turn_outcome = turn_outcome(string(object, "reason").as_deref());
            }
            // `/clear` ends the conversation and keeps the process, and with
            // it the loaded module. Every other end takes the module along.
            Some("session.end") if string(object, "reason").as_deref() != Some("clear") => {
                self.bridge_version = None;
                self.turn_open = false;
                self.commands = None;
                return self.drop_waiting_requests();
            }
            _ => {}
        }
        Vec::new()
    }

    /// Hands one command to the bridge module and waits for its word on it.
    pub(super) fn ask_bridge(
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

    /// Asks the bridge to abort the running turn.
    pub(super) fn cancel_turn(&mut self, deadline: Duration) -> Result<ApplyResult> {
        if !self.state().cancel_turn.enabled {
            return Ok(ApplyResult::Refused {
                reason: "no running turn with a live bridge".into(),
            });
        }
        Ok(
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
        )
    }

    /// Answers every question of the shown structured request through the
    /// bridge, by its call id. Never falls back to the terminal.
    pub(super) fn answer_structured_questions(
        &mut self,
        payload: &Value,
        deadline: Duration,
    ) -> Result<ApplyResult> {
        let request_id = payload.get("requestId").and_then(Value::as_str);
        let Some(request) = self.pending_request.as_ref().filter(|r| {
            Some(r.id.as_str()) == request_id
                && r.request_type == RequestType::Question
                && !r.questions.is_empty()
                && self.bridge_version.is_some()
        }) else {
            return Ok(ApplyResult::Refused {
                reason: "structured answers require the current question and a live bridge".into(),
            });
        };
        let Some(answers) = payload.get("answers").and_then(Value::as_object) else {
            return Ok(ApplyResult::Refused {
                reason: "answers must be a question-to-text map".into(),
            });
        };
        let keys: std::collections::HashSet<_> = request
            .questions
            .iter()
            .map(|q| q.question.as_str())
            .collect();
        if keys.len() != request.questions.len()
            || answers.len() != keys.len()
            || !keys.iter().all(|key| {
                answers
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.trim().is_empty() && v.len() <= 4096)
            })
        {
            return Ok(ApplyResult::Refused {
                reason: "provide exactly one nonempty answer per question".into(),
            });
        }
        let mut payload = serde_json::Map::new();
        payload.insert("tool_use_id".into(), Value::from(request.id.clone()));
        payload.insert("answers".into(), Value::Object(answers.clone()));
        Ok(
            match self.ask_bridge("answer_question", payload, deadline)? {
                BridgeAnswer::Accepted => {
                    if let Some(answered) = request_id {
                        self.close_request(answered);
                    }
                    self.last_screen_refresh = None;
                    ApplyResult::Accepted { correlation: None }
                }
                BridgeAnswer::Refused(reason) => ApplyResult::Refused { reason },
                BridgeAnswer::NotTaken => {
                    self.bridge_version = None;
                    ApplyResult::Refused {
                        reason: "the bridge is no longer live; answer at the terminal".into(),
                    }
                }
                BridgeAnswer::Queued => ApplyResult::Refused {
                    reason: "the bridge did not answer the question".into(),
                },
            },
        )
    }
}

/// How long the bridge module has to take a queued command. It looks twice a
/// second, so a command still waiting after this long has no one to take it.
const BRIDGE_CLAIM_TIMEOUT: Duration = Duration::from_millis(2_500);

pub(super) enum BridgeAnswer {
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
    read_bounded(File::open(sidecar).ok()?, from)
        .ok()?
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

/// The most commands a greeting may advertise, and how long each part may
/// be, as the contract bounds them.
const MAX_ADVERTISED_COMMANDS: usize = 200;

const MAX_COMMAND_NAME_CHARS: usize = 128;

pub(super) const MAX_COMMAND_DESCRIPTION_CHARS: usize = 512;

const MAX_COMMAND_SOURCE_CHARS: usize = 64;

/// The command catalog a bridge greeting carries. An entry without a usable
/// name is left out; the rest are bounded, not refused.
fn bridge_commands(list: &[Value]) -> Vec<crate::conversation::AdvertisedCommand> {
    list.iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let name = string(object, "name")?;
            if name.is_empty() || name.chars().count() > MAX_COMMAND_NAME_CHARS {
                return None;
            }
            Some(crate::conversation::AdvertisedCommand {
                name,
                description: bounded_chars(
                    string(object, "description").unwrap_or_default(),
                    MAX_COMMAND_DESCRIPTION_CHARS,
                ),
                source: string(object, "source")
                    .map(|source| bounded_chars(source, MAX_COMMAND_SOURCE_CHARS)),
            })
        })
        .take(MAX_ADVERTISED_COMMANDS)
        .collect()
}

/// The bridge decision a permission label stands for, or none when the label
/// is one only the dialog itself can honour.
pub(super) fn bridge_permission_decision(label: &str) -> Option<&'static str> {
    let label = label.trim();
    if label.eq_ignore_ascii_case("yes") || label.eq_ignore_ascii_case("allow") {
        Some("allow")
    } else if label.eq_ignore_ascii_case("no") || label.eq_ignore_ascii_case("deny") {
        Some("deny")
    } else {
        None
    }
}

fn bounded_chars(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text
    } else {
        text.chars().take(max_chars).collect()
    }
}

/// The bridge's `turn.complete` reason as the contract names it. A reason
/// this Latch does not know is no outcome rather than a guess.
fn turn_outcome(reason: Option<&str>) -> Option<crate::conversation::TurnOutcome> {
    use crate::conversation::TurnOutcome;
    match reason {
        Some("answer") => Some(TurnOutcome::Answer),
        Some("aborted") => Some(TurnOutcome::Aborted),
        Some("refusal") => Some(TurnOutcome::Refusal),
        Some("error") => Some(TurnOutcome::Error),
        _ => None,
    }
}
