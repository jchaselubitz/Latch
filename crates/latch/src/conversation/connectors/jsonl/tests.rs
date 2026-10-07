use super::bridge::MAX_COMMAND_DESCRIPTION_CHARS;
use super::checkpoint::RuntimeCheckpoint;
use super::claude::turn_closed_at_or_after;
use super::redaction::MAX_TOOL_SUMMARY_CHARS;
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
                questions,
                status,
            } => {
                let mut kind = serde_json::json!({
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
                });
                if !questions.is_empty() { kind["questions"] = serde_json::to_value(questions).unwrap(); }
                kind
            },
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

    let permission: Value =
        serde_json::from_slice(&fs::read(cases.join("permission-request/expected.json")).unwrap())
            .unwrap();
    assert_eq!(
        permission["snapshot"]["state"]["pendingRequest"],
        "ddc8b841-342d-431b-84a0-076fb535b263"
    );
    assert!(permission["snapshot"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |item| item["kind"]["type"] == "request" && item["kind"]["requestType"] == "permission"
        ));

    let question: Value =
        serde_json::from_slice(&fs::read(cases.join("ask-user-question/expected.json")).unwrap())
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

    let truncation: Value =
        serde_json::from_slice(&fs::read(cases.join("branch-truncation/expected.json")).unwrap())
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
        questions: Vec::new(),
        screen_seen: true,
        announced_at: None,
        bridge_call: false,
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
        questions: Vec::new(),
        screen_seen: false,
        announced_at: None,
        bridge_call: false,
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
        questions: Vec::new(),
        screen_seen: false,
        announced_at: Some("2026-09-22T07:03:52Z".to_owned()),
        bridge_call: false,
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

/// The greeting's catalog reaches the state while the bridge is live,
/// bounded as the contract bounds it, and leaves with the bridge.
#[test]
fn the_bridge_greeting_advertises_the_command_catalog_while_it_is_live() {
    use crate::conversation::AdvertisedCommand;
    let dir = tempfile::tempdir().unwrap();
    let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    observe_bridge(&mut connector, "hello", "2026-09-22T08:00:00.000Z");
    assert_eq!(
        connector.state().commands,
        None,
        "a greeting without commands leaves the catalog unknown"
    );

    let mut hello = bridge_record("hello", "2026-09-22T09:00:00.000Z");
    hello["commands"] = serde_json::json!([
        { "name": "compact", "description": "Compacts the conversation.", "source": "builtin" },
        { "name": "", "description": "nameless" },
        { "name": "x".repeat(129), "description": "too long a name" },
        { "name": "review", "description": "d".repeat(900) },
        "not an object",
    ]);
    connector.claude_record(hello.as_object().unwrap(), "hook", 1);
    let commands = connector.state().commands.expect("catalog");
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0],
        AdvertisedCommand {
            name: "compact".into(),
            description: "Compacts the conversation.".into(),
            source: Some("builtin".into()),
        }
    );
    assert_eq!(commands[1].name, "review");
    assert_eq!(
        commands[1].description.chars().count(),
        MAX_COMMAND_DESCRIPTION_CHARS
    );
    assert_eq!(commands[1].source, None);

    let mut end = bridge_record("session.end", "2026-09-22T09:10:00.000Z");
    end["reason"] = Value::from("exit");
    connector.claude_record(end.as_object().unwrap(), "hook", 2);
    assert_eq!(connector.state().commands, None);
}

