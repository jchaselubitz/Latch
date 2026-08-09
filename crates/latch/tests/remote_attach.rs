//! M2 remote attach resilience: the phone-shaped contract.
//!
//! SSH is deliberately outside Latch's transport boundary.  These tests use
//! the Unix socket directly, then make that byte stream behave like a phone
//! whose network changes, whose app backgrounds, or whose cellular link cannot
//! drain output.  The worker must treat every reconnect as an ordinary attach:
//! there is no resume protocol and no client-side replay bookkeeping to hide a
//! broken snapshot path.

mod support;

use std::time::Duration;

use latch_protocol::control::{AttachMode, ControlMessage};
use latch_protocol::frame::{self, FrameType};

use latch::cli::attach::RetryPolicy;
use latch::worker::manifest::{TerminalSize, WorkerTuning};
use support::{latch_cmd, sh_manifest, size, Harness, PtyProcess, SocketProxy, BRIEF, SETTLE};

const DESK: (u16, u16) = (200, 50);
const PHONE: (u16, u16) = (40, 20);

/// The worker must survive a network drop before the first complete frame.
///
/// This is intentionally lower-level than `Client::attach`: a client that dies
/// halfway through its handshake has not sent an attach request at all.  A
/// worker that blocks its accept loop on those bytes would make a transient SSH
/// tunnel failure take the whole session offline.
#[test]
fn a_drop_mid_handshake_does_not_wedge_the_session_and_a_fresh_attach_gets_the_screen() {
    let harness = Harness::new();
    let session =
        harness.spawn_sh("printf 'PHONE-REATTACH-SCREEN\\n'; while :; do sleep 0.2; done");

    let attach = latch_protocol::control::ControlMessage::Attach {
        protocol: latch_protocol::PROTOCOL_VERSION,
        mode: AttachMode::Watch,
        steal: false,
        client: latch_protocol::control::ClientInfo {
            kind: "ios".to_owned(),
            name: "termius".to_owned(),
        },
        size: size(PHONE.0, PHONE.1),
    };
    let encoded = frame::encode(
        FrameType::Control,
        &latch_protocol::control::encode(&attach),
    )
    .expect("the test attach frame is valid");
    let mut broken = session
        .connect()
        .expect("the socket accepts the broken link");
    broken
        .send_raw(&encoded[..3])
        .expect("the transport accepts a partial write");
    broken.abandon();

    assert!(
        session.is_live(),
        "a partial handshake must not take down the session"
    );
    let mut reattached = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    reattached.wait_for_output("PHONE-REATTACH-SCREEN", SETTLE);
}

/// A snapshot is output, not a special replay transaction.  Losing the socket
/// while it is in flight must therefore be indistinguishable from losing it
/// during live output: the next attach begins from the current screen.
#[test]
fn a_drop_mid_snapshot_is_followed_by_an_exactly_usable_fresh_snapshot() {
    let harness = Harness::new();
    let session = harness.spawn_sh(
        "printf '\\033[?1049hPHONE-ALT-SCREEN\\033[?25l\\n'; while :; do sleep 0.2; done",
    );

    let mut first = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    // Consume acceptance but deliberately do not drain the snapshot.  The drop
    // is between its frames on a constrained link, not a clean detach.
    let _ = first.next_control(SETTLE).expect("attach is acknowledged");
    first.abandon();

    let mut second = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    let snapshot = second.wait_for_output("PHONE-ALT-SCREEN", SETTLE);
    let text = String::from_utf8_lossy(&snapshot);
    assert!(
        text.contains("\u{1b}[?1049h"),
        "reattach restores alternate-screen state"
    );
    assert!(
        session.is_live(),
        "a dropped snapshot leaves the worker serving"
    );
}

