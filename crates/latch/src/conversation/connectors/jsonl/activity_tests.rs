//! Replays of the live captures in `fixtures/conversation/claude/activity`
//! (coo:1334.m1ng): subagents, tasks, notices and session state.
use super::*;
use crate::conversation::{ActivityStep, ConversationActivity};

const SESSION: &str = "dde3ace3-2cf2-4b13-bd4c-62b94fbcda42";
const AGENT: &str = "ae6a238f4d47fe252";
/// The assistant record whose only block is the Agent call.
const AGENT_CALL_ITEM: &str = "74fe6b70-0d75-4d15-89f2-29df71ed5ff7";

fn fixture_dir(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/conversation/claude/activity")
        .join(case)
}

fn fixture_lines(case: &str, file: &str) -> Vec<Value> {
    let path = fixture_dir(case).join(file);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn poll_all(connector: &mut JsonlConnector) -> Vec<ConnectorMutation> {
    let budget = PollBudget {
        max_records: 512,
        max_bytes: crate::conversation::MAX_CONVERSATION_BATCH_BYTES,
        deadline: Duration::from_secs(5),
    };
    let mut mutations = Vec::new();
    loop {
        let result = connector.poll(budget.clone()).unwrap();
        // As the Hub does: a poll that only skipped bookkeeping records
        // still says whether more source work is waiting.
        if result.mutations.is_empty() && !result.has_more_source_work {
            break;
        }
        mutations.extend(result.mutations);
    }
    mutations
}

/// A connector over a copy of the capture laid out as Claude writes it: the
/// transcript, `<session>/subagents/` beside it, and the hook sidecar.
fn replay_connector(dir: &std::path::Path, hooks: &str) -> JsonlConnector {
    let case = fixture_dir("subagent-tasks");
    let project = dir.join("project");
    let subagents = project.join(SESSION).join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    let source = project.join(format!("{SESSION}.jsonl"));
    fs::copy(case.join("transcript.jsonl"), &source).unwrap();
    for file in [
        format!("agent-{AGENT}.jsonl"),
        format!("agent-{AGENT}.meta.json"),
    ] {
        fs::copy(case.join("subagents").join(&file), subagents.join(&file)).unwrap();
    }
    let home = LatchHome::new(dir.join("home"));
    let session = SessionId::parse("ses_fixture").unwrap();
    fs::create_dir_all(home.session(&session).dir()).unwrap();
    fs::write(home.session(&session).conversation_source_hooks(), hooks).unwrap();
    let mut connector = JsonlConnector::fixture("claude", source);
    connector.home = home;
    connector
}

fn upserts(mutations: &[ConnectorMutation]) -> Vec<&ObservedItem> {
    mutations
        .iter()
        .filter_map(|mutation| match mutation {
            ConnectorMutation::Upsert(item) => Some(item),
            _ => None,
        })
        .collect()
}

fn position(mutations: &[ConnectorMutation], id: &str) -> usize {
    upserts(mutations)
        .iter()
        .position(|item| item.id.as_str() == id)
        .unwrap_or_else(|| panic!("no upsert of {id}"))
}

fn last_kind<'a>(mutations: &'a [ConnectorMutation], id: &str) -> &'a ConversationItemKind {
    &upserts(mutations)
        .into_iter()
        .rev()
        .find(|item| item.id.as_str() == id)
        .unwrap_or_else(|| panic!("no upsert of {id}"))
        .kind
}

fn subagent_item() -> String {
    format!("subagent:{AGENT}")
}