/// The outcome is the agent's own reason for closing the newest turn. It
/// is carried only between turns, and an unknown reason is no outcome.
#[test]
fn the_turn_outcome_is_the_agents_reason_and_lasts_until_the_next_turn() {
    use crate::conversation::TurnOutcome;
    let dir = tempfile::tempdir().unwrap();
    let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");
    assert_eq!(connector.state().turn_outcome, None);

    let complete = |connector: &mut JsonlConnector, reason: Option<&str>, at: &str| {
        let mut record = bridge_record("turn.complete", at);
        if let Some(reason) = reason {
            record["reason"] = Value::from(reason);
        }
        connector.claude_record(record.as_object().unwrap(), "hook", 1);
    };

    observe_bridge(&mut connector, "turn.start", "2026-09-22T09:00:01.000Z");
    complete(&mut connector, Some("aborted"), "2026-09-22T09:00:05.000Z");
    assert_eq!(connector.state().phase, ConversationPhase::Idle);
    assert_eq!(connector.state().turn_outcome, Some(TurnOutcome::Aborted));

    observe_bridge(&mut connector, "turn.start", "2026-09-22T09:01:00.000Z");
    assert_eq!(
        connector.state().turn_outcome,
        None,
        "an open turn has no outcome yet"
    );
    complete(&mut connector, Some("refusal"), "2026-09-22T09:01:05.000Z");
    assert_eq!(connector.state().turn_outcome, Some(TurnOutcome::Refusal));

    observe_bridge(&mut connector, "turn.start", "2026-09-22T09:02:00.000Z");
    complete(&mut connector, Some("error"), "2026-09-22T09:02:05.000Z");
    assert_eq!(connector.state().turn_outcome, Some(TurnOutcome::Error));

    observe_bridge(&mut connector, "turn.start", "2026-09-22T09:03:00.000Z");
    complete(&mut connector, Some("answer"), "2026-09-22T09:03:05.000Z");
    assert_eq!(connector.state().turn_outcome, Some(TurnOutcome::Answer));

    observe_bridge(&mut connector, "turn.start", "2026-09-22T09:04:00.000Z");
    complete(
        &mut connector,
        Some("something new"),
        "2026-09-22T09:04:05.000Z",
    );
    assert_eq!(
        connector.state().turn_outcome,
        None,
        "an unknown reason is not guessed at"
    );

    // The checkpoint keeps both facts, and one written before they
    // existed still restores.
    complete(&mut connector, Some("aborted"), "2026-09-22T09:05:00.000Z");
    let checkpoint = connector.runtime_checkpoint();
    let mut restored = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    restored.restore_runtime(checkpoint);
    assert_eq!(restored.state().turn_outcome, Some(TurnOutcome::Aborted));
    let legacy: RuntimeCheckpoint = serde_json::from_str(
        r#"{"pending_request":null,"tools":{},"tool_running":false,"last_state":null,"screen_can_send":null}"#,
    )
    .unwrap();
    assert_eq!(legacy.turn_outcome, None);
    assert_eq!(legacy.commands, None);
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
        questions: Vec::new(),
        screen_seen: false,
        announced_at: None,
        bridge_call: false,
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
        questions: Vec::new(),
        screen_seen: true,
        announced_at: None,
        bridge_call: false,
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

fn resolve(request_id: &str, choice: &str) -> ConnectorAction {
    ConnectorAction {
        id: ACTION_RESOLVE_REQUEST.to_owned(),
        payload: serde_json::json!({ "requestId": request_id, "choice": choice }),
    }
}

fn bridge_permission() -> PendingRequest {
    PendingRequest {
        id: "toolu_bash".to_owned(),
        request_type: RequestType::Permission,
        prompt: "Create the marker file".to_owned(),
        choices: vec![
            "Yes".to_owned(),
            "Yes, and don't ask again".to_owned(),
            "No".to_owned(),
        ],
        questions: Vec::new(),
        screen_seen: true,
        announced_at: None,
        bridge_call: true,
    }
}

#[test]
fn a_bridge_permission_is_answered_by_its_call_id_and_rule_labels_stay_on_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let mut connector = bridged_connector(temp.path());
    let inbox = connector
        .home
        .session(&connector.session)
        .conversation_bridge_inbox();

    connector.pending_request = Some(bridge_permission());
    let module = answer_next_bridge_command(&connector, "accepted");
    let result = connector
        .apply(resolve("toolu_bash", "Yes"), Duration::from_secs(5))
        .unwrap();
    assert_eq!(result, ApplyResult::Accepted { correlation: None });
    assert!(connector.pending_request.is_none());
    let command = module.join().unwrap();
    assert_eq!(command["kind"], "answer_permission");
    assert_eq!(command["tool_use_id"], "toolu_bash");
    assert_eq!(command["decision"], "allow");

    connector.pending_request = Some(bridge_permission());
    let module = answer_next_bridge_command(&connector, "refused");
    let result = connector
        .apply(resolve("toolu_bash", "no"), Duration::from_secs(5))
        .unwrap();
    assert!(matches!(result, ApplyResult::Refused { .. }));
    assert_eq!(module.join().unwrap()["decision"], "deny");
    // A refusal leaves the dialog where it was.
    assert!(connector.pending_request.is_some());

    // A label that also writes a rule is the dialog's own: it is never
    // handed to the bridge. The key path then fails here for want of a
    // kernel, which is the proof it was the path taken.
    assert!(connector
        .apply(
            resolve("toolu_bash", "Yes, and don't ask again"),
            Duration::from_secs(2)
        )
        .is_err());
    assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
    assert_eq!(connector.bridge_version, Some(1));

    // A permission the hook announced has no call id the bridge knows.
    connector.pending_request = Some(PendingRequest {
        bridge_call: false,
        id: "permission-1".to_owned(),
        ..bridge_permission()
    });
    assert!(connector
        .apply(resolve("permission-1", "Yes"), Duration::from_secs(2))
        .is_err());
    assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
    assert_eq!(connector.bridge_version, Some(1));
}