/// Backgrounding is steady state on iOS.  Repeating it must neither leak an
/// attachment nor promote a watcher to controller, even while an alternate
/// screen application continues to write.
#[test]
fn rapid_phone_background_cycles_preserve_the_desk_controller_and_the_current_screen() {
    let harness = Harness::new();
    let session = harness.spawn_sh(
        "printf '\\033[?1049hALT-LOOP\\n'; i=0; while :; do i=$((i+1)); printf 'FLOW:%d\\n' \"$i\"; sleep 0.03; done",
    );
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("ALT-LOOP", SETTLE);

    for _ in 0..12 {
        let mut phone = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
        let output = phone.wait_for_output("ALT-LOOP", SETTLE);
        assert!(
            String::from_utf8_lossy(&output).contains("\u{1b}[?1049h"),
            "every phone reattach starts from the alternate screen rather than a byte tail"
        );
        phone.abandon();
    }

    desk.send_input(b"still-the-desk\n")
        .expect("the incumbent controller remains connected");
    let mut challenger = session.connect().expect("the worker still accepts peers");
    assert!(
        matches!(
            challenger.attach(AttachMode::Control, false, size(PHONE.0, PHONE.1)),
            ControlMessage::Error { ref code, .. } if code == "control_busy"
        ),
        "watch reattaches must never leak or steal the desk controller"
    );
    assert!(
        session.is_live(),
        "rapid watcher departures must not leak control state or harm the session"
    );
}

/// The D4 loop must work for a dropped phone connection, not just an orderly
/// close.  The child observes actual PTY geometry, so this catches a registry
/// update that was never applied to the terminal.
#[test]
fn a_dropped_phone_controller_reverts_the_live_pty_to_the_desk_geometry() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.05; done",
        TerminalSize::new(DESK.0, DESK.1),
    ));
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("50 200", SETTLE);

    let phone = session.attach(AttachMode::Control, true, size(PHONE.0, PHONE.1));
    desk.wait_for_output("20 40", SETTLE);
    phone.abandon();

    desk.wait_for_output("50 200", SETTLE);
}

/// A throttled phone is allowed to miss intermediate frames, but never to make
/// the PTY stop.  When it eventually reads, the snapshot must place it near the
/// desk client's current output rather than feeding an unbounded backlog.
#[test]
fn a_slow_phone_is_resynchronized_without_stalling_the_pty() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(
            "PAD=$(printf 'x%.0s' $(seq 1 3900)); i=0; while :; do i=$((i+1)); printf 'CELL:%d %s\\n' \"$i\" \"$PAD\"; sleep 0.01; done",
            TerminalSize::new(DESK.0, DESK.1),
        ),
        WorkerTuning {
            queue_capacity_bytes: 16 * 1024,
            queue_capacity_chunks: 32,
            ..WorkerTuning::default()
        },
    ));
    let mut phone = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    let mut desk = session.attach(AttachMode::Watch, false, size(DESK.0, DESK.1));

    std::thread::sleep(Duration::from_millis(900));
    let before = support::last_marker(&desk.output_for(Duration::from_millis(250)), "CELL:")
        .expect("the healthy desk link keeps receiving output");
    let caught_up = support::last_marker(&phone.output_for(Duration::from_millis(1200)), "CELL:")
        .expect("the slow phone is resynchronized with a snapshot");
    assert!(
        caught_up >= before,
        "the slow phone catches up instead of replaying its backlog"
    );
    assert!(
        session.is_live(),
        "the PTY remains live while the phone is stalled"
    );
}

/// The screen a phone is sent must be the screen a phone has.
///
/// Taking control at 40 columns reflows a 200-column session, so the screen
/// serialized for the new controller has to be the reflowed one.  When it is
/// not, the phone paints 200 columns of content into 40 and recovers only if
/// the child happens to repaint on `SIGWINCH` — which a shell does not, and
/// which is why "reattach restores the screen correctly, every time" is the M2
/// criterion rather than "usually".
///
/// Asserted on width directly.  The child paints a run far wider than a phone
/// and narrower than the desk, so the width of the widest line in what the
/// phone receives says unambiguously which geometry the screen was serialized
/// at — and, being a full row, that a screen arrived at all.
#[test]
fn a_phone_that_takes_control_is_sent_its_own_geometry_not_the_desks() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "printf '\\033[?1049h\\033[H\\033[2J'; printf 'W%.0s' $(seq 1 150); \
         while :; do sleep 0.2; done",
        TerminalSize::new(DESK.0, DESK.1),
    ));
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    assert_eq!(
        support::longest_run(&desk.wait_for_output("WWWW", SETTLE), 'W'),
        150,
        "the desk client sees all 150 columns its own geometry can hold"
    );

    let mut phone = session.attach(AttachMode::Control, true, size(PHONE.0, PHONE.1));
    let taking_control = phone.wait_for_output("WWWW", SETTLE);

    assert_eq!(
        support::longest_run(&taking_control, 'W'),
        u32::from(PHONE.0),
        "the phone must be sent the reflowed screen, not {} columns of the \
         geometry it displaced painted into {}",
        DESK.0,
        PHONE.0
    );
}

