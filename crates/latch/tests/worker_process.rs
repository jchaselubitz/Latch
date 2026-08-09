//! Process and lifetime.
//!
//! The worker owns the PTY and the child's process group, and it owns them
//! independently of whoever asked for the session. Two separate facts hide behind
//! the phrase "the CLI does not matter":
//!
//! - killing the launching process does not signal the child, which is about
//!   process-group membership; and
//! - hanging the launching process does not stall the child, which is about the
//!   worker's loop never waiting on it.
//!
//! A single test that closed the launcher and checked the child would pass with
//! either one of those broken, so they are pinned apart.
//!
//! The other rule this file exists for is structural: **no stored PID is ever
//! consulted for a kill.** It is asserted by making a stored PID as tempting as
//! possible — a plausible file, in the session directory, naming a live process —
//! and requiring that it changes nothing.

mod support;

use std::time::{Duration, Instant};

use latch::cli::manage::{self, StopRequest};
use latch::worker::meta;
use latch::worker::paths::{EXIT_NAME, JOURNAL_NAME, META_NAME, SOCKET_NAME};
use latch::worker::process::Signal;
use latch::worker::{self, socket};
use latch_protocol::control::SessionState;

use latch::worker::manifest::{TerminalSize, WorkerTuning};
use support::{dir_entries, is_alive, last_marker, sh_manifest, signal, Harness, BRIEF, SETTLE};

/// A child that announces its pid and then stays alive.
const ANNOUNCE_AND_WAIT: &str = "echo PID:$$; while :; do sleep 0.2; done";

#[test]
fn spawn_detached_starts_a_session_whose_socket_answers() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    assert!(session.is_live(), "a freshly spawned session must be live");
    assert_eq!(
        worker::derive_state(session.paths()).expect("state should derive"),
        SessionState::Running,
        "a session whose socket answers is running, and the answer is where that comes from"
    );
}

#[test]
fn the_child_leads_its_own_session_and_process_group() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );

    let output = client.wait_for_output("PID:", SETTLE);
    let pid = last_marker(&output, "PID:").expect("the child announces its pid") as i32;

    assert_eq!(
        support::group_of(pid),
        pid,
        "the child must lead its own process group, so a stop signals it and everything it started"
    );
    assert_eq!(
        support::session_of(pid),
        pid,
        "the child must lead its own session, so it has the pty as its controlling terminal \
         and no terminal that started it can hang it up"
    );
    assert_ne!(
        support::session_of(pid),
        support::session_of(std::process::id() as i32),
        "the child must not share a session with the process that asked for it"
    );
}

#[test]
fn killing_the_launching_process_does_not_signal_the_child() {
    let harness = Harness::new();
    let mut launcher = support::launch_via_shell(
        &harness,
        sh_manifest(ANNOUNCE_AND_WAIT, TerminalSize::new(200, 50)),
    );

    let paths = launcher.paths().clone();
    let mut client = support::Client::connect(&paths).expect("the socket should accept");
    client.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    let output = client.wait_for_output("PID:", SETTLE);
    let child_pid = last_marker(&output, "PID:").expect("the child announces its pid") as i32;

    launcher.kill_group();
    assert!(!launcher.is_alive(), "the launcher should be gone");

    // Given time for a signal to have propagated if one was going to.
    std::thread::sleep(BRIEF);

    assert!(
        is_alive(child_pid),
        "closing the window that started a session must not kill the process in it"
    );
    assert!(
        socket::is_live(&paths.socket()),
        "the worker must still be serving after its launcher died"
    );
    assert!(
        !paths.exit().exists(),
        "no exit record should exist for a session that never exited"
    );
}

#[test]
fn hanging_the_launching_process_does_not_stall_the_child() {
    let harness = Harness::new();
    let launcher = support::launch_via_shell(
        &harness,
        sh_manifest(
            "i=0; while :; do i=$((i+1)); echo TICK:$i; sleep 0.05; done",
            TerminalSize::new(200, 50),
        ),
    );
    let paths = launcher.paths().clone();

    // Stopped, not killed: the launcher is still there, still holding whatever
    // it holds, and never reading again. This is a phone whose app was
    // backgrounded mid-write, and it must not be able to freeze the agent.
    launcher.stop_group();

    let mut client = support::Client::connect(&paths).expect("the socket should accept");
    client.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );

    let first = client.output_for(Duration::from_millis(400));
    let early = last_marker(&first, "TICK:").expect("the child should be producing output");
    let later = client.output_for(Duration::from_millis(400));
    let late = last_marker(&later, "TICK:").expect("the child should still be producing output");

    assert!(
        late > early,
        "the child must keep running while the process that launched it is stopped \
         (saw {early} then {late})"
    );

    launcher.continue_group();
}

#[test]
fn child_exit_is_detected_and_recorded_with_its_code() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 7");

    session.wait_until_gone(SETTLE);

    let record = meta::read_exit(session.paths())
        .expect("the exit record should be readable")
        .expect("a session that exited has an exit record");
    assert_eq!(record.code, Some(7), "the child's status is recorded");
    assert_eq!(record.signal, None, "a normal exit names no signal");
    assert!(
        !record.exited_at.is_empty(),
        "the exit record is timestamped so `latch list` can sort by it"
    );

    assert_eq!(
        worker::derive_state(session.paths()).expect("state should derive"),
        SessionState::Exited,
        "exit.json present with the socket gone is what `exited` means"
    );
}