#[test]
fn an_untaken_permission_answer_is_withdrawn_before_the_key_path_is_tried() {
    let temp = tempfile::tempdir().unwrap();
    let mut connector = bridged_connector(temp.path());
    let inbox = connector
        .home
        .session(&connector.session)
        .conversation_bridge_inbox();
    connector.pending_request = Some(bridge_permission());
    // Nobody plays the module. The key path then fails for want of a
    // kernel, and the request is still there to answer at the terminal.
    assert!(connector
        .apply(resolve("toolu_bash", "Yes"), Duration::from_secs(5))
        .is_err());
    assert_eq!(connector.bridge_version, None);
    assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
    assert!(connector.pending_request.is_some());
}

/// The engine's permission hook carries no call id. The bridge announces
/// the dialog under the call's id; the hook for the same dialog must not
/// replace it, the transcript must not dismiss it, and the bridge closes it.
#[test]
fn the_bridge_announces_an_open_permission_under_its_call_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");

    let mut open = bridge_record("permission.open", "2026-09-22T09:00:01.000Z");
    open["tool_use_id"] = Value::from("toolu_bash");
    open["tool"] = Value::from("Bash");
    open["input"] = serde_json::json!({
        "command": "touch marker",
        "description": "Create the marker file",
    });
    let mutations = connector.claude_record(open.as_object().unwrap(), "hook", 2);
    assert!(matches!(
        mutations.as_slice(),
        [ConnectorMutation::Upsert(ObservedItem {
            kind: ConversationItemKind::Request {
                request_id,
                request_type: RequestType::Permission,
                prompt,
                choices,
                status: RequestStatus::Pending,
                ..
            },
            ..
        })] if request_id == "toolu_bash" && prompt == "Create the marker file" && choices == &["Yes", "No"]
    ));
    assert_eq!(connector.state().phase, ConversationPhase::AwaitingInput);
    assert!(connector.state().resolve_request.enabled);

    let hook = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "latch_observer_version": 2,
        "tool_name": "Bash",
        "tool_input": { "command": "touch marker", "description": "Create the marker file" },
        "prompt_id": "prompt-1",
        "timestamp": "2026-09-22T09:00:01.100Z",
    });
    assert!(connector
        .claude_record(hook.as_object().unwrap(), "hook", 3)
        .is_empty());
    assert_eq!(connector.pending_request.as_ref().unwrap().id, "toolu_bash");

    let later = serde_json::json!({
        "type": "user",
        "uuid": "later-user-message",
        "timestamp": "2026-09-22T09:00:02.000Z",
        "message": { "content": "Carry on." }
    });
    let mutations = connector.claude_record(later.as_object().unwrap(), "user", 4);
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
    assert_eq!(connector.pending_request.as_ref().unwrap().id, "toolu_bash");

    let mut closed = bridge_record("permission.closed", "2026-09-22T09:00:09.000Z");
    closed["tool_use_id"] = Value::from("toolu_bash");
    let mutations = connector.claude_record(closed.as_object().unwrap(), "hook", 5);
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