/// The bound a phone actually feels.
///
/// On iOS every app-backgrounding is a reattach, so the history that
/// accompanies a snapshot is a cost paid on every glance at the phone rather
/// than once per session.  The policy — and the measurements it was chosen from
/// — are in `docs/DECISION_SCROLLBACK.md`; what is asserted here is that the
/// worker honors the configured ceiling against a session that has far more
/// history than the ceiling allows.
///
/// The screen itself is deliberately outside the bound.  It is not optional and
/// its size is set by the session's geometry, so folding it into the history
/// budget would mean either a policy no geometry could satisfy or a screen sent
/// in pieces.  The two are separated on the wire by the alternate-screen
/// statement, which is the first thing every snapshot emits after its history.
#[test]
fn scrollback_sent_on_attach_never_exceeds_the_configured_history_bound() {
    const HISTORY_BYTES: usize = 4 * 1024;
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(
            "i=0; while [ $i -lt 400 ]; do printf 'HISTORY:%04d %0180d\\n' \"$i\" 0; \
             i=$((i+1)); done; printf 'FILLED\\n'; while :; do sleep 0.2; done",
            TerminalSize::new(DESK.0, DESK.1),
        ),
        WorkerTuning {
            attach_history_lines: 10_000,
            attach_history_bytes: HISTORY_BYTES,
            ..WorkerTuning::default()
        },
    ));

    // Waited for rather than slept through: under load a sleep can return
    // before the child has produced any history at all, which would let this
    // pass by measuring a session that has nothing to send.
    let mut warmup = session.attach(AttachMode::Watch, false, size(DESK.0, DESK.1));
    warmup.wait_for_output("FILLED", SETTLE);
    warmup.abandon();

    let mut phone = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    let snapshot = phone.next_frame(SETTLE).expect("attach sends a snapshot");
    assert_eq!(snapshot.frame_type, FrameType::TerminalOutput);

    let history = support::attach_history(&snapshot.payload);
    assert!(
        !history.is_empty(),
        "a session with 400 lines of scrollback sent none of it; the bound is \
         only meaningful if history is sent at all"
    );
    assert!(
        history.len() <= HISTORY_BYTES,
        "the history preamble was {} bytes against a {HISTORY_BYTES} byte policy",
        history.len()
    );
    assert!(
        String::from_utf8_lossy(history).contains("HISTORY:"),
        "the preamble carries the session's own scrolled-off output"
    );
}