#[test]
fn a_replayed_subagent_nests_its_tool_calls_under_the_agent_call() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = fs::read_to_string(fixture_dir("subagent-tasks").join("hooks.jsonl")).unwrap();
    let mut connector = replay_connector(dir.path(), &hooks);
    let mutations = poll_all(&mut connector);

    // The subagent follows the call that started it, and its own calls
    // follow it, whatever order the hooks and transcripts were read in.
    let call = position(&mutations, AGENT_CALL_ITEM);
    let subagent = position(&mutations, &subagent_item());
    assert!(call < subagent, "the Agent call precedes its subagent");
    let ConversationItemKind::Activity(ConversationActivity::Subagent {
        subagent_id,
        agent_type,
        status,
        summary,
        parent_tool_id,
    }) = last_kind(&mutations, &subagent_item())
    else {
        panic!("not a subagent item");
    };
    assert_eq!(subagent_id, AGENT);
    assert_eq!(agent_type, "general-purpose");
    assert_eq!(status, "completed");
    assert_eq!(
        parent_tool_id.as_ref().map(ConversationItemId::as_str),
        Some(AGENT_CALL_ITEM)
    );
    assert!(
        summary.starts_with("List current directory files"),
        "{summary}"
    );
    assert!(
        summary.contains("a.txt"),
        "the answer is in the summary: {summary}"
    );

    let children: Vec<_> = upserts(&mutations)
        .into_iter()
        .filter(|item| {
            item.parent_subagent_id
                .as_ref()
                .map(ConversationItemId::as_str)
                == Some(subagent_item().as_str())
        })
        .collect();
    assert!(!children.is_empty(), "the subagent's Bash call is nested");
    let ConversationItemKind::Tool { name, status, .. } = &children.last().unwrap().kind else {
        panic!("a nested tool");
    };
    assert_eq!(name, "Bash");
    assert_eq!(*status, ToolStatus::Succeeded);
    assert!(position(&mutations, children[0].id.as_str()) > subagent);
}

#[test]
fn a_task_notification_updates_its_subagent_instead_of_reading_as_a_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // Without the observer's hooks the transcript alone still tells the story.
    let mut connector = replay_connector(dir.path(), "");
    let mutations = poll_all(&mut connector);
    assert!(upserts(&mutations).iter().all(|item| !matches!(
        &item.kind,
        ConversationItemKind::Message { role: MessageRole::User, text, .. }
            if text.contains("task-notification")
    )));
    // No `SubagentStart` was seen, so no subagent is invented either.
    assert!(upserts(&mutations)
        .iter()
        .all(|item| item.id.as_str() != subagent_item()));
}