#[test]
fn a_permission_the_hook_announced_first_is_replaced_by_its_call_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    observe_bridge(&mut connector, "hello", "2026-09-22T09:00:00.000Z");

    let hook = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "latch_observer_version": 2,
        "tool_name": "Edit",
        "tool_input": { "file_path": "src/main.rs" },
        "prompt_id": "prompt-1",
        "timestamp": "2026-09-22T09:00:01.000Z",
    });
    connector.claude_record(hook.as_object().unwrap(), "hook", 2);
    assert_eq!(connector.pending_request.as_ref().unwrap().id, "prompt-1");

    let mut open = bridge_record("permission.open", "2026-09-22T09:00:01.050Z");
    open["tool_use_id"] = Value::from("toolu_edit");
    open["tool"] = Value::from("Edit");
    open["input"] = serde_json::json!({ "file_path": "src/main.rs" });
    let mutations = connector.claude_record(open.as_object().unwrap(), "hook", 3);
    assert!(matches!(
        mutations.as_slice(),
        [
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    request_id: dismissed,
                    status: RequestStatus::Dismissed,
                    ..
                },
                ..
            }),
            ConnectorMutation::Upsert(ObservedItem {
                kind: ConversationItemKind::Request {
                    request_id: pending,
                    prompt,
                    status: RequestStatus::Pending,
                    ..
                },
                ..
            })
        ] if dismissed == "prompt-1" && pending == "toolu_edit" && prompt == "Allow Edit?"
    ));
    assert!(connector.pending_request.as_ref().unwrap().bridge_call);
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
                questions,
                status: RequestStatus::Pending,
            },
            ..
        })] if request_id == "toolu_q" && prompt == "Which color?" && choices == &["Red", "Blue"] && questions.len() == 1 && !questions[0].multi_select
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
fn open_structured_questions(connector: &mut JsonlConnector) {
    let mut open = bridge_record("question.open", "2026-09-22T09:00:01.000Z");
    open["tool_use_id"] = Value::from("toolu_multi");
    open["questions"] = serde_json::json!([
        {"question": "Which colors?\nChoose freely.", "header": "Colors", "multi_select": true,
         "options": [{"label": "Red", "description": "Warm"}, {"label": "Blue", "description": "Cool"}]},
        {"question": "Why?", "multi_select": false, "options": []}
    ]);
    connector.claude_record(open.as_object().unwrap(), "hook", 2);
}

#[test]
fn structured_answers_preserve_question_keys_and_accept_free_text() {
    let temp = tempfile::tempdir().unwrap();
    let mut connector = bridged_connector(temp.path());
    open_structured_questions(&mut connector);
    let request = connector.pending_request.as_ref().unwrap();
    assert_eq!(request.questions.len(), 2);
    assert!(request.questions[0].multi_select);
    assert_eq!(request.questions[0].options[1].description, "Cool");
    assert_eq!(request.questions[0].header.as_deref(), Some("Colors"));
    let module = answer_next_bridge_command(&connector, "accepted");
    let answers = serde_json::json!({"Which colors?\nChoose freely.": "Red, Blue", "Why?": "A custom explanation"});
    let result = connector
        .apply(
            ConnectorAction {
                id: ACTION_RESOLVE_REQUEST.into(),
                payload: serde_json::json!({"requestId": "toolu_multi", "answers": answers}),
            },
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(result, ApplyResult::Accepted { correlation: None });
    let command = module.join().unwrap();
    assert_eq!(command["tool_use_id"], "toolu_multi");
    assert_eq!(command["answers"], answers);
    assert!(connector.pending_request.is_none());
}

#[test]
fn structured_answers_refuse_incomplete_extra_stale_and_legacy_flattened_answers() {
    let temp = tempfile::tempdir().unwrap();
    let mut connector = bridged_connector(temp.path());
    open_structured_questions(&mut connector);
    for payload in [
        serde_json::json!({"requestId": "toolu_multi", "answers": {"Why?": "text"}}),
        serde_json::json!({"requestId": "toolu_multi", "answers": {"Which colors?\nChoose freely.": "Red", "Why?": "text", "Extra": "text"}}),
        serde_json::json!({"requestId": "old", "answers": {"Which colors?\nChoose freely.": "Red", "Why?": "text"}}),
        serde_json::json!({"requestId": "toolu_multi", "answers": {"Which colors?\nChoose freely.": "Red", "Why?": "  "}}),
        serde_json::json!({"requestId": "toolu_multi", "choice": "Red"}),
    ] {
        assert!(matches!(
            connector
                .apply(
                    ConnectorAction {
                        id: ACTION_RESOLVE_REQUEST.into(),
                        payload
                    },
                    Duration::from_millis(20)
                )
                .unwrap(),
            ApplyResult::Refused { .. }
        ));
        assert!(connector.pending_request.is_some());
    }
    assert!(!connector
        .home
        .session(&connector.session)
        .conversation_bridge_inbox()
        .exists());
}

#[test]
fn structured_answers_never_fall_back_to_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let mut connector = bridged_connector(temp.path());
    open_structured_questions(&mut connector);
    let result = connector.apply(ConnectorAction {
        id: ACTION_RESOLVE_REQUEST.into(),
        payload: serde_json::json!({"requestId": "toolu_multi", "answers": {"Which colors?\nChoose freely.": "Red, Blue", "Why?": "text"}}),
    }, Duration::from_millis(30)).unwrap();
    assert!(matches!(result, ApplyResult::Refused { .. }));
    assert!(connector.bridge_version.is_none());
    assert!(connector.pending_request.is_some());
    assert!(!connector.state().resolve_request.enabled);
}

