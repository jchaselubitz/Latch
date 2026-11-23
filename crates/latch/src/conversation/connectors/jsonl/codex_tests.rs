use super::*;
use crate::conversation::{ConversationActivity as Activity, TurnOutcome};

fn replay(path: &str) -> (Vec<ObservedItem>, Vec<ConversationState>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/conversation/codex")
        .join(path);
    let mut connector = JsonlConnector::fixture("codex", path);
    let mut items = Vec::new();
    let mut states = Vec::new();
    loop {
        let batch = connector
            .poll(PollBudget {
                max_records: 1,
                max_bytes: 256 * 1024,
                deadline: Duration::from_secs(1),
            })
            .unwrap();
        for change in batch.mutations {
            match change {
                ConnectorMutation::Upsert(item) => items.push(item),
                ConnectorMutation::State(state) => states.push(state),
                _ => {}
            }
        }
        if !batch.has_more_source_work {
            break;
        }
    }
    (items, states)
}

#[test]
fn captured_approval_clients_project_live_turns_and_the_command_lifecycle() {
    for path in [
        "app-server/command-approval.owner-client.jsonl",
        "app-server/command-approval.second-client.jsonl",
    ] {
        let (items, states) = replay(path);
        let tools: Vec<_> = items
            .iter()
            .filter_map(|item| match &item.kind {
                ConversationItemKind::Tool {
                    name,
                    summary,
                    status,
                    ..
                } if name == "exec_command" => {
                    assert!(summary.contains("touch probe-file.txt") && summary.contains("/work"));
                    Some((&item.id, status))
                }
                _ => None,
            })
            .collect();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0], (tools[1].0, &ToolStatus::Running));
        assert_eq!(tools[1].1, &ToolStatus::Failed);
        assert!(items.iter().any(|item| matches!(
            item.kind,
            ConversationItemKind::Message {
                role: MessageRole::User,
                ..
            }
        )));
        assert!(states
            .iter()
            .any(|state| state.phase == ConversationPhase::Working));
        assert_eq!(states.last().unwrap().phase, ConversationPhase::Idle);
        assert_eq!(
            states.last().unwrap().turn_outcome,
            Some(TurnOutcome::Answer)
        );
    }
}

#[test]
fn rich_live_activity_links_file_diffs_and_keeps_child_messages_nested() {
    let (items, states) = replay("app-server/rich-activity.jsonl");
    let named = |id: &'static str| items.iter().filter(move |item| item.id.as_str() == id);
    assert!(matches!(&named("command").next_back().unwrap().kind,
        ConversationItemKind::Tool { summary, status: ToolStatus::Succeeded, .. }
            if summary.contains("exit 0") && summary.contains("sample output")));
    assert!(matches!(
        &named("generation").next_back().unwrap().kind,
        ConversationItemKind::Tool {
            status: ToolStatus::Failed,
            ..
        }
    ));
    assert!(matches!(&named("reason").next_back().unwrap().kind,
        ConversationItemKind::Activity(Activity::ReasoningSummary { text }) if text == "Check the file."));
    let diffs: Vec<_> = items.iter().filter(|item| matches!(&item.kind,
        ConversationItemKind::Activity(Activity::FileChange { tool_item_id: Some(tool), diff_summary, .. })
            if tool.as_str() == "patch" && diff_summary.contains("-old\n+new"))).collect();
    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0].id, diffs[1].id);
    let child = named("codex:subagent:child").next_back().unwrap();
    assert!(matches!(&child.kind,
        ConversationItemKind::Activity(Activity::Subagent { status, .. }) if status == "completed"));
    assert_eq!(
        named("child-message")
            .next_back()
            .unwrap()
            .parent_subagent_id
            .as_ref(),
        Some(&child.id)
    );
    assert_eq!(
        named("child-tool")
            .next_back()
            .unwrap()
            .parent_subagent_id
            .as_ref(),
        Some(&child.id)
    );
    assert!(items.iter().any(|item| matches!(
        item.kind,
        ConversationItemKind::Activity(Activity::PlanUpdate { .. })
    )));
    assert!(items.iter().any(|item| matches!(&item.kind,
        ConversationItemKind::Activity(Activity::Notice { notice_kind, .. }) if notice_kind == "interrupted")));
    assert_eq!(
        states
            .last()
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .context_percent,
        Some(40)
    );
    assert_eq!(states.last().unwrap().phase, ConversationPhase::Idle);
    assert_eq!(
        states.last().unwrap().turn_outcome,
        Some(TurnOutcome::Aborted)
    );
}

#[test]
fn rollout_recovers_completed_activity_and_turn_state_without_setup() {
    let (items, states) = replay("rollout-activity.jsonl");
    let encoded = serde_json::to_string(&items).unwrap();
    assert!(!encoded.contains("never project this"));
    for name in [
        "exec_command",
        "apply_patch",
        "example/lookup",
        "web_search",
        "view_image",
    ] {
        assert!(
            items.iter().any(|item| matches!(&item.kind,
            ConversationItemKind::Tool { name: observed, status, .. }
                if name == observed && *status != ToolStatus::Running)),
            "{name}"
        );
    }
    for kind in ["compaction", "model_reroute", "rate_limit", "interrupted"] {
        assert!(
            items.iter().any(|item| matches!(&item.kind,
            ConversationItemKind::Activity(Activity::Notice { notice_kind, .. })
                if kind == notice_kind)),
            "{kind}"
        );
    }
    assert!(encoded.contains("Read notes"));
    assert!(encoded.contains("Review the notes first."));
    assert!(encoded.contains("codex:subagent:child-1"));
    assert!(states
        .iter()
        .any(|state| state.turn_outcome == Some(TurnOutcome::Aborted)));
    assert!(states
        .iter()
        .any(|state| state.phase == ConversationPhase::Working));
    assert_eq!(states.last().unwrap().phase, ConversationPhase::Idle);
    assert_eq!(
        states
            .last()
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .context_percent,
        Some(20)
    );
}