#[test]
fn tasks_form_one_list_per_turn_placed_where_the_transcript_made_them() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = fs::read_to_string(fixture_dir("subagent-tasks").join("hooks.jsonl")).unwrap();
    let mut connector = replay_connector(dir.path(), &hooks);
    let mutations = poll_all(&mut connector);
    let lists: Vec<_> = upserts(&mutations)
        .into_iter()
        .filter(|item| {
            matches!(
                item.kind,
                ConversationItemKind::Activity(ConversationActivity::TaskList { .. })
            )
        })
        .collect();
    let ids: std::collections::BTreeSet<_> = lists.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids.len(), 1, "one list for the turn: {ids:?}");
    let prompt = upserts(&mutations)
        .iter()
        .position(|item| {
            matches!(
                item.kind,
                ConversationItemKind::Message {
                    role: MessageRole::User,
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        position(&mutations, lists[0].id.as_str()) > prompt,
        "the list follows the prompt that asked for it"
    );
    let ConversationItemKind::Activity(ConversationActivity::TaskList { tasks }) =
        &lists.last().unwrap().kind
    else {
        unreachable!()
    };
    assert_eq!(
        tasks,
        &vec![
            ActivityStep {
                id: "1".into(),
                title: "Count files".into(),
                status: "completed".into(),
            },
            ActivityStep {
                id: "2".into(),
                title: "Summarize".into(),
                status: "completed".into(),
            },
        ]
    );
}

#[test]
fn session_state_carries_model_effort_and_permission_mode() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = fs::read_to_string(fixture_dir("subagent-tasks").join("hooks.jsonl")).unwrap();
    let mut connector = replay_connector(dir.path(), &hooks);
    poll_all(&mut connector);
    let state = connector.state();
    assert_eq!(state.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(state.effort.as_deref(), Some("medium"));
    assert_eq!(state.permission_mode.as_deref(), Some("default"));

    // The bridge's usage report fills in the context window.
    let usage = serde_json::json!({
        "hook_event_name": crate::observer::CLAUDE_BRIDGE_EVENT,
        "bridge_event": "session.usage",
        "timestamp": "2026-11-23T15:49:00.000Z",
        "model": "claude-sonnet-5-5",
        "context_tokens": 50204,
        "context_window": 200000,
        "context_percent": 25.1,
        "rate_limits": [{"kind": "five_hour", "percent_used": 12.6, "resets_at": "2026-11-23T18:00:00Z"}],
        "cost_usd": 0.0152,
    });
    connector.claude_record(usage.as_object().unwrap(), "", 99);
    let state = connector.state();
    assert_eq!(state.model.as_deref(), Some("claude-sonnet-5-5"));
    let usage = state.usage.unwrap();
    assert_eq!(usage.context_tokens, Some(50204));
    assert_eq!(usage.context_window, Some(200000));
    assert_eq!(usage.context_percent, Some(25));
    assert_eq!(usage.rate_limits[0].percent_used, 13);
    assert_eq!(usage.cost_cents, Some(2));

    // An older transcript record replayed later does not undo a newer value.
    let mut old = serde_json::json!({
        "type": "user", "uuid": "late", "parentUuid": null,
        "timestamp": "2026-11-23T15:00:00.000Z", "permissionMode": "plan",
        "message": {"role": "user", "content": "hi"},
    });
    connector.claude_record(old.as_object().unwrap(), "user", 100);
    assert_eq!(
        connector.state().permission_mode.as_deref(),
        Some("default")
    );
    old["timestamp"] = Value::from("2026-11-23T16:00:00.000Z");
    old["uuid"] = Value::from("later");
    connector.claude_record(old.as_object().unwrap(), "user", 101);
    assert_eq!(connector.state().permission_mode.as_deref(), Some("plan"));
}

#[test]
fn activity_survives_a_checkpoint_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = fs::read_to_string(fixture_dir("subagent-tasks").join("hooks.jsonl")).unwrap();
    let mut connector = replay_connector(dir.path(), &hooks);
    poll_all(&mut connector);
    let checkpoint = connector.checkpoint_snapshot().unwrap();
    let mut restored = replay_connector(dir.path(), &hooks);
    restored.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(restored.activity, connector.activity);
    assert!(
        poll_all(&mut restored).is_empty(),
        "nothing is replayed twice"
    );
}

fn hook_mutations(case: &str) -> Vec<ConnectorMutation> {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    fixture_lines(case, "hooks.jsonl")
        .into_iter()
        .enumerate()
        .flat_map(|(index, record)| {
            connector.claude_record(record.as_object().unwrap(), "", index as u64 + 1)
        })
        .collect()
}

fn notices(mutations: &[ConnectorMutation]) -> Vec<(String, String, String)> {
    upserts(mutations)
        .into_iter()
        .filter_map(|item| match &item.kind {
            ConversationItemKind::Activity(ConversationActivity::Notice { notice_kind, text }) => {
                Some((
                    item.id.as_str().to_owned(),
                    notice_kind.clone(),
                    text.clone(),
                ))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn compaction_is_one_notice_that_settles_and_its_fork_is_no_subagent() {
    let mutations = hook_mutations("compaction");
    let notices = notices(&mutations);
    assert_eq!(notices.len(), 2);
    assert_eq!(
        notices[0].0, notices[1].0,
        "PostCompact settles the same item"
    );
    assert_eq!(notices[0].1, "compaction");
    assert_eq!(notices[1].2, "Conversation compacted");
    assert!(upserts(&mutations).iter().all(|item| !matches!(
        item.kind,
        ConversationItemKind::Activity(ConversationActivity::Subagent { .. })
    )));
}

#[test]
fn a_model_switch_is_a_notice_and_the_new_model() {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    let mut mutations = Vec::new();
    for (index, record) in fixture_lines("model-switch", "hooks.jsonl")
        .into_iter()
        .enumerate()
    {
        mutations.extend(connector.claude_record(record.as_object().unwrap(), "", index as u64));
    }
    assert_eq!(
        notices(&mutations)
            .into_iter()
            .map(|(_, kind, text)| (kind, text))
            .collect::<Vec<_>>(),
        vec![(
            "model_switch".to_owned(),
            "Switched model to claude-sonnet-5-5 from claude-haiku-5-5".to_owned()
        )],
        "PreModelSwitch is not registered by the observer and adds nothing"
    );
    assert_eq!(
        connector.state().model.as_deref(),
        Some("claude-sonnet-5-5")
    );
}

#[test]
fn a_stop_failure_closes_the_turn_with_an_error_notice() {
    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("/nonexistent/s.jsonl"));
    connector.turn_open = true;
    let record = fixture_lines("stop-failure", "hooks.jsonl").remove(0);
    let mutations = connector.claude_record(record.as_object().unwrap(), "", 1);
    let notices = notices(&mutations);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, "api_error");
    assert!(notices[0].2.starts_with("The model is not available: "));
    assert!(!connector.turn_open);
    assert_eq!(
        connector.state().turn_outcome,
        Some(crate::conversation::TurnOutcome::Error)
    );
}

#[test]
fn a_subagent_transcript_is_read_in_bounded_steps() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = fs::read_to_string(fixture_dir("subagent-tasks").join("hooks.jsonl")).unwrap();
    let mut connector = replay_connector(dir.path(), &hooks);
    // A long-running subagent: many more calls than are shown.
    let subagent = dir
        .path()
        .join("project")
        .join(SESSION)
        .join("subagents")
        .join(format!("agent-{AGENT}.jsonl"));
    let mut extra = String::new();
    for index in 0..150 {
        extra.push_str(
            &serde_json::json!({
                "type": "assistant", "uuid": format!("extra-{index}"),
                "timestamp": "2026-11-23T15:48:11.000Z", "isSidechain": true,
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": format!("toolu_extra_{index}"), "name": "Read",
                     "input": {"file_path": "/work/a.txt"}}
                ]},
            })
            .to_string(),
        );
        extra.push('\n');
    }
    let mut file = fs::OpenOptions::new().append(true).open(&subagent).unwrap();
    std::io::Write::write_all(&mut file, extra.as_bytes()).unwrap();
    let budget = PollBudget {
        max_records: 40,
        max_bytes: crate::conversation::MAX_CONVERSATION_BATCH_BYTES,
        deadline: Duration::from_secs(5),
    };
    let mut polls = 0;
    let mut mutations = Vec::new();
    loop {
        let result = connector.poll(budget.clone()).unwrap();
        polls += 1;
        if result.mutations.is_empty() {
            break;
        }
        mutations.extend(result.mutations);
        assert!(polls < 50);
    }
    assert!(polls > 3, "the backlog took several bounded polls");
    let nested = upserts(&mutations)
        .into_iter()
        .filter(|item| item.parent_subagent_id.is_some())
        .map(|item| item.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        nested.len(),
        60,
        "at most MAX_SUBAGENT_TOOLS calls are shown"
    );
    let ConversationItemKind::Activity(ConversationActivity::Subagent { summary, .. }) =
        last_kind(&mutations, &subagent_item())
    else {
        unreachable!()
    };
    assert!(
        summary.ends_with("91 more tool calls not shown"),
        "{summary}"
    );
}