fn permission_open(id: &str, marker: &str, at: &str) -> Value {
    let mut open = bridge_record("permission.open", at);
    open["tool_use_id"] = Value::from(id);
    open["tool"] = Value::from("Bash");
    open["input"] = serde_json::json!({
        "command": format!("touch {marker}"),
        "description": format!("Create marker {marker}"),
    });
    open
}

fn permission_closed(id: &str, answered_by: &str, at: &str) -> Value {
    let mut closed = bridge_record("permission.closed", at);
    closed["tool_use_id"] = Value::from(id);
    closed["answered_by"] = Value::from(answered_by);
    closed
}

/// Claude's permission dialog for one call of the batch, as painted, with
/// whatever the screen still holds above it.
fn permission_dialog(above: &str, marker: &str) -> String {
    format!(
        "{above}\nBash command\n  touch {marker}\n  Create marker {marker}\nDo you want to proceed?\n❯ 1. Yes\n  2. Yes, and don't ask again for touch commands in this project\n  3. No, and tell Claude what to do differently (esc)"
    )
}

/// Plays the Hub: the observation connector's mutations go into the
/// projection clients read, and every action runs on a separate action
/// connector restored from the latest checkpoint first.
struct Batch {
    observer: JsonlConnector,
    actor: JsonlConnector,
    projection: super::super::super::Projection,
    ordinal: u64,
}

impl Batch {
    fn new(home: &std::path::Path) -> Self {
        let observer = bridged_connector(home);
        let mut actor = JsonlConnector::fixture("claude", observer.source.clone().unwrap());
        actor.home = observer.home.clone();
        actor.session = observer.session.clone();
        let projection = super::super::super::Projection::new(
            super::super::super::OperationEpoch::new("fixture"),
            ConversationState::starting(Some(observer.identity())),
        );
        Self {
            observer,
            actor,
            projection,
            ordinal: 0,
        }
    }

    fn publish(&mut self, mutations: Vec<ConnectorMutation>) -> Vec<ConnectorMutation> {
        for mutation in mutations.iter().cloned() {
            self.projection.apply_connector(mutation).unwrap();
        }
        mutations
    }

    fn hook(&mut self, record: Value) -> Vec<ConnectorMutation> {
        self.ordinal += 1;
        let mutations =
            self.observer
                .claude_record(record.as_object().unwrap(), "hook", self.ordinal);
        self.publish(mutations)
    }

    fn screen(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        let mutations = self.observer.observe_screen(screen);
        self.publish(mutations)
    }

    fn act(&mut self, action: ConnectorAction) -> ApplyResult {
        let checkpoint = self.observer.checkpoint_snapshot().unwrap();
        self.actor.restore_checkpoint(&checkpoint).unwrap();
        self.actor.apply(action, Duration::from_secs(5)).unwrap()
    }

    fn key_for(&mut self, screen: &str, choice: &str) -> std::result::Result<String, String> {
        let checkpoint = self.observer.checkpoint_snapshot().unwrap();
        self.actor.restore_checkpoint(&checkpoint).unwrap();
        self.actor.terminal_choice_key(screen, choice)
    }

    /// The request clients are shown, which must be the one the
    /// connector would answer.
    fn shown(&self) -> Option<String> {
        let shown = self.projection.state().pending_request;
        assert_eq!(
            shown,
            self.observer.pending_request.as_ref().map(|r| r.id.clone()),
            "the Hub presents the request the connector answers"
        );
        shown
    }
}

