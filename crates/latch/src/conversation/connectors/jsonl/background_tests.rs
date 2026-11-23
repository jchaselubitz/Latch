//! Replays of the live capture in
//! `fixtures/conversation/claude/activity/background-work` (coo:1334.4s0s):
//! the background work a `Stop` reports, and what clears it.
use super::*;
use crate::conversation::BackgroundWork;

fn fixture_hooks() -> Vec<serde_json::Map<String, Value>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/conversation/claude/activity/background-work/hooks.jsonl");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .map(|value| value.as_object().unwrap().clone())
        .collect()
}

fn event(hook: &serde_json::Map<String, Value>) -> &str {
    hook["hook_event_name"].as_str().unwrap()
}

fn connector() -> (tempfile::TempDir, JsonlConnector) {
    let dir = tempfile::tempdir().unwrap();
    let connector = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    (dir, connector)
}

/// Feeds the capture up to and including its first `Stop`.
fn through_first_stop(connector: &mut JsonlConnector) -> Vec<serde_json::Map<String, Value>> {
    let hooks = fixture_hooks();
    let first_stop = hooks.iter().position(|hook| event(hook) == "Stop").unwrap();
    for (ordinal, hook) in hooks[..=first_stop].iter().enumerate() {
        connector.claude_record(hook, "hook", ordinal as u64 + 1);
    }
    hooks
}

fn prompt(uuid: &str, parent: Option<&str>, at: &str) -> Value {
    serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": parent,
        "timestamp": at,
        "message": { "role": "user", "content": "What next?" },
    })
}

fn running(count: u32, kinds: &[&str]) -> Option<BackgroundWork> {
    Some(BackgroundWork {
        count,
        kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
    })
}

#[test]
fn a_turn_that_ends_with_work_running_reports_it_until_an_empty_stop() {
    let (_dir, mut connector) = connector();
    let mut seen = Vec::new();
    for (ordinal, hook) in fixture_hooks().iter().enumerate() {
        connector.claude_record(hook, "hook", ordinal as u64 + 1);
        if event(hook) == "Stop" {
            let state = connector.state();
            // The turn is over even though work continues.
            assert_eq!(state.phase, ConversationPhase::Idle);
            seen.push(state.background_work);
        }
    }
    assert_eq!(
        seen,
        vec![
            running(3, &["subagent", "shell", "monitor"]),
            running(3, &["shell", "monitor"]),
            running(3, &["shell", "monitor"]),
            running(3, &["shell", "monitor"]),
            running(2, &["shell", "monitor"]),
            running(1, &["monitor"]),
            None,
        ]
    );
}

#[test]
fn a_subagent_stop_does_not_replace_the_main_report() {
    let (_dir, mut connector) = connector();
    let hooks = through_first_stop(&mut connector);
    let subagent_stop = hooks
        .iter()
        .find(|hook| event(hook) == "SubagentStop")
        .unwrap();
    // It lists the subagent itself and its own task: four entries.
    assert_eq!(
        subagent_stop["background_tasks"].as_array().unwrap().len(),
        4
    );
    connector.claude_record(subagent_stop, "hook", 99);
    assert_eq!(
        connector.state().background_work,
        running(3, &["subagent", "shell", "monitor"])
    );
}

#[test]
fn session_end_clears_work_no_later_stop_reported() {
    let (_dir, mut connector) = connector();
    let hooks = through_first_stop(&mut connector);
    assert!(connector.state().background_work.is_some());
    let end = hooks
        .iter()
        .find(|hook| event(hook) == "SessionEnd")
        .unwrap();
    connector.claude_record(end, "hook", 99);
    assert_eq!(connector.state().background_work, None);
}

#[test]
fn the_next_prompt_clears_it_but_a_replayed_older_prompt_does_not() {
    let (_dir, mut connector) = connector();
    through_first_stop(&mut connector);
    // Hooks are read before the transcript: the prompt that started the
    // reported turn replays after its Stop and must not clear the report.
    let older = prompt("user-1", None, "2026-11-23T17:00:02.100Z");
    connector.claude_record(older.as_object().unwrap(), "user", 50);
    assert!(connector.state().background_work.is_some());

    let next = prompt("user-2", Some("user-1"), "2026-11-23T17:05:00.000Z");
    connector.claude_record(next.as_object().unwrap(), "user", 51);
    let state = connector.state();
    assert_eq!(state.phase, ConversationPhase::Working);
    assert_eq!(state.background_work, None);
}

#[test]
fn a_task_notification_turn_keeps_the_report_until_its_own_stop() {
    let (_dir, mut connector) = connector();
    through_first_stop(&mut connector);
    let notification = serde_json::json!({
        "type": "user",
        "uuid": "note-1",
        "timestamp": "2026-11-23T17:05:00.000Z",
        "origin": { "kind": "task-notification" },
        "message": { "role": "user", "content": "<task-notification>\n<task-id>bbpb1b4y8</task-id>\n<status>completed</status>\n</task-notification>" },
    });
    connector.claude_record(notification.as_object().unwrap(), "user", 50);
    assert!(connector.state().background_work.is_some());
}

#[test]
fn the_report_survives_a_checkpoint_and_a_rebind_drops_it() {
    let (dir, mut connector) = connector();
    through_first_stop(&mut connector);
    let saved = connector.saved_checkpoint().unwrap();
    let mut restored = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    restored.restore_saved_checkpoint(&saved).unwrap();
    assert_eq!(
        restored.state().background_work,
        running(3, &["subagent", "shell", "monitor"])
    );

    // An older checkpoint without the field restores to none.
    let mut legacy: Value = serde_json::from_slice(&saved).unwrap();
    legacy["runtime"]
        .as_object_mut()
        .unwrap()
        .retain(|key, _| !key.starts_with("background_work"));
    let mut old = JsonlConnector::fixture("claude", dir.path().join("source.jsonl"));
    old.restore_saved_checkpoint(&serde_json::to_vec(&legacy).unwrap())
        .unwrap();
    assert_eq!(old.state().background_work, None);
}

#[test]
fn settled_unknown_and_odd_entries_are_counted_carefully() {
    let stop = serde_json::json!({
        "hook_event_name": "Stop",
        "background_tasks": [
            { "id": "a", "type": "shell", "status": "running" },
            { "id": "b", "type": "shell", "status": "completed" },
            { "id": "c", "type": "Cloud Session" },
            { "id": "d", "status": "running" },
            "not a task",
            { "id": "e", "type": "mcp_task", "status": "killed" },
        ],
    });
    assert_eq!(
        claude::background_work(stop.as_object().unwrap()),
        running(3, &["shell", "cloud_session", "task"])
    );
    let empty = serde_json::json!({ "hook_event_name": "Stop", "background_tasks": [] });
    assert_eq!(claude::background_work(empty.as_object().unwrap()), None);
    let absent = serde_json::json!({ "hook_event_name": "Stop" });
    assert_eq!(claude::background_work(absent.as_object().unwrap()), None);
}