/// Replays a real hosted session: `LATCH_REPLAY_HOME=<latch home>
/// LATCH_REPLAY_SESSION=ses_… cargo test -p latch --lib
/// replays_a_live_session_home -- --ignored --nocapture` prints the activity
/// and the state a phone would get.
#[test]
#[ignore = "needs LATCH_REPLAY_HOME and LATCH_REPLAY_SESSION"]
fn replays_a_live_session_home() {
    let home = LatchHome::new(std::env::var("LATCH_REPLAY_HOME").unwrap());
    let session = SessionId::parse(&std::env::var("LATCH_REPLAY_SESSION").unwrap()).unwrap();
    let mut connector = JsonlConnector::for_session(home, session);
    connector.live_screen = false;
    connector.refresh_screen = false;
    let mutations = poll_all(&mut connector);
    for item in upserts(&mutations) {
        let nested = item
            .parent_subagent_id
            .as_ref()
            .map_or(String::new(), |parent| {
                format!("  [in {}]", parent.as_str())
            });
        match &item.kind {
            ConversationItemKind::Activity(activity) => {
                println!(
                    "{}{nested}: {}",
                    item.id.as_str(),
                    serde_json::to_string(activity).unwrap()
                )
            }
            ConversationItemKind::Tool { name, status, .. }
                if item.parent_subagent_id.is_some() =>
            {
                println!("{}{nested}: {name} {status:?}", item.id.as_str())
            }
            _ => {}
        }
    }
    println!(
        "state: {}",
        serde_json::to_string(&connector.state()).unwrap()
    );
}
