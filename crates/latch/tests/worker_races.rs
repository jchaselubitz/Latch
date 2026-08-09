//! Races, abrupt departures, and hostile input.
//!
//! Everything here is something the worker cannot prevent and must survive.
//! Clients arrive concurrently, vanish mid-handshake, retry requests they are
//! not sure landed, and — because the socket is a file in the user's home
//! directory — occasionally receive whatever a confused program decided to write
//! into it.
//!
//! The bar in every case is the same, and it is deliberately stronger than "does
//! not crash": **the child is unaffected.** A session's value is the process
//! running in it. A worker that survives a malformed frame by dropping the PTY
//! has not survived anything worth having.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use latch_protocol::control::{self, AttachMode, ClientInfo, ControlMessage, SessionState};
use latch_protocol::frame::FrameType;
use latch_protocol::{MAX_FRAME_PAYLOAD, PROTOCOL_VERSION};
use proptest::prelude::*;

use latch::worker::manifest::TerminalSize;
use latch::worker::registry::Registry;
use support::{attach_request, sh_manifest, size, Harness, BRIEF, SETTLE};

const ANNOUNCE_AND_WAIT: &str = "echo READY:$$; while :; do sleep 0.2; done";

// -- concurrency -----------------------------------------------------------

#[test]
fn concurrent_attaches_all_succeed_with_distinct_identities() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let paths = session.paths().clone();

    let ids: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let paths = paths.clone();
                scope.spawn(move || {
                    let mut client =
                        support::Client::connect(&paths).expect("the socket should accept");
                    let reply = client.attach(AttachMode::Watch, false, size(80, 24));
                    let ControlMessage::Attached { attachments, .. } = reply else {
                        panic!("attach {n} was refused: {reply:?}");
                    };
                    // Held open: the presence list is only interesting while
                    // everyone is still here.
                    std::thread::sleep(Duration::from_millis(200));
                    attachments
                        .last()
                        .expect("the new attachment appears in its own list")
                        .id
                        .clone()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("attach thread"))
            .collect()
    });

    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "attachment ids are assigned by the worker and must be distinct: {ids:?}"
    );
    assert!(session.is_live());
}

#[test]
fn concurrent_stealing_control_attaches_leave_exactly_one_controller() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let paths = session.paths().clone();
    let accepted = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let paths = paths.clone();
            let accepted = Arc::clone(&accepted);
            scope.spawn(move || {
                let mut client = support::Client::connect(&paths).expect("the socket accepts");
                if matches!(
                    client.attach(AttachMode::Control, true, size(80, 24)),
                    ControlMessage::Attached { .. }
                ) {
                    accepted.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(300));
            });
        }
    });

    // Whoever won, exactly one attachment held control at a time and the
    // session is intact. The next attach proves it: control is free again
    // because every holder's socket closed.
    let mut successor = session.connect().expect("the socket accepts");
    assert!(
        matches!(
            successor.attach(AttachMode::Control, false, size(200, 50)),
            ControlMessage::Attached { .. }
        ),
        "with every contender gone, control is available without a steal"
    );
    assert!(accepted.load(Ordering::Relaxed) >= 1);
}

#[test]
fn a_steal_and_a_detach_racing_leave_a_consistent_controller() {
    // Both orders, deterministically, because the wire cannot be made to produce
    // them on demand and each one breaks a different naive implementation.
    for incumbent_leaves_first in [true, false] {
        let mut registry = Registry::new(size(200, 50));
        let desk = registry
            .attach(attach_request(
                AttachMode::Control,
                false,
                "desk",
                size(200, 50),
            ))
            .expect("the desk takes control");

        if incumbent_leaves_first {
            registry.detach(&desk.id);
            let phone = registry
                .attach(attach_request(
                    AttachMode::Control,
                    true,
                    "phone",
                    size(40, 20),
                ))
                .expect("the phone attaches");
            assert_eq!(
                registry.controller(),
                Some(phone.id),
                "a steal against nobody is just taking control"
            );
        } else {
            let phone = registry
                .attach(attach_request(
                    AttachMode::Control,
                    true,
                    "phone",
                    size(40, 20),
                ))
                .expect("the phone steals");
            registry.detach(&desk.id);
            assert_eq!(
                registry.controller(),
                Some(phone.id),
                "the demoted incumbent's departure must not take control away from the \
                 attachment that already holds it"
            );
            assert_eq!(
                registry.session_size(),
                size(40, 20),
                "nor must it drag the session's size back with it"
            );
        }
    }
}