#[test]
fn child_death_by_signal_is_recorded_as_a_signal() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    let output = client.wait_for_output("PID:", SETTLE);
    let child_pid = last_marker(&output, "PID:").expect("the child announces its pid") as i32;

    signal(child_pid, libc::SIGTERM);
    session.wait_until_gone(SETTLE);

    let record = meta::read_exit(session.paths())
        .expect("the exit record should be readable")
        .expect("a session whose child died has an exit record");
    assert_eq!(
        record.signal.as_deref(),
        Some(Signal::Terminate.name()),
        "the signal is recorded by name, so a reader does not have to know that 15 means TERM"
    );
    assert_eq!(record.code, None, "a signalled child has no exit status");
}

#[test]
fn the_exit_record_exists_before_the_socket_disappears() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");

    // A client notices a session ended by its socket going away. If the record
    // is written after that, every clean exit has a window in which the session
    // reads as `lost` — which is the state reserved for a worker that died.
    //
    // "Gone" is the socket file being unlinked, not a connection being refused.
    // The two are not the same event: a live worker refuses connections while it
    // flushes its clients, and on Darwin it refuses them as soon as a caller
    // probing in a loop fills its accept backlog. Polling `connect` here would
    // assert the ordering of a moment the worker never claimed anything about.
    let deadline = Instant::now() + SETTLE;
    loop {
        if !session.paths().socket().exists() {
            assert!(
                session.paths().exit().exists(),
                "exit.json must be in place before the socket is removed"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the session should have exited within {SETTLE:?}"
        );
    }
}

/// A child writing as fast as the worker reads must not lock the worker out of
/// its own control plane.
///
/// `while :; do echo; done` keeps the pty permanently readable, and a drain that
/// runs until `WouldBlock` never returns to the part of the loop that accepts
/// connections and reads control frames. The failure is quiet and self-sealing:
/// the session answers nothing, so `latch list` calls a running session `lost`
/// and `prune` offers to reclaim it, while the one command that would end the
/// flood is the one the flood prevents.
///
/// Stopping is asserted through to `exited` rather than to a dead socket,
/// because a worker that dies partway through its own shutdown also leaves a
/// dead socket — and leaves no record of how the session ended.
///
#[test]
fn a_flooding_child_leaves_the_session_answerable_and_stoppable() {
    let harness = Harness::new();
    // The grace is shortened because this test is about the session ending and
    // saying how, not about which of the two signals ended it. A child buried in
    // its own output does not always take the graceful one promptly, and waiting
    // out the default five seconds to find that out proves nothing extra.
    let session = harness.spawn(support::tuned(
        sh_manifest("while :; do echo spam; done", TerminalSize::new(200, 50)),
        WorkerTuning {
            stop_grace_ms: 500,
            ..WorkerTuning::default()
        },
    ));

    // Long enough that the flood is the steady state rather than a burst.
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        session.is_live(),
        "a worker draining a flooding pty must still answer its socket"
    );
    assert_eq!(
        worker::derive_state(session.paths()).expect("state should derive"),
        SessionState::Running,
        "a flood is not a death"
    );

    session.request_stop();

    // Waited on the socket file rather than by probing the socket. Probing opens
    // a connection per attempt, and every connection is one more client this
    // worker services on a pass it is already spending on the flood — a poll
    // loop fast enough to be useful is fast enough to bury the loop it is
    // waiting for. The file going away is the same event, observed for free.
    assert!(
        support::settled(SETTLE, || !session.paths().socket().exists()),
        "a flooding session must still end when asked"
    );
    assert_eq!(
        worker::derive_state(session.paths()).expect("state should derive"),
        SessionState::Exited,
        "a stopped session records how it ended; `lost` would mean the worker \
         died before writing exit.json"
    );
}

#[test]
fn a_graceful_stop_escalates_to_a_forced_stop() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(
            "trap '' TERM; echo PID:$$; while :; do sleep 0.2; done",
            TerminalSize::new(200, 50),
        ),
        WorkerTuning {
            stop_grace_ms: 500,
            ..WorkerTuning::default()
        },
    ));

    // The child installs its trap and *then* announces itself, so waiting for the
    // announcement is what makes "a child that ignores the graceful signal" true
    // before the stop is sent. Without it this races `/bin/sh` startup: a TERM
    // that arrives first kills a shell that has not reached `trap` yet, and the
    // test fails claiming the grace interval was skipped.
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    client.wait_for_output("PID:", SETTLE);
    drop(client);

    let started = Instant::now();
    session.request_stop();
    session.wait_until_gone(Duration::from_secs(30));
    let took = started.elapsed();

    assert!(
        took >= Duration::from_millis(400),
        "a stop must give the child its grace interval before forcing it (took {took:?})"
    );

    let record = meta::read_exit(session.paths())
        .expect("the exit record should be readable")
        .expect("a stopped session has an exit record");
    assert_eq!(
        record.signal.as_deref(),
        Some(Signal::Kill.name()),
        "a child that ignores the graceful signal must still be stopped; \
         a stop that can be refused is a session that cannot be stopped"
    );
}

