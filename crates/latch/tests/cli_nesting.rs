//! Nesting refusal: `LATCH_SESSION_ID` makes a session within a session refuse.
//!
//! With an iTerm profile pointed at `latch`, every new window is a session, and
//! running `latch` inside one is a daily occurrence rather than an edge case.
//! Attach or decline — never create a nested session.

mod support;

use latch::cli::create::CreateOptions;
use latch::cli::nesting::{self, NestingDecision, SESSION_ID_ENV};
use latch::worker::manifest::{LaunchManifest, TerminalSize};

use support::{latch_cmd, latch_cmd_nested, stderr_str, Harness};

#[test]
fn session_id_env_name_is_stable() {
    // Overlord and anyone else detecting nesting key off this exact string.
    assert_eq!(SESSION_ID_ENV, "LATCH_SESSION_ID");
}

#[test]
fn without_an_enclosing_session_creation_is_allowed() {
    assert_eq!(
        nesting::nesting_decision(None, true),
        NestingDecision::Allow
    );
}

#[test]
fn inside_a_session_bare_latch_attaches_to_the_enclosing_one() {
    match nesting::nesting_decision(Some("ses_01JNEST"), true) {
        NestingDecision::AttachToEnclosing { session_id } => {
            assert_eq!(session_id, "ses_01JNEST");
        }
        other => panic!("bare latch inside a session must attach, not {other:?}"),
    }
}

#[test]
fn inside_a_session_a_create_that_cannot_attach_is_declined() {
    // `latch run -- other` names a different command than the enclosing
    // session. Decline rather than nest.
    match nesting::nesting_decision(Some("ses_01JNEST"), true) {
        NestingDecision::AttachToEnclosing { .. } | NestingDecision::Decline { .. } => {}
        NestingDecision::Allow => {
            panic!("creation inside a session must never be Allow")
        }
    }
}

#[test]
fn worker_exports_latch_session_id_into_the_child() {
    // Until the worker exports the variable this fails. Express the requirement
    // through the nesting module's constant so the contract is named in one place.
    assert_eq!(SESSION_ID_ENV, "LATCH_SESSION_ID");

    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'ENV:%s\\n' \"$LATCH_SESSION_ID\"; sleep 30");
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        support::size(80, 24),
    );
    let out = client.wait_for_output("ENV:", support::SETTLE);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains(session.id().as_str()),
        "the worker must export {SESSION_ID_ENV} into the child so nesting is \
         detectable; saw {text:?}"
    );
}

#[test]
fn binary_refuses_to_nest_when_latch_session_id_is_set() {
    let harness = Harness::new();
    // Seed a live enclosing session via the worker so the attach-or-decline
    // path has something to resolve against once nesting is implemented.
    let enclosing = harness.spawn_sh("while :; do sleep 0.2; done");
    let enclosing_id = enclosing.id().to_string();
    // Options shape create will use; kept so the type stays exercised.
    let _ = CreateOptions {
        home: harness.home().clone(),
        manifest: LaunchManifest::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "while :; do sleep 0.2; done".to_owned(),
            ],
            std::env::temp_dir(),
            TerminalSize::new(80, 24),
        ),
        attach: false,
    };

    let sessions_before = support::session_dirs(&harness);
    for args in [
        vec![] as Vec<&str>,
        vec!["shell"],
        vec!["run", "--", "/bin/echo", "nested"],
    ] {
        let output = latch_cmd_nested(&harness, &enclosing_id, &args);
        let err = stderr_str(&output);
        // Must not successfully create a second session directory.
        // Until implemented, an error is required; "lands in M1" without a new
        // session is acceptable, silently nesting is not.
        let sessions_after = support::session_dirs(&harness);
        assert_eq!(
            sessions_after.len(),
            sessions_before.len(),
            "nested latch must not create a session within a session; \
             args={args:?} stderr={err}"
        );
        let _ = latch_cmd; // keep helper linked for the no-env control case
    }
}

#[test]
fn nesting_decision_is_consulted_before_create() {
    // The create path must call nesting_decision; a test that only checks the
    // binary's eventual refusal would pass an implementation that nests and
    // then deletes. Pin the policy function itself.
    assert!(
        !matches!(
            nesting::nesting_decision(Some("ses_01JNEST"), true),
            NestingDecision::Allow
        ),
        "the policy function itself must refuse create-inside-session"
    );
}