fn surfaced(mutations: &[ConnectorMutation]) -> Vec<(String, RequestStatus)> {
    mutations
        .iter()
        .filter_map(|mutation| match mutation {
            ConnectorMutation::Upsert(ObservedItem {
                kind:
                    ConversationItemKind::Request {
                        request_id, status, ..
                    },
                ..
            }) => Some((request_id.clone(), status.clone())),
            _ => None,
        })
        .collect()
}

/// A parallel batch raises every dialog at once. The Hub shows one at a
/// time, oldest first or whichever Claude paints, surfaces the next when
/// one closes, and answers each by its own call id from Latch or leaves
/// it to the terminal, in any order.
#[test]
fn a_batch_of_three_permission_dialogs_is_answered_in_mixed_order() {
    let temp = tempfile::tempdir().unwrap();
    let mut batch = Batch::new(temp.path());

    // The bridge's records arrive out of order: each is its own run of
    // `latch`. The classic hook's own records for the same dialogs
    // arrive among them and are ignored.
    assert_eq!(
        surfaced(&batch.hook(permission_open("toolu_a", "a", "2026-09-22T09:00:01.001Z"))),
        [("toolu_a".to_owned(), RequestStatus::Pending)]
    );
    for (record, marker) in [
        (
            permission_open("toolu_c", "c", "2026-09-22T09:00:01.003Z"),
            None,
        ),
        (serde_json::Value::Null, Some("b")),
        (
            permission_open("toolu_b", "b", "2026-09-22T09:00:01.002Z"),
            None,
        ),
        (serde_json::Value::Null, Some("c")),
    ] {
        let record = match marker {
            Some(marker) => serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "latch_observer_version": 2,
                "tool_name": "Bash",
                "tool_input": {
                    "command": format!("touch {marker}"),
                    "description": format!("Create marker {marker}"),
                },
                "prompt_id": format!("prompt-{marker}"),
                "timestamp": "2026-09-22T09:00:01Z",
            }),
            None => record,
        };
        assert!(
            batch.hook(record).is_empty(),
            "only the shown dialog reaches clients"
        );
    }
    assert_eq!(batch.shown().as_deref(), Some("toolu_a"));
    let queued: Vec<_> = batch
        .observer
        .queued_requests
        .iter()
        .map(|request| request.id.as_str())
        .collect();
    assert_eq!(
        queued,
        ["toolu_b", "toolu_c"],
        "waiting in the order raised"
    );
    assert_eq!(
        batch.observer.state().phase,
        ConversationPhase::AwaitingInput
    );

    // Claude paints the oldest. Its labels replace the seeded ones, and a
    // rule-writing label takes the screen-verified key path.
    let screen_a = permission_dialog("Earlier output", "a");
    batch.screen(&screen_a);
    assert_eq!(batch.shown().as_deref(), Some("toolu_a"));
    assert_eq!(
        batch.key_for(
            &screen_a,
            "Yes, and don't ask again for touch commands in this project"
        ),
        Ok("2".to_owned())
    );

    // 1. The terminal answers A. The bridge reports it and B surfaces.
    let mutations = batch.hook(permission_closed(
        "toolu_a",
        "terminal",
        "2026-09-22T09:00:04.000Z",
    ));
    assert_eq!(
        surfaced(&mutations),
        [
            ("toolu_a".to_owned(), RequestStatus::Dismissed),
            ("toolu_b".to_owned(), RequestStatus::Pending),
        ]
    );
    assert_eq!(batch.shown().as_deref(), Some("toolu_b"));

    // Claude paints C next, B's text still in the scrollback above it.
    // The key path for B refuses: a key would land in C's dialog.
    let screen_c = permission_dialog(
        "Bash command\n  touch b\n  Create marker b\nEarlier output",
        "c",
    );
    assert!(batch.key_for(&screen_c, "Yes").is_err());
    // The screen then shows clients the dialog that is really open.
    assert_eq!(
        surfaced(&batch.screen(&screen_c)),
        [("toolu_c".to_owned(), RequestStatus::Pending)]
    );
    assert_eq!(batch.shown().as_deref(), Some("toolu_c"));
    assert_eq!(batch.key_for(&screen_c, "Yes"), Ok("1".to_owned()));

    // 2. Latch denies C by its call id; the module closes it.
    let module = answer_next_bridge_command(&batch.actor, "accepted");
    assert_eq!(
        batch.act(resolve("toolu_c", "No")),
        ApplyResult::Accepted { correlation: None }
    );
    let command = module.join().unwrap();
    assert_eq!(command["kind"], "answer_permission");
    assert_eq!(command["tool_use_id"], "toolu_c");
    assert_eq!(command["decision"], "deny");
    let mutations = batch.hook(permission_closed(
        "toolu_c",
        "latch",
        "2026-09-22T09:00:06.000Z",
    ));
    assert_eq!(
        surfaced(&mutations),
        [("toolu_c".to_owned(), RequestStatus::Dismissed)],
        "B is pending at the Hub already"
    );
    assert_eq!(batch.shown().as_deref(), Some("toolu_b"));

    // A stale answer aimed at C is refused without reaching the agent.
    assert!(matches!(
        batch.act(resolve("toolu_c", "Yes")),
        ApplyResult::Refused { .. }
    ));

    // 3. Latch allows B by its call id; the module closes it and nothing
    // is left waiting.
    let screen_b = permission_dialog("Earlier output", "b");
    batch.screen(&screen_b);
    let module = answer_next_bridge_command(&batch.actor, "accepted");
    assert_eq!(
        batch.act(resolve("toolu_b", "Yes")),
        ApplyResult::Accepted { correlation: None }
    );
    assert_eq!(module.join().unwrap()["decision"], "allow");
    let mutations = batch.hook(permission_closed(
        "toolu_b",
        "latch",
        "2026-09-22T09:00:08.000Z",
    ));
    assert_eq!(
        surfaced(&mutations),
        [("toolu_b".to_owned(), RequestStatus::Dismissed)]
    );
    assert_eq!(batch.shown(), None);
    assert!(!batch.observer.has_waiting_requests());
    assert_ne!(
        batch.observer.state().phase,
        ConversationPhase::AwaitingInput
    );
}