/// History is what a client is sent when it *arrives*.  A client that fell
/// behind and had its queue dropped is resynchronized with the screen alone.
///
/// This is the difference between a bound and a leak.  The resync path exists
/// because a link is too slow to keep up; re-sending scrollback down it every
/// time it fails would turn the mechanism that protects a slow link into the
/// thing that saturates it.
#[test]
fn a_backpressure_resync_carries_the_screen_and_no_history() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(
            "i=0; while [ $i -lt 400 ]; do printf 'HISTORY:%04d %0180d\\n' \"$i\" 0; \
             i=$((i+1)); done; printf 'FILLED\\n'; \
             PAD=$(printf 'x%.0s' $(seq 1 3900)); \
             while :; do i=$((i+1)); printf 'CELL:%d %s\\n' \"$i\" \"$PAD\"; sleep 0.01; done",
            TerminalSize::new(DESK.0, DESK.1),
        ),
        WorkerTuning {
            queue_capacity_bytes: 16 * 1024,
            queue_capacity_chunks: 32,
            ..WorkerTuning::default()
        },
    ));

    // The history has to exist before the measured client attaches, or this
    // test passes by observing a session that had nothing to send.
    let mut warmup = session.attach(AttachMode::Watch, false, size(DESK.0, DESK.1));
    warmup.wait_for_output("FILLED", SETTLE);
    warmup.abandon();

    let mut phone = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    let first = phone.next_frame(SETTLE).expect("attach sends a snapshot");
    assert!(
        !support::attach_history(&first.payload).is_empty(),
        "the attach itself does carry history, or this test proves nothing"
    );

    // Stall long enough to overflow the queue, then drain and find the resync.
    std::thread::sleep(Duration::from_millis(900));
    let resynced = phone.output_for(Duration::from_millis(1200));
    let resets: Vec<usize> = resynced
        .windows(2)
        .enumerate()
        .filter(|(_, window)| *window == *b"\x1bc")
        .map(|(at, _)| at)
        .collect();
    assert!(
        !resets.is_empty(),
        "the stalled client was resynchronized with a fresh snapshot"
    );
    for at in resets {
        let history = support::attach_history(&resynced[at..]);
        assert!(
            history.is_empty(),
            "a resync carried {} bytes of history; only an attach may",
            history.len()
        );
    }
}

/// `--watch` and `--retry` are client responsibilities in M2.  The worker
/// already understands watch attachments; this process-level contract prevents
/// the CLI from accepting the flags but silently leaving them inert.
#[test]
fn the_phone_cli_reports_watch_mode_and_stops_retrying_after_a_bounded_failure() {
    let harness = Harness::new();
    let watch = latch_cmd(&harness, &["attach", "ses_missing", "--watch"]);
    assert!(
        !watch.status.success() && support::stderr_str(&watch).contains("session"),
        "a watch attach reports an unavailable session without attempting to take control: {}",
        support::stderr_str(&watch)
    );

    // A nonexistent target makes the retry policy observable without sleeping
    // forever or requiring a real network.  It must terminate and tell the
    // phone user that reconnecting has stopped.
    let policy = RetryPolicy::default();
    let started = std::time::Instant::now();
    let retry = latch_cmd(&harness, &["attach", "ses_missing", "--retry", "--watch"]);
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(10), "retry must be bounded");
    assert!(
        elapsed >= policy.budget(),
        "retry gave up after {elapsed:?} without waiting out its own backoff of {:?}; \
         a policy that never sleeps is not a policy",
        policy.budget()
    );
    assert!(
        !retry.status.success() && support::stderr_str(&retry).contains("retry"),
        "a failed retry must report that it gave up instead of silently taking control or hanging: {}",
        support::stderr_str(&retry)
    );

    std::thread::sleep(BRIEF);
}

/// Backoff is bounded, monotonic, and capped.  Asserted directly because the
/// end-to-end tests can only observe the total, and a policy that reached its
/// budget by sleeping once for the whole of it would satisfy them.
#[test]
fn the_retry_policy_backs_off_and_stops() {
    let policy = RetryPolicy::default();
    assert!(policy.attempts > 0, "`--retry` must actually retry");

    let waits: Vec<Duration> = (1..=policy.attempts).map(|n| policy.backoff(n)).collect();
    assert_eq!(waits[0], policy.initial_backoff);
    for pair in waits.windows(2) {
        assert!(pair[1] >= pair[0], "backoff must not shrink: {waits:?}");
    }
    for wait in &waits {
        assert!(*wait <= policy.max_backoff, "backoff must cap: {waits:?}");
    }
    assert_eq!(policy.budget(), waits.iter().sum::<Duration>());
    assert!(
        policy.budget() < Duration::from_secs(30),
        "a phone user staring at a frozen screen deserves an answer, not patience"
    );

    // The no-retry policy is the same code path with nothing to spend, which is
    // what keeps plain `latch attach` and `--retry` from diverging.
    assert_eq!(RetryPolicy::NONE.attempts, 0);
    assert_eq!(RetryPolicy::NONE.budget(), Duration::ZERO);
}

