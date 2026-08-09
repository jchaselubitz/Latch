//! What a session leaves behind, and how long it leaves it there.
//!
//! This is the second half of the plan's open item 2, and it is a phone
//! question before it is a housekeeping one. A command finishes while you are
//! away from your desk; the thing you want is the screen it finished on. A
//! session that becomes unreadable the instant its child exits answers that
//! with nothing, and one that is never reclaimed answers it by filling a disk.
//!
//! The decision and its reasoning are in `docs/DECISION_SCROLLBACK.md`. What is
//! asserted here:
//!
//! * An exited session leaves the same snapshot bytes a live worker would have
//!   sent — not a journal to be replayed, which is the byte-tail reconstruction
//!   the screen model exists to avoid.
//! * Attaching to one shows that screen and says how it ended, read-only, and
//!   `--retry` does not treat it as a link to wait for.
//! * `prune` keeps it for the retention window and says so; `--all` overrides.
//! * A `lost` session — nothing to show — is reclaimed on sight.

mod support;

use std::time::Duration;

use latch::cli::json::PruneReport;
use latch::worker::paths::FILE_MODE;
use latch_protocol::control::SessionState;

use support::{latch_cmd, mode_of, stdout_json, Harness, PtyProcess, BRIEF, SETTLE};

/// Waits for a spawned session to reach `exited`, which is what makes its
/// directory eligible for everything below.
fn wait_for_exit(session: &support::Session) {
    let deadline = std::time::Instant::now() + SETTLE;
    while std::time::Instant::now() < deadline {
        if matches!(
            latch::worker::derive_state(session.paths()),
            Ok(SessionState::Exited)
        ) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the session never reached `exited`");
}

/// Backdates a session's exit record, which is how the far side of a
/// twenty-four hour window is reachable in a test that takes a second.
///
/// The record's own timestamp is what `prune` reads, so this is not a fixture
/// hack around the implementation — it is the same field a session that really
/// exited yesterday would carry.
fn backdate_exit(session: &support::Session, ago: Duration) {
    let path = session.paths().exit();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("the exit record exists"))
            .expect("the exit record is json");
    let then = std::time::SystemTime::now() - ago;
    let seconds = then
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    // A minimal RFC 3339 stamp, built the same way the worker's own is.
    let days = seconds / 86_400;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    record["exited_at"] = serde_json::Value::String(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    ));
    std::fs::write(&path, serde_json::to_vec(&record).expect("serializable"))
        .expect("the exit record is writable");
}

/// Days since the epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// --- The artifact -------------------------------------------------------------

/// The final screen is a snapshot, written once, owner-only.
///
/// Owner-only for the same reason the journal is: it holds terminal output, and
/// on a shared machine that is somebody's work.
#[test]
fn an_exited_session_leaves_its_final_screen_as_a_snapshot() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'THE LAST THING IT PRINTED\\n'; exit 3");
    wait_for_exit(&session);

    let screen = std::fs::read(session.paths().screen()).expect("the final screen is on disk");
    assert!(
        screen.starts_with(b"\x1bc"),
        "the artifact is a snapshot — it opens with the same hard reset every \
         snapshot does, so it reconstructs from a terminal in any state"
    );
    assert!(
        String::from_utf8_lossy(&screen).contains("THE LAST THING IT PRINTED"),
        "and it carries the screen the child ended on"
    );
    assert_eq!(
        mode_of(&session.paths().screen()),
        FILE_MODE,
        "the final screen is 0600: it holds terminal output"
    );
}

/// A session whose worker never got to write one is still reportable. The exit
/// record is what state is derived from; the screen is a courtesy, and a
/// courtesy that becomes a precondition is a new way to fail.
#[test]
fn a_session_without_a_final_screen_still_reports_how_it_ended() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    wait_for_exit(&session);
    std::fs::remove_file(session.paths().screen()).expect("removable");

    let mut client = PtyProcess::spawn(&harness, &["attach", &session.id().to_string()], 80, 24);
    client.wait_for_screen("exited cleanly", SETTLE);
    let status = client.wait(SETTLE);
    assert!(status.success());
}

// --- Attaching to one ---------------------------------------------------------

/// The phone case: something finished while you were away, and its last screen
/// is the reason you picked up the phone.
#[test]
fn attaching_to_an_exited_session_shows_its_last_screen_and_how_it_ended() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'BUILD FAILED: 3 errors\\n'; exit 3");
    wait_for_exit(&session);

    let mut client = PtyProcess::spawn(&harness, &["attach", &session.id().to_string()], 80, 24);
    client.wait_for_screen("BUILD FAILED: 3 errors", SETTLE);
    client.wait_for_screen("exited with status 3", SETTLE);
    let status = client.wait(SETTLE);
    assert!(
        status.success(),
        "reading a finished session is a success, not an error: it is what the \
         session is for once it has ended"
    );
    assert!(
        client.transcript().contains("latch prune"),
        "and it says what makes the screen go away, since nothing else will: \
         {:?}",
        client.transcript()
    );
}