/// The other mix: Latch answers the shown dialog first, the terminal then
/// answers one that was never shown, and a rule label from Latch for the
/// last takes the key path on the dialog Claude paints.
#[test]
fn a_waiting_dialog_answered_at_the_terminal_never_reaches_clients() {
    let temp = tempfile::tempdir().unwrap();
    let mut batch = Batch::new(temp.path());
    for (id, marker, at) in [
        ("toolu_a", "a", "2026-09-22T09:00:01.001Z"),
        ("toolu_b", "b", "2026-09-22T09:00:01.002Z"),
        ("toolu_c", "c", "2026-09-22T09:00:01.003Z"),
    ] {
        batch.hook(permission_open(id, marker, at));
    }

    // 1. Latch allows A by its call id.
    let module = answer_next_bridge_command(&batch.actor, "accepted");
    assert_eq!(
        batch.act(resolve("toolu_a", "Yes")),
        ApplyResult::Accepted { correlation: None }
    );
    assert_eq!(module.join().unwrap()["tool_use_id"], "toolu_a");
    // The action connector already looks past A; clients follow the
    // bridge's own close.
    assert_eq!(
        batch.actor.pending_request.as_ref().map(|r| r.id.as_str()),
        Some("toolu_b")
    );
    let mutations = batch.hook(permission_closed(
        "toolu_a",
        "latch",
        "2026-09-22T09:00:03.000Z",
    ));
    assert_eq!(
        surfaced(&mutations),
        [
            ("toolu_a".to_owned(), RequestStatus::Dismissed),
            ("toolu_b".to_owned(), RequestStatus::Pending),
        ]
    );

    // 2. The terminal answers C while B is shown: C was never shown, so
    // clients have nothing to dismiss, and B stays.
    assert!(batch
        .hook(permission_closed(
            "toolu_c",
            "terminal",
            "2026-09-22T09:00:04.000Z"
        ))
        .is_empty());
    assert_eq!(batch.shown().as_deref(), Some("toolu_b"));
    assert!(!batch.observer.has_waiting_requests());

    // 3. With B the only dialog, a rule label takes the key path.
    let screen_b = permission_dialog("Earlier output", "b");
    batch.screen(&screen_b);
    assert_eq!(
        batch.key_for(
            &screen_b,
            "Yes, and don't ask again for touch commands in this project"
        ),
        Ok("2".to_owned())
    );
    let mutations = batch.hook(permission_closed(
        "toolu_b",
        "terminal",
        "2026-09-22T09:00:06.000Z",
    ));
    assert_eq!(
        surfaced(&mutations),
        [("toolu_b".to_owned(), RequestStatus::Dismissed)]
    );
    assert_eq!(batch.shown(), None);
}