/// A link that ends part-way through a frame must be reported, not waited on.
///
/// The proxy carries a deliberately small prefix of the worker's first response
/// and then ends the stream, so the client is holding an incomplete frame when
/// the transport disappears.  Without `--retry` the only correct behaviours are
/// to say so and exit; hanging is the failure this whole milestone is about.
#[test]
fn a_link_that_dies_mid_frame_is_reported_rather_than_waited_out() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'DESK-SCREEN\\n'; while :; do sleep 0.2; done");
    let mut desk = session.attach(AttachMode::Watch, false, size(DESK.0, DESK.1));
    desk.wait_for_output("DESK-SCREEN", SETTLE);

    let tunnel = SocketProxy::in_front_of(&harness, &session);
    tunnel.truncate_next_connection_after(8);
    let mut phone = PtyProcess::spawn(
        &harness,
        &["attach", tunnel.session_id().as_str(), "--watch"],
        PHONE.0,
        PHONE.1,
    );

    let status = phone.wait(SETTLE);
    assert!(
        !status.success(),
        "a dead transport must not look like a clean detach: {}",
        phone.transcript()
    );
    assert!(
        phone.transcript().contains("connection lost"),
        "the client must say the link died rather than leaving a still screen: {:?}",
        phone.transcript()
    );

    // The session is untouched by its client's misfortune, and the screen the
    // desk client is looking at never moved.
    assert!(session.is_live(), "a dead client must not end the session");
    let mut reattached = session.attach(AttachMode::Watch, false, size(DESK.0, DESK.1));
    reattached.wait_for_output("DESK-SCREEN", SETTLE);
    desk.send_input(b"\n").expect("the desk link is unaffected");
}

/// `--retry` recovers from that same death without any resume protocol.
///
/// The second connection is an ordinary attach, so the screen comes back the
/// only way it ever does: a fresh snapshot.  Watch mode is used deliberately —
/// a reconnecting watcher that came back as a controller would have taken
/// control nobody asked for.
#[test]
fn retry_reconnects_over_a_fresh_link_and_restores_the_screen_without_taking_control() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'AGENT-OUTPUT\\n'; while :; do sleep 0.2; done");
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("AGENT-OUTPUT", SETTLE);

    let tunnel = SocketProxy::in_front_of(&harness, &session);
    // Enough to be admitted and start receiving the screen, not enough to
    // finish it: the drop lands mid-snapshot.
    tunnel.truncate_next_connection_after(220);
    let mut phone = PtyProcess::spawn(
        &harness,
        &["attach", tunnel.session_id().as_str(), "--watch", "--retry"],
        PHONE.0,
        PHONE.1,
    );

    phone.wait_for_screen("reconnecting", SETTLE);
    phone.wait_for_screen("AGENT-OUTPUT", SETTLE);
    assert!(
        tunnel.connections() >= 2,
        "the client must reconnect over a new link rather than resuming the dead one"
    );
    assert!(
        phone.is_running(),
        "a successful reconnection keeps the client attached: {:?}",
        phone.transcript()
    );

    // The desk terminal still holds control, and the reconnected watcher still
    // cannot take it: reconnection is not a promotion.
    let mut challenger = session.connect().expect("the worker still accepts peers");
    assert!(
        matches!(
            challenger.attach(AttachMode::Control, false, size(PHONE.0, PHONE.1)),
            ControlMessage::Error { ref code, .. } if code == "control_busy"
        ),
        "the desk client must still be the controller after the phone reconnected"
    );
}