/// `--retry` exists for a link that might come back. A session that has ended
/// is not one, and a client that waited out its backoff against one would be
/// waiting for something that cannot happen.
#[test]
fn retry_does_not_wait_for_an_exited_session_to_come_back() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'DONE\\n'; exit 0");
    wait_for_exit(&session);

    let started = std::time::Instant::now();
    let mut client = PtyProcess::spawn(
        &harness,
        &["attach", &session.id().to_string(), "--retry"],
        80,
        24,
    );
    let status = client.wait(SETTLE);
    let elapsed = started.elapsed();

    assert!(status.success());
    assert!(
        elapsed < latch::cli::attach::RetryPolicy::default().budget(),
        "attaching to an exited session spent {elapsed:?} — it retried a \
         session that is never coming back"
    );
    assert!(client.transcript().contains("DONE"));
}

/// Once reclaimed, the session is gone and says so plainly. This is the other
/// half of the retention decision: the window ends, and what it ends is this.
#[test]
fn a_reclaimed_session_is_gone_rather_than_silently_empty() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'GONE SOON\\n'; exit 0");
    wait_for_exit(&session);
    let id = session.id().to_string();

    let pruned = latch_cmd(&harness, &["prune", "--all"]);
    assert!(pruned.status.success());

    let attached = latch_cmd(&harness, &["attach", &id]);
    assert!(!attached.status.success());
    assert!(
        support::stderr_str(&attached).contains(&id)
            || support::stderr_str(&attached).contains("session"),
        "stderr={}",
        support::stderr_str(&attached)
    );
}

// --- The retention window -----------------------------------------------------

/// A session that has just exited is kept, and `prune` names it rather than
/// silently skipping it — a skipped session is otherwise indistinguishable from
/// a `prune` that does not work.
#[test]
fn prune_keeps_a_recently_exited_session_and_says_which() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    wait_for_exit(&session);
    let id = session.id().to_string();

    let output = latch_cmd(&harness, &["prune", "--json"]);
    let report: PruneReport =
        serde_json::from_value(stdout_json(&output)).expect("prune emits a PruneReport");
    assert!(
        report.reclaimed.is_empty(),
        "a session that exited a moment ago is still worth reading"
    );
    assert_eq!(
        report
            .retained
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec![id.as_str()]
    );
    assert!(
        report.retained[0].reclaimable_in_ms > 0,
        "and the report says how long it has left"
    );
    assert!(
        session.paths().dir().exists(),
        "a retained session keeps its directory"
    );
}

/// The far side of the window.
#[test]
fn prune_reclaims_a_session_that_has_aged_out_of_the_window() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    wait_for_exit(&session);
    backdate_exit(&session, Duration::from_secs(48 * 60 * 60));

    let output = latch_cmd(&harness, &["prune", "--json"]);
    let report: PruneReport =
        serde_json::from_value(stdout_json(&output)).expect("prune emits a PruneReport");
    assert_eq!(
        report.reclaimed,
        vec![session.id().to_string()],
        "a session that exited two days ago is past the window"
    );
    assert!(report.retained.is_empty());
    assert!(!session.paths().dir().exists());
}

/// `--all` is the escape hatch, and it must actually reclaim rather than
/// reporting that it would.
#[test]
fn prune_all_reclaims_inside_the_window() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    wait_for_exit(&session);

    let output = latch_cmd(&harness, &["prune", "--all", "--json"]);
    let report: PruneReport =
        serde_json::from_value(stdout_json(&output)).expect("prune emits a PruneReport");
    assert_eq!(report.reclaimed, vec![session.id().to_string()]);
    assert!(!session.paths().dir().exists());
}

/// A dry run inside the window reclaims nothing and removes nothing, and a dry
/// run past it names what it would take without taking it.
#[test]
fn a_dry_run_never_removes_a_directory() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    wait_for_exit(&session);
    backdate_exit(&session, Duration::from_secs(48 * 60 * 60));

    let output = latch_cmd(&harness, &["prune", "--dry-run", "--json"]);
    let report: PruneReport =
        serde_json::from_value(stdout_json(&output)).expect("prune emits a PruneReport");
    assert!(report.dry_run);
    assert_eq!(report.reclaimed, vec![session.id().to_string()]);
    assert!(
        session.paths().dir().exists(),
        "a dry run says what it would do and does none of it"
    );
}

/// A `lost` session — no socket and no exit record — has no screen to offer and
/// no exit to report, so the window does not apply to it. Retaining it would
/// keep a directory nobody can learn anything from.
#[test]
fn a_lost_session_is_reclaimed_without_waiting_out_the_window() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while :; do sleep 0.2; done");
    // Removing the socket without an exit record is exactly the state a worker
    // killed with SIGKILL leaves behind.
    session.request_stop();
    wait_for_exit(&session);
    std::fs::remove_file(session.paths().exit()).expect("removable");
    let _ = std::fs::remove_file(session.paths().socket());
    assert!(matches!(
        latch::worker::derive_state(session.paths()),
        Ok(SessionState::Lost)
    ));

    let output = latch_cmd(&harness, &["prune", "--json"]);
    let report: PruneReport =
        serde_json::from_value(stdout_json(&output)).expect("prune emits a PruneReport");
    assert_eq!(report.reclaimed, vec![session.id().to_string()]);
    assert!(report.retained.is_empty());

    std::thread::sleep(BRIEF);
}
