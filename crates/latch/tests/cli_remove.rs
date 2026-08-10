//! Targeted session removal through the public management contract.

mod support;

use latch::cli::manage::{self, RemoveRequest};

use support::{latch_cmd, stderr_str, stdout_json, Harness, SETTLE};

#[test]
fn removes_an_exited_session_and_reports_json() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    session.wait_until_gone(SETTLE);
    let id = session.id().to_string();

    let output = latch_cmd(&harness, &["remove", &id, "--json"]);
    assert!(output.status.success(), "{}", stderr_str(&output));
    assert_eq!(
        stdout_json(&output),
        serde_json::json!({ "id": id, "removed": true })
    );
    assert!(!session.paths().dir().exists());
}

#[test]
fn refuses_to_remove_a_live_session_without_force() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while :; do sleep 0.2; done");

    let error = manage::remove(RemoveRequest {
        home: harness.home().clone(),
        session: session.id().to_string(),
        force: false,
    })
    .expect_err("live removal must be explicit");

    assert!(error.to_string().contains("--force"), "{error:#}");
    assert!(session.is_live());
    assert!(session.paths().dir().exists());
}

#[test]
fn force_stops_then_removes_a_live_session() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while :; do sleep 0.2; done");
    let id = session.id().to_string();

    let report = manage::remove(RemoveRequest {
        home: harness.home().clone(),
        session: id.clone(),
        force: true,
    })
    .expect("forced removal should stop through the worker first");

    assert_eq!(report.id, id);
    assert!(report.removed);
    assert!(!session.paths().dir().exists());
}

#[test]
fn removes_a_lost_session_directly() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    session.wait_until_gone(SETTLE);
    std::fs::remove_file(session.paths().exit()).expect("exit record is removable");
    let id = session.id().to_string();

    let report = manage::remove(RemoveRequest {
        home: harness.home().clone(),
        session: id.clone(),
        force: false,
    })
    .expect("lost session has no live worker and can be removed");

    assert_eq!(report.id, id);
    assert!(!session.paths().dir().exists());
}

#[test]
fn missing_session_is_not_reported_as_removed() {
    let harness = Harness::new();
    let error = manage::remove(RemoveRequest {
        home: harness.home().clone(),
        session: "ses_missing".to_owned(),
        force: false,
    })
    .expect_err("a nonexistent session cannot be removed");

    assert!(error.to_string().contains("no session"), "{error:#}");
}