/// A reconnecting client waits for control it is entitled to and never takes
/// control it is not.
///
/// The interesting case is unavoidable rather than hypothetical: a link that
/// dropped frequently leaves an attachment the worker has not reaped yet, so
/// the client's own ghost is holding control when it reconnects.  Backing off
/// waits that out.  Escalating to a steal would resolve it too — by taking
/// control from whoever really holds it.
#[test]
fn retry_never_escalates_to_a_steal_it_was_not_asked_for() {
    let harness = Harness::new();
    let session = harness.spawn_sh("printf 'DESK-CONTROLS\\n'; while :; do sleep 0.2; done");
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("DESK-CONTROLS", SETTLE);

    let started = std::time::Instant::now();
    let phone = latch_cmd(&harness, &["attach", session.id().as_str(), "--retry"]);
    let elapsed = started.elapsed();

    assert!(
        !phone.status.success(),
        "a controller that cannot get control must not report success"
    );
    assert!(
        support::stderr_str(&phone).contains("retry"),
        "the phone must be told reconnection stopped: {}",
        support::stderr_str(&phone)
    );
    assert!(
        elapsed >= RetryPolicy::default().budget() && elapsed < Duration::from_secs(10),
        "a busy controller is retried on a bounded schedule, not immediately abandoned \
         and not forever: {elapsed:?}"
    );

    // The whole point: the desk client never lost control to a client that was
    // only ever told to reconnect.
    desk.send_input(b"still-mine\n")
        .expect("the desk client is still attached");
    let mut challenger = session.connect().expect("the worker still accepts peers");
    assert!(
        matches!(
            challenger.attach(AttachMode::Control, false, size(PHONE.0, PHONE.1)),
            ControlMessage::Error { ref code, .. } if code == "control_busy"
        ),
        "`--retry` must never turn into `--steal`"
    );
}

/// Peeking from a phone changes nothing about the desk session.
///
/// Both halves matter and they fail separately: a watcher that took control
/// would stop the desk from typing, and a watcher that resized would reflow a
/// 200-column session to 40 while its owner was looking at it.
#[test]
fn watching_from_a_phone_takes_neither_control_nor_the_desk_geometry() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.05; done",
        TerminalSize::new(DESK.0, DESK.1),
    ));
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("50 200", SETTLE);

    let mut phone = PtyProcess::spawn(
        &harness,
        &["attach", session.id().as_str(), "--watch"],
        PHONE.0,
        PHONE.1,
    );
    phone.wait_for_screen("50 200", SETTLE);
    // Rotating the phone is a resize the session must not follow either.
    phone.set_window(PHONE.1, PHONE.0);
    phone.drain(BRIEF);

    let seen = desk.output_for(Duration::from_millis(400));
    let text = String::from_utf8_lossy(&seen);
    assert!(
        text.contains("50 200"),
        "the desk session keeps its geometry while a phone watches: {text:?}"
    );
    assert!(
        !text.contains("20 40") && !text.contains("40 20"),
        "a watcher must never resize the session: {text:?}"
    );

    let mut challenger = session.connect().expect("the worker still accepts peers");
    assert!(
        matches!(
            challenger.attach(AttachMode::Control, false, size(PHONE.0, PHONE.1)),
            ControlMessage::Error { ref code, .. } if code == "control_busy"
        ),
        "a watching phone must not have taken control from the desk"
    );
}

/// D4 through a real client and a real drop, which is what a phone does.
///
/// The worker-level test above proves the registry unwinds its stack.  This one
/// proves the whole path: a `latch attach` process took control at phone size,
/// the child observed the reflow through its own `TIOCGWINSZ`, and the link
/// vanished without a goodbye.
#[test]
fn a_dropped_phone_client_returns_the_desk_session_to_its_own_geometry() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.05; done",
        TerminalSize::new(DESK.0, DESK.1),
    ));
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output("50 200", SETTLE);

    let tunnel = SocketProxy::in_front_of(&harness, &session);
    let mut phone = PtyProcess::spawn(
        &harness,
        &["attach", tunnel.session_id().as_str(), "--steal"],
        PHONE.0,
        PHONE.1,
    );
    phone.wait_for_screen("20 40", SETTLE);
    desk.wait_for_output("20 40", SETTLE);

    tunnel.cut();
    let status = phone.wait(SETTLE);
    assert!(
        !status.success(),
        "a dropped controller reports the drop: {:?}",
        phone.transcript()
    );

    desk.wait_for_output("50 200", SETTLE);
}