#[test]
fn force_stop_skips_the_grace_interval() {
    let harness = Harness::new();
    let session = harness.spawn_sh("trap '' TERM; echo READY; while :; do sleep 0.2; done");
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    client.wait_for_output("READY", SETTLE);
    drop(client);

    let started = Instant::now();
    let report = manage::stop(StopRequest {
        home: harness.home().clone(),
        session: session.id().to_string(),
        force: true,
    })
    .expect("force stop should reach the worker");
    session.wait_until_gone(SETTLE);

    assert_eq!(report.state, "exited");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "--force must not wait out the default five-second graceful interval"
    );
    let record = meta::read_exit(session.paths())
        .expect("the exit record should be readable")
        .expect("force stop records the child exit");
    assert_eq!(record.signal.as_deref(), Some(Signal::Kill.name()));
}

#[test]
fn a_stop_ignores_a_stale_pid_left_in_the_session_directory() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let mut client = session.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    let output = client.wait_for_output("PID:", SETTLE);
    let child_pid = last_marker(&output, "PID:").expect("the child announces its pid") as i32;

    // A live bystander, and every plausible name a PID file might have. If any
    // code path anywhere reads a PID from the session directory and signals it,
    // this process dies and the test says so.
    let mut decoy = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .spawn()
        .expect("the decoy should start");
    let decoy_pid = decoy.id() as i32;
    for name in ["pid", "worker.pid", "child.pid", "session.pid"] {
        std::fs::write(session.paths().dir().join(name), format!("{decoy_pid}\n"))
            .expect("the decoy pid file should be written");
    }
    drop(client);

    session.request_stop();
    session.wait_until_gone(Duration::from_secs(30));

    assert!(
        is_alive(decoy_pid),
        "a stop must signal the worker's own child's process group and nothing else; \
         a pid read from disk names whatever the operating system has since reused"
    );
    assert!(
        !is_alive(child_pid),
        "the session's actual child must be gone"
    );

    let _ = decoy.kill();
    let _ = decoy.wait();
}

#[test]
fn a_live_session_directory_contains_no_pid_at_all() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let entries = dir_entries(session.paths().dir());
    let mut expected = vec![
        SOCKET_NAME.to_owned(),
        JOURNAL_NAME.to_owned(),
        META_NAME.to_owned(),
    ];
    expected.sort();
    assert_eq!(
        entries, expected,
        "a session directory holds a socket, a journal, and metadata — no registry, \
         no status file, and nothing a kill could be read out of"
    );

    let meta_text = std::fs::read_to_string(session.paths().meta()).expect("meta.json is readable");
    assert!(
        !meta_text.contains("\"pid\""),
        "meta.json must not record a pid: {meta_text}"
    );

    session.request_stop();
    session.wait_until_gone(Duration::from_secs(30));

    let after = dir_entries(session.paths().dir());
    assert!(
        after.contains(&EXIT_NAME.to_owned()) && !after.contains(&SOCKET_NAME.to_owned()),
        "an exited session keeps its record and loses its socket, and gains nothing else: {after:?}"
    );
}

#[test]
fn stopping_one_session_leaves_another_running() {
    let harness = Harness::new();
    let doomed = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let spared = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut spared_client = spared.attach(
        latch_protocol::control::AttachMode::Watch,
        false,
        latch_protocol::control::Size { cols: 80, rows: 24 },
    );
    // Read by marker, not by "the first digits in the output". An attach is
    // answered with the serialized screen, whose leading mode resets contain
    // digits of their own — `\x1b[?1049l` parses as 1049 to anything that just
    // looks for a number.
    let spared_pid = last_marker(&spared_client.wait_for_output("PID:", SETTLE), "PID:")
        .expect("the spared child announces its pid") as i32;

    doomed.request_stop();
    doomed.wait_until_gone(SETTLE);

    assert!(
        spared.is_live(),
        "stopping one session must not touch another"
    );
    assert!(
        is_alive(spared_pid),
        "the spared child must still be running"
    );
}

#[test]
fn a_leftover_socket_with_nobody_behind_it_is_a_lost_session() {
    let harness = Harness::new();
    let paths = harness.home().session(&harness.next_session_id());
    paths
        .ensure()
        .expect("the session directory should be created");

    // A socket file with nobody behind it is exactly what a `SIGKILL`ed worker
    // leaves. It must not read as alive, and without an exit record it is not
    // `exited` either — it is `lost`, which is a distinct thing a user can act on.
    std::fs::write(paths.socket(), b"not a socket").expect("a decoy socket should be writable");

    assert!(
        !socket::is_live(&paths.socket()),
        "liveness is the socket answering, never the file existing"
    );
    assert_eq!(
        worker::derive_state(&paths).expect("state should derive"),
        SessionState::Lost,
        "no answering socket and no exit record is a lost session"
    );
}
