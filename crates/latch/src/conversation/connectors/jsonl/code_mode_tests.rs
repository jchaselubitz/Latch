//! Replays of the live Code Mode captures in
//! `fixtures/conversation/claude/code-mode` (coo:1334.w5d6): each REPL script
//! is one Code item, and its in-script calls add no items of their own.
use super::*;
use crate::conversation::ConversationActivity;

fn fixture_lines(case: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/conversation/claude/code-mode")
        .join(case)
        .join("transcript.jsonl");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn replay(case: &str) -> (JsonlConnector, Vec<ConnectorMutation>) {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    let mut mutations = Vec::new();
    for (index, record) in fixture_lines(case).into_iter().enumerate() {
        let event = record["type"].as_str().unwrap_or_default().to_owned();
        mutations.extend(connector.claude_record(
            record.as_object().unwrap(),
            &event,
            index as u64 + 1,
        ));
    }
    (connector, mutations)
}

/// Each item as its last upsert left it, in first-seen order.
fn settled(mutations: &[ConnectorMutation]) -> Vec<ObservedItem> {
    let mut items: Vec<ObservedItem> = Vec::new();
    for mutation in mutations {
        if let ConnectorMutation::Upsert(item) = mutation {
            match items.iter_mut().find(|known| known.id == item.id) {
                Some(known) => *known = item.clone(),
                None => items.push(item.clone()),
            }
        }
    }
    items
}

fn tools(mutations: &[ConnectorMutation]) -> Vec<(String, String, ToolStatus)> {
    settled(mutations)
        .into_iter()
        .filter_map(|item| match item.kind {
            ConversationItemKind::Tool {
                name,
                summary,
                status,
                ..
            } => Some((name, summary, status)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_script_is_one_code_item_listing_the_tools_it_used() {
    let (connector, mutations) = replay("repl-parallel");
    assert_eq!(
        tools(&mutations),
        vec![(
            "Code".to_owned(),
            "Script · used Bash, Read".to_owned(),
            ToolStatus::Succeeded
        )],
        "the in-script Bash and Read calls are not items of their own"
    );
    // The code is never shown, in any upsert of the item. (The prompt
    // itself mentions REPL.)
    for mutation in &mutations {
        if let ConnectorMutation::Upsert(
            item @ ObservedItem {
                kind: ConversationItemKind::Tool { .. },
                ..
            },
        ) = mutation
        {
            let text = serde_json::to_string(item).unwrap();
            assert!(!text.contains("Promise.all"), "{text}");
            assert!(!text.contains("REPL"), "{text}");
        }
    }
    let answer = settled(&mutations).into_iter().any(|item| {
        matches!(
            item.kind,
            ConversationItemKind::Message {
                role: MessageRole::Assistant,
                ref text,
                ..
            } if text.contains("hello from fixture")
        )
    });
    assert!(answer, "the answer after the script still shows");
    assert!(!connector.tool_running);
}

#[test]
fn a_running_script_names_each_tool_as_it_starts() {
    let (_, mutations) = replay("repl-parallel");
    let running: Vec<String> = mutations
        .iter()
        .filter_map(|mutation| match mutation {
            ConnectorMutation::Upsert(ObservedItem {
                kind:
                    ConversationItemKind::Tool {
                        summary,
                        status: ToolStatus::Running,
                        ..
                    },
                ..
            }) => Some(summary.clone()),
            _ => None,
        })
        .collect();
    // The engine wrote the script's first calls before its `tool_use`.
    assert_eq!(running, vec!["Script · running Bash, Read".to_owned()]);
}

#[test]
fn a_refused_call_is_named_and_the_script_still_succeeds() {
    let (_, mutations) = replay("refused-call");
    assert_eq!(
        tools(&mutations),
        vec![(
            "Code".to_owned(),
            "Script · used Bash · 1 call refused (Bash)".to_owned(),
            ToolStatus::Succeeded
        )]
    );
}

#[test]
fn a_script_that_throws_ends_failed_with_its_error() {
    let (_, mutations) = replay("script-error");
    assert_eq!(
        tools(&mutations),
        vec![(
            "Code".to_owned(),
            "Script · used Bash · failed: Error: probe failure after one call".to_owned(),
            ToolStatus::Failed
        )]
    );
}

#[test]
fn without_its_tool_use_result_the_outcome_is_read_from_the_result_text() {
    let block = serde_json::json!({
        "type": "tool_result",
        "tool_use_id": "toolu_repl",
        "is_error": true,
        "content": "<repl-output>\n<tools-used-before-summary>Bash, Edit</tools-used-before-summary>\n\
                    <refused>Tool calls that were refused while this script ran:\n\
                    - \"Edit\", refused: \"No\"</refused>\n<error>TypeError: x is undefined\nat line 2</error>",
    });
    assert_eq!(
        code_mode::script_outcome("Fix the build", &block, None),
        (
            ToolStatus::Failed,
            "Fix the build · used Bash, Edit · 1 call refused (Edit) · failed: TypeError: x is undefined"
                .to_owned()
        )
    );
}

#[test]
fn a_task_list_written_inside_a_script_is_still_shown() {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    let records = [
        serde_json::json!({"type": "user", "uuid": "u1", "parentUuid": null,
            "timestamp": "2026-11-23T16:00:00.000Z", "message": {"content": "plan it"}}),
        serde_json::json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "timestamp": "2026-11-23T16:00:01.000Z", "message": {"content": [
                {"type": "tool_use", "id": "toolu_repl", "name": "REPL",
                 "input": {"code": "await tools.TodoWrite({todos})", "description": "Write the plan"}}]}}),
        serde_json::json!({"type": "user", "uuid": "r1", "parentUuid": "a1",
            "timestamp": "2026-11-23T16:00:02.000Z", "toolUseResult": {"toolsUsed": ["TodoWrite"]},
            "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_repl", "content": "ok"}]}}),
        serde_json::json!({"type": "assistant", "uuid": "v1", "parentUuid": "r1", "isVirtual": true,
            "ranByToolUseId": "toolu_repl", "timestamp": "2026-11-23T16:00:02.000Z",
            "message": {"content": [{"type": "tool_use", "id": "toolu_cc_1", "name": "TodoWrite",
                "input": {"todos": [{"content": "Read the code", "status": "in_progress"}]}}]}}),
        serde_json::json!({"type": "user", "uuid": "v2", "parentUuid": "v1", "isVirtual": true,
            "timestamp": "2026-11-23T16:00:02.000Z",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_cc_1", "content": ""}]}}),
    ];
    let mut mutations = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let event = record["type"].as_str().unwrap().to_owned();
        mutations.extend(connector.claude_record(
            record.as_object().unwrap(),
            &event,
            index as u64,
        ));
    }
    let items = settled(&mutations);
    assert!(items.iter().any(|item| matches!(
        &item.kind,
        ConversationItemKind::Activity(ConversationActivity::TaskList { tasks })
            if tasks.len() == 1 && tasks[0].title == "Read the code"
    )));
    assert_eq!(
        tools(&mutations),
        vec![(
            "Code".to_owned(),
            "Write the plan · used TodoWrite".to_owned(),
            ToolStatus::Succeeded
        )]
    );
    assert_eq!(
        connector.active_chain.last().map(String::as_str),
        Some("v2"),
        "the script's calls stay on the chain the next record continues"
    );
}

#[test]
fn a_permission_inside_a_script_says_a_no_refuses_that_call_only() {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    let hello = serde_json::json!({
        "hook_event_name": crate::observer::CLAUDE_BRIDGE_EVENT,
        "bridge_event": "hello",
        "bridge_version": 5,
        "timestamp": "2026-11-23T16:13:16.000Z",
    });
    connector.claude_record(hello.as_object().unwrap(), "hook", 1);
    let open = serde_json::json!({
        "hook_event_name": crate::observer::CLAUDE_BRIDGE_EVENT,
        "bridge_event": "permission.open",
        "bridge_version": 5,
        "timestamp": "2026-11-23T16:13:17.168Z",
        "tool_use_id": "toolu_cc_47829608009f4d819da30dd90e1aa218",
        "tool": "Bash",
        "input": {"command": "touch made.txt"},
        "in_script": true,
        "options": [{"id": "deny_with_feedback", "label": "No, and tell Claude what to do instead", "requires_text": true}],
    });
    let mutations = connector.claude_record(open.as_object().unwrap(), "hook", 2);
    let request = settled(&mutations)
        .into_iter()
        .find_map(|item| match item.kind {
            ConversationItemKind::Request {
                request_id,
                metadata,
                ..
            } => Some((request_id, metadata)),
            _ => None,
        })
        .expect("the request is announced");
    assert_eq!(request.0, "toolu_cc_47829608009f4d819da30dd90e1aa218");
    assert_eq!(
        request.1.details.get("reason").and_then(Value::as_str),
        Some("A Code Mode script made this call. No refuses this call only; the script goes on.")
    );
    assert_ne!(request.1.answerable, Some(false), "the phone can answer it");
    assert_eq!(
        connector.state().pending_request.as_deref(),
        Some("toolu_cc_47829608009f4d819da30dd90e1aa218")
    );
}