/// Dialogs whose prompts the screen cannot tell apart: the oldest stays
/// shown and is still answered by call id, but no key is pressed.
#[test]
fn dialogs_the_screen_cannot_tell_apart_are_answered_only_by_call_id() {
    let temp = tempfile::tempdir().unwrap();
    let mut batch = Batch::new(temp.path());
    for (id, at) in [
        ("toolu_a", "2026-09-22T09:00:01.001Z"),
        ("toolu_b", "2026-09-22T09:00:01.002Z"),
    ] {
        batch.hook(permission_open(id, "same", at));
    }
    let screen = permission_dialog("Earlier output", "same");
    assert!(batch.screen(&screen).is_empty());
    assert_eq!(batch.shown().as_deref(), Some("toolu_a"));
    assert!(batch.key_for(&screen, "Yes").is_err());

    let module = answer_next_bridge_command(&batch.actor, "accepted");
    assert_eq!(
        batch.act(resolve("toolu_a", "Yes")),
        ApplyResult::Accepted { correlation: None }
    );
    assert_eq!(module.join().unwrap()["tool_use_id"], "toolu_a");
}

/// The queue survives a Hub checkpoint, and leaves with the bridge: only
/// the bridge says when a waiting dialog closes.
#[test]
fn waiting_dialogs_survive_a_checkpoint_and_leave_with_the_bridge() {
    let temp = tempfile::tempdir().unwrap();
    let mut batch = Batch::new(temp.path());
    for (id, marker, at) in [
        ("toolu_a", "a", "2026-09-22T09:00:01.001Z"),
        ("toolu_b", "b", "2026-09-22T09:00:01.002Z"),
        ("toolu_c", "c", "2026-09-22T09:00:01.003Z"),
    ] {
        batch.hook(permission_open(id, marker, at));
    }
    // C is painted, so A waits displaced and still pending at the Hub.
    batch.screen(&permission_dialog("Earlier output", "c"));
    assert_eq!(batch.shown().as_deref(), Some("toolu_c"));

    let checkpoint = batch.observer.checkpoint_snapshot().unwrap();
    let mut restored = JsonlConnector::fixture("claude", batch.observer.source.clone().unwrap());
    restored.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(restored.pending_request, batch.observer.pending_request);
    assert_eq!(restored.queued_requests, batch.observer.queued_requests);
    assert_eq!(
        restored.displaced_requests,
        batch.observer.displaced_requests
    );
    // A checkpoint from before the queue existed still restores.
    let old: RuntimeCheckpoint = serde_json::from_str(
        r#"{"pending_request":null,"tools":{},"tool_running":false,"last_state":null,"screen_can_send":null}"#,
    )
    .unwrap();
    assert!(old.queued_requests.is_empty() && old.displaced_requests.is_empty());

    let mut end = bridge_record("session.end", "2026-09-22T09:00:09.000Z");
    end["reason"] = Value::from("prompt_input_exit");
    assert_eq!(
        surfaced(&batch.hook(end)),
        [("toolu_a".to_owned(), RequestStatus::Dismissed)]
    );
    assert!(!batch.observer.has_waiting_requests());
    assert_eq!(batch.shown().as_deref(), Some("toolu_c"));
}

/// Without a bridge the hook names no call, so a second dialog replaces
/// the first rather than leaving it pending at the Hub for good.
#[test]
fn a_hook_permission_replaced_by_another_is_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let mut connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    let hook = |prompt: &str| {
        serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "latch_observer_version": 2,
            "tool_name": "Bash",
            "tool_input": { "command": "true", "description": prompt },
            "prompt_id": prompt,
            "timestamp": "2026-09-22T09:00:01Z",
        })
    };
    connector.claude_record(hook("first").as_object().unwrap(), "hook", 1);
    let mutations = connector.claude_record(hook("second").as_object().unwrap(), "hook", 2);
    assert_eq!(
        surfaced(&mutations),
        [
            ("first".to_owned(), RequestStatus::Dismissed),
            ("second".to_owned(), RequestStatus::Pending),
        ]
    );
}