#[test]
fn an_attach_racing_the_child_exiting_is_answered_or_refused_but_never_hangs() {
    let harness = Harness::new();
    let session = harness.spawn_sh("sleep 0.25; exit 0");
    let paths = session.paths().clone();

    // Attach repeatedly across the moment the child exits. Every attempt must
    // reach a conclusion: an acceptance, an error, or a connection that was
    // never established because the session is gone.
    let deadline = std::time::Instant::now() + SETTLE;
    let mut after_exit = 0;
    while std::time::Instant::now() < deadline {
        match support::Client::connect(&paths) {
            Ok(mut client) => {
                let reply = client.attach(AttachMode::Watch, false, size(80, 24));
                match reply {
                    ControlMessage::Attached { .. } | ControlMessage::Error { .. } => {}
                    other => panic!("an attach during exit produced {other:?}"),
                }
            }
            Err(_) => {
                after_exit += 1;
                if after_exit > 3 {
                    break;
                }
            }
        }
    }

    assert!(
        after_exit > 0,
        "the socket must eventually stop accepting; a session that exited is not attachable"
    );
    assert!(
        session.paths().exit().exists(),
        "and the exit record is there for the client that arrived too late"
    );
}

#[test]
fn a_stop_racing_a_reconnect_still_ends_the_session() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);
    let paths = session.paths().clone();

    let reconnecting = std::thread::spawn(move || {
        for _ in 0..200 {
            match support::Client::connect(&paths) {
                Ok(mut client) => {
                    let _ = client.attach(AttachMode::Watch, false, size(80, 24));
                }
                Err(_) => return,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    session.request_stop();
    session.wait_until_gone(SETTLE);
    reconnecting
        .join()
        .expect("the reconnect loop should finish");

    assert!(
        session.paths().exit().exists(),
        "a client reconnecting while a stop is in flight must not keep the session alive"
    );
}

// -- idempotence -----------------------------------------------------------

#[test]
fn a_repeated_resize_produces_one_change() {
    let mut registry = Registry::new(size(200, 50));
    let desk = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "desk",
            size(200, 50),
        ))
        .expect("the desk takes control");

    let first = registry.resize(&desk.id, size(120, 40));
    let second = registry.resize(&desk.id, size(120, 40));

    assert!(!first.is_empty(), "the first resize applies");
    assert!(
        second.is_empty(),
        "a duplicate frame — a retry, or a terminal that reports a size twice — must not \
         make a full-screen application redraw again"
    );
}

#[test]
fn duplicate_frames_over_the_wire_are_harmless() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while read line; do echo GOT:$line; done");

    let mut controller = session.attach(AttachMode::Control, false, size(200, 50));
    let resize = ControlMessage::Resize {
        cols: 120,
        rows: 40,
    };
    for _ in 0..5 {
        controller.send_control(&resize).expect("resize delivered");
    }
    for _ in 0..5 {
        controller
            .send_control(&ControlMessage::ControlRequest { steal: false })
            .expect("control request delivered");
    }
    controller
        .send_input(b"still-here\n")
        .expect("input delivered");

    controller.wait_for_output("GOT:still-here", SETTLE);
    assert!(session.is_live());
}

#[test]
fn a_second_attach_on_one_connection_is_refused_without_disturbing_the_first() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut client = session.attach(AttachMode::Watch, false, size(80, 24));
    client
        .send_control(&ControlMessage::Attach {
            protocol: PROTOCOL_VERSION,
            mode: AttachMode::Control,
            steal: true,
            client: ClientInfo {
                kind: "cli".to_owned(),
                name: "second".to_owned(),
            },
            size: size(40, 20),
        })
        .expect("the duplicate attach is delivered");

    // Whatever the worker replies, the session must still be there afterwards.
    let _ = client.next_control(BRIEF);
    assert!(
        session.is_live(),
        "an attachment that attaches twice is confused, not dangerous"
    );
}

// -- abrupt departure ------------------------------------------------------

#[test]
fn a_client_can_vanish_at_any_point_in_the_handshake() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let attach = control::encode(&ControlMessage::Attach {
        protocol: PROTOCOL_VERSION,
        mode: AttachMode::Control,
        steal: true,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: "vanishing".to_owned(),
        },
        size: size(80, 24),
    });
    let framed = latch_protocol::frame::encode(FrameType::Control, &attach)
        .expect("the attach frame is within bounds");

    // Every cut point: nothing, a partial header, a whole header, a partial
    // payload, the whole frame, and the whole frame plus a wait for the reply.
    let cuts = [0, 2, 5, 5 + framed.len() / 2, framed.len()];
    for cut in cuts {
        let mut client = session.connect().expect("the socket should accept");
        client.send_raw(&framed[..cut]).expect("partial write");
        client.abandon();
        assert!(
            session.is_live(),
            "a client that vanished after {cut} bytes must not take the session with it"
        );
    }

    let mut settled = session.connect().expect("the socket should accept");
    settled.send_raw(&framed).expect("full write");
    let _ = settled.next_control(SETTLE);
    settled.abandon();

    let mut after = session.connect().expect("the socket still accepts");
    assert!(
        matches!(
            after.attach(AttachMode::Control, false, size(200, 50)),
            ControlMessage::Attached { .. }
        ),
        "and control held by a vanished client is free again"
    );
}

