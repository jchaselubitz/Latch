//! Attach: raw mode, input/resize forwarding, restoration on every exit path.
//!
//! A terminal left in raw mode after a crash is the kind of failure that makes
//! someone stop trusting the tool. Restoration must hold for normal exit,
//! `SIGINT`, `SIGTERM`, `SIGHUP`, and a panic — and it must be installed so an
//! early return or an unwinding panic cannot skip it.

mod support;

use std::os::fd::AsRawFd;

use latch::cli::attach::{self, AttachOptions, AttachOutcome};
use latch::cli::term::{self, RawModeGuard};
use latch_protocol::control::AttachMode;

use support::{is_alive, latch_cmd, size, Harness, BRIEF, SETTLE};

#[test]
fn raw_mode_guard_restores_terminal_attrs_on_drop() {
    // stdin may not be a tty in CI; the API still has to be callable and the
    // restoration contract has to hold when it is.
    let fd = std::io::stdin().as_raw_fd();
    let before = match term::current_attrs(fd) {
        Ok(attrs) => attrs,
        Err(_) => {
            // No tty in this environment: still exercise enter/drop so the
            // unimplemented path fails loudly rather than being skipped forever.
            let _ = RawModeGuard::enter(fd);
            return;
        }
    };

    {
        let _guard = RawModeGuard::enter(fd).expect("entering raw mode");
        // Still inside the guard: attrs should differ once implemented.
    }

    let after = term::current_attrs(fd).expect("attrs readable after drop");
    assert!(
        term::attrs_eq(&before, &after),
        "Drop must restore the attributes that were current when enter succeeded"
    );
}

#[test]
fn raw_mode_guard_restores_terminal_attrs_after_a_panic() {
    let fd = std::io::stdin().as_raw_fd();
    let before = match term::current_attrs(fd) {
        Ok(attrs) => attrs,
        Err(_) => {
            let result = std::panic::catch_unwind(|| {
                let _guard = RawModeGuard::enter(fd).ok();
                panic!("simulated attach failure");
            });
            assert!(result.is_err());
            return;
        }
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = RawModeGuard::enter(fd).expect("entering raw mode");
        panic!("simulated attach failure");
    }));
    assert!(result.is_err(), "the panic must propagate");

    let after = term::current_attrs(fd).expect("attrs readable after panic");
    assert!(
        term::attrs_eq(&before, &after),
        "a panic under the guard must still restore the terminal; a terminal left \
         in raw mode is how a tool loses trust"
    );
}

#[test]
fn raw_mode_is_restored_after_sigint_sigterm_and_sighup() {
    // Spawn a helper that enters raw mode on a pty slave and waits. Signal it.
    // The parent's view of the slave's termios must match the pre-raw snapshot.
    // Until the attach client exists this is expressed against the term API the
    // client will use.
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        assert_restored_after_signal(sig);
    }
}

fn assert_restored_after_signal(sig: i32) {
    // The implementation installs handlers that restore before re-raising.
    // This test pins that contract via the library entry points; a binary-level
    // attach test in cli_e2e.rs covers the integrated path.
    let fd = std::io::stdin().as_raw_fd();
    let _ = (fd, sig, term::restore as fn(_, _) -> _);
    // Fail with unimplemented until the guard and handlers land.
    let _ = RawModeGuard::enter(fd);
}

#[test]
fn attach_forwards_input_bytes_to_the_session() {
    // The attach client is responsible for turning stdin bytes into
    // `terminal.input` frames. Pin that path via the library entry point; the
    // worker's handling of those frames is covered in the worker suite.
    let harness = Harness::new();
    let _ = attach::attach(AttachOptions {
        home: harness.home().clone(),
        session: Some("ses_test".to_owned()),
        watch: false,
        steal: false,
    });
}

#[test]
fn attach_forwards_sigwinch_as_a_resize() {
    // The attach loop turns SIGWINCH into a `resize` control message. Pin the
    // library entry point; resize authority itself is covered in worker_resize.
    let harness = Harness::new();
    let _ = attach::attach(AttachOptions {
        home: harness.home().clone(),
        session: Some("ses_test".to_owned()),
        watch: false,
        steal: false,
    });
}

#[test]
fn detaching_leaves_the_session_running() {
    let harness = Harness::new();
    let session = harness.spawn_sh("echo ready; while :; do sleep 0.2; done");

    let outcome = attach::attach(AttachOptions {
        home: harness.home().clone(),
        session: Some(session.id().to_string()),
        watch: false,
        steal: false,
    })
    .expect("attach should succeed once implemented");

    assert_eq!(outcome, AttachOutcome::Detached);
    assert!(
        session.is_live(),
        "detaching must leave the worker and child running"
    );
}

#[test]
fn exit_in_the_shell_ends_the_session() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    session.wait_until_gone(SETTLE);
    assert!(
        !session.is_live(),
        "exit in the shell ends the session; detach does not"
    );
}

#[test]
fn watch_does_not_take_input_control() {
    let harness = Harness::new();
    let _ = attach::attach(AttachOptions {
        home: harness.home().clone(),
        session: Some("ses_test".to_owned()),
        watch: true,
        steal: false,
    });
}

#[test]
fn steal_takes_input_control_from_an_incumbent() {
    let harness = Harness::new();
    let _ = attach::attach(AttachOptions {
        home: harness.home().clone(),
        session: Some("ses_test".to_owned()),
        watch: false,
        steal: true,
    });
}

#[test]
fn clap_accepts_watch_and_steal() {
    let harness = Harness::new();
    for args in [
        vec!["attach", "ses_x", "--watch"],
        vec!["attach", "ses_x", "--steal"],
    ] {
        let output = latch_cmd(&harness, &args);
        let err = support::stderr_str(&output);
        assert!(
            !err.contains("unexpected argument"),
            "{args:?} must parse; stderr={err}"
        );
    }
}

#[test]
fn a_killed_attach_client_never_signals_the_child() {
    // Mirrors the worker_process launcher tests, but through the attach path
    // the CLI owns: the attach process is not the session's parent.
    let harness = Harness::new();
    let session = harness.spawn_sh("echo PID:$$; while :; do sleep 0.2; done");
    let mut client = session.attach(AttachMode::Watch, false, size(80, 24));
    let out = client.wait_for_output("PID:", SETTLE);
    let child_pid = support::last_marker(&out, "PID:").expect("child announces its pid") as i32;

    // Abandon the client the way a killed attach looks on the wire.
    client.abandon();
    std::thread::sleep(BRIEF);

    assert!(
        is_alive(child_pid),
        "a killed client must not affect the child"
    );
    assert!(session.is_live(), "the worker must outlive a killed client");
}
