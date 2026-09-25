//! Opt-in smoke test against an installed, authenticated Cursor CLI.
//! cargo build -p latch -p latchd
//! cargo test -p latch --lib cursor_chat_sends_and_observes_two_turns -- --ignored --nocapture
use std::process::Command;
use std::time::{Duration, Instant};

use crate::conversation::{
    connectors::connector_for_session, ConnectorAction, ConnectorMutation, ConversationId,
    ConversationItemKind, MessageRole, PollBudget, ACTION_SEND_MESSAGE,
};
use crate::session::paths::LatchHome;
use serde_json::json;

#[test]
#[ignore = "requires an authenticated Cursor agent and makes two small model requests"]
fn cursor_chat_sends_and_observes_two_turns() {
    let temp = tempfile::Builder::new()
        .prefix("cursor-chat-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = LatchHome::new(temp.path().join("home"));
    let binary_path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("latch");
    let binary = binary_path.to_str().unwrap();
    let manifest = json!({
        "format_version": 1,
        "launch": {
            "argv": ["agent", "--mode", "ask"], "agent": "cursor",
            "login_shell": {"path": "/bin/zsh"},
            "cwd": std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap(),
            "env": {}, "inherit_env": true, "size": {"cols": 100, "rows": 30}
        }, "display": {"name": "Cursor chat smoke test"}
    });
    let manifest_path = temp.path().join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = Command::new(binary)
        .env("LATCH_HOME", home.root())
        .env_remove("LATCH_SESSION_ID")
        .env("LATCHD_SOCKET_DIR", temp.path().join("s"))
        .args(["create", "--json", "--manifest-file"])
        .arg(&manifest_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let session = report["session"]["id"]
        .as_str()
        .expect("created session id");
    struct Stop<'a>(&'a str, &'a LatchHome, &'a str);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = Command::new(self.0)
                .env("LATCH_HOME", self.1.root())
                .args(["stop", self.2, "--force"])
                .output();
        }
    }
    let _stop = Stop(binary, &home, session);
    let conversation = ConversationId::new(session);
    let mut connector = connector_for_session(home.clone(), &conversation);
    let mut can_send = false;
    for expected in ["LATCH_CURSOR_FIRST", "LATCH_CURSOR_SECOND"] {
        let deadline = Instant::now() + Duration::from_secs(90);
        while !can_send {
            let result = connector
                .poll(PollBudget {
                    max_records: 100,
                    deadline: Duration::from_secs(3),
                })
                .unwrap();
            if result.mutations.iter().any(|mutation| {
                matches!(mutation,
                ConnectorMutation::State(state) if state.send_message.enabled)
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Cursor never offered its empty composer"
            );
            connector
                .wait_for_activity(Duration::from_millis(200), Duration::from_millis(500))
                .unwrap();
        }
        can_send = false;
        let result = connector.apply(ConnectorAction {
            id: ACTION_SEND_MESSAGE.into(),
            payload: json!({"text": format!("Reply with exactly {expected}. Do not use tools.")}),
        }, Duration::from_secs(5)).unwrap();
        assert!(
            matches!(result, crate::conversation::ApplyResult::Accepted { .. }),
            "{result:?}"
        );
        loop {
            let result = connector
                .poll(PollBudget {
                    max_records: 100,
                    deadline: Duration::from_secs(3),
                })
                .unwrap();
            for mutation in &result.mutations {
                if let ConnectorMutation::State(state) = mutation {
                    can_send = state.send_message.enabled;
                }
            }
            if result.mutations.iter().any(|mutation| matches!(mutation,
                ConnectorMutation::Upsert(item) if matches!(&item.kind,
                    ConversationItemKind::Message { role: MessageRole::Assistant, text, .. } if text.contains(expected)))) { break; }
            assert!(
                Instant::now() < deadline,
                "Cursor response did not reach the connector"
            );
            connector
                .wait_for_activity(Duration::from_millis(200), Duration::from_millis(500))
                .unwrap();
        }
        eprintln!("Observed Cursor reply: {expected}");
    }
}