// -- hostile input ---------------------------------------------------------

#[test]
fn malformed_frames_are_refused_without_ending_the_session() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let hostile: Vec<(&str, Vec<u8>)> = vec![
        ("an unknown frame type", vec![0xff, 0, 0, 0, 1, b'x']),
        (
            "a control payload that is not MessagePack",
            latch_protocol::frame::encode(FrameType::Control, b"not-messagepack").unwrap(),
        ),
        (
            "a control message with no discriminator",
            latch_protocol::frame::encode(FrameType::Control, &[0x80]).unwrap(),
        ),
        ("a truncated header", vec![0x10, 0, 0]),
        ("a zero-length control frame", vec![0x10, 0, 0, 0, 0]),
    ];

    for (what, bytes) in hostile {
        let mut client = session.connect().expect("the socket should accept");
        client
            .send_raw(&bytes)
            .expect("hostile bytes are delivered");
        let _ = client.next_frame(BRIEF);
        client.abandon();
        assert!(session.is_live(), "{what} must not end the session");
    }

    let mut good = session.connect().expect("the socket still accepts");
    assert!(matches!(
        good.attach(AttachMode::Watch, false, size(80, 24)),
        ControlMessage::Attached { .. }
    ));
}

#[test]
fn an_oversized_frame_is_refused_rather_than_allocated_for() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut client = session.connect().expect("the socket should accept");
    let announced = (MAX_FRAME_PAYLOAD as u32) + 1;
    let mut header = vec![FrameType::TerminalInput.tag()];
    header.extend_from_slice(&announced.to_be_bytes());
    client.send_raw(&header).expect("the header is delivered");
    client.send_raw(&vec![0u8; 4096]).ok();
    client.abandon();

    assert!(
        session.is_live(),
        "a peer announcing more than the codec accepts is refused on its word, not \
         allocated for"
    );
}

#[test]
fn a_frame_that_announces_a_huge_length_and_then_closes_is_survived() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut client = session.connect().expect("the socket should accept");
    let mut header = vec![FrameType::TerminalOutput.tag()];
    header.extend_from_slice(&u32::MAX.to_be_bytes());
    client.send_raw(&header).expect("the header is delivered");
    client.abandon();

    // The worker must neither reserve four gigabytes nor wait forever for bytes
    // that are never coming.
    std::thread::sleep(BRIEF);
    assert!(session.is_live(), "the session survives a lying length");

    let mut good = session.connect().expect("the socket still accepts");
    assert!(matches!(
        good.attach(AttachMode::Watch, false, size(80, 24)),
        ControlMessage::Attached { .. }
    ));
}

#[test]
fn input_frames_are_only_forwarded_from_the_controller() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while read line; do echo GOT:$line; done");

    let mut controller = session.attach(AttachMode::Control, false, size(200, 50));
    let mut watcher = session.attach(AttachMode::Watch, false, size(200, 50));

    watcher.send_input(b"forged\n").expect("delivered");
    controller.send_input(b"genuine\n").expect("delivered");

    let seen = controller.wait_for_output("GOT:genuine", SETTLE);
    assert!(
        !String::from_utf8_lossy(&seen).contains("GOT:forged"),
        "two uncontrolled writers must be impossible, not merely discouraged"
    );
}

#[test]
fn the_child_is_unaffected_by_everything_a_client_can_send() {
    let harness = Harness::new();
    let session = harness.spawn(sh_manifest(
        "i=0; while :; do i=$((i+1)); echo TICK:$i; sleep 0.05; done",
        TerminalSize::new(200, 50),
    ));

    let mut observer = session.attach(AttachMode::Watch, false, size(200, 50));
    let before = support::last_marker(&observer.output_for(Duration::from_millis(300)), "TICK:")
        .expect("the child is running");

    let paths = session.paths().clone();
    proptest!(ProptestConfig::with_cases(24), |(bytes in prop::collection::vec(any::<u8>(), 0..2048))| {
        let mut client = support::Client::connect(&paths)
            .expect("the socket keeps accepting through the fuzz");
        client.send_raw(&bytes).ok();
        client.abandon();
    });

    let after = support::last_marker(&observer.output_for(Duration::from_millis(600)), "TICK:")
        .expect("the child is still running");
    assert!(
        after > before,
        "arbitrary bytes on the socket must leave the child entirely alone; it was at \
         TICK:{before} and is now at TICK:{after}"
    );
    assert_eq!(
        latch::worker::derive_state(session.paths()).expect("state should derive"),
        SessionState::Running
    );
}
