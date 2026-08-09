//! The control socket and the attachment registry.
//!
//! Two levels, on purpose.
//!
//! The registry is a state machine that performs no I/O and returns the effects
//! it wants performed, so "the incumbent is demoted *and told*" is asserted as an
//! ordered list rather than inferred from bytes that happened to arrive in a
//! convenient order. The same tests then run against a live socket, because a
//! correct state machine wired up wrongly is still a broken product.
//!
//! Three claims this file exists to pin:
//!
//! - **The snapshot is not a message.** After `attached`, the screen arrives as
//!   ordinary `terminal.output`. Reconnect is therefore just attach — no replay
//!   round trip, no second code path, which is what makes mobile reattach
//!   reliable rather than a separate thing to maintain.
//! - **Control is released by socket close, not by timer expiry.** There is no
//!   lease here and no clock. Locally the operating system reports departure
//!   precisely, so a timer could only be wrong relative to it (decision D5).
//! - **`session.update` merges.** An absent field and a null field mean
//!   different things, and conflating them is how a title change silently
//!   empties a presence list.

mod support;

use std::time::Duration;

use latch_protocol::control::{AttachMode, ClientInfo, ControlMessage, SessionState};
use latch_protocol::frame::FrameType;
use latch_protocol::PROTOCOL_VERSION;

use latch::worker::manifest::{TerminalSize, WorkerTuning};
use latch::worker::registry::{Effect, Registry, Rejection};
use latch::worker::socket::{self, PeerCredentials, SocketError};
use support::{
    attach_request, broadcasts, current_uid, sent_to, sh_manifest, size, snapshots_to, Harness,
    BRIEF, SETTLE,
};

const ANNOUNCE_AND_WAIT: &str = "echo READY; while :; do sleep 0.2; done";

// -- peer credentials ------------------------------------------------------

#[test]
fn a_peer_of_another_uid_is_refused() {
    let owner = current_uid();
    let stranger = PeerCredentials {
        uid: owner.wrapping_add(1),
        gid: 0,
        pid: 1,
    };

    let err = socket::authorize(&stranger, owner)
        .expect_err("another user must never reach a session socket");
    assert!(
        matches!(err, SocketError::PeerRejected { .. }),
        "a session carries live input to a process running as its owner; anything short of \
         `is the owner` is a privilege boundary being crossed: {err}"
    );

    socket::authorize(
        &PeerCredentials {
            uid: owner,
            gid: 0,
            pid: 1,
        },
        owner,
    )
    .expect("the owner must be admitted");
}

#[test]
fn a_connection_from_an_unauthorized_uid_is_closed_and_the_session_survives() {
    let harness = Harness::new();
    // The session is owned by a uid that is not this test's, so every connection
    // this test makes is the "another user" case — reachable without a second
    // account, which is what makes the rejection path testable at all.
    let session = harness.spawn(support::tuned(
        sh_manifest(ANNOUNCE_AND_WAIT, TerminalSize::new(200, 50)),
        WorkerTuning {
            owner_uid: Some(current_uid().wrapping_add(1)),
            ..WorkerTuning::default()
        },
    ));

    let mut client = session
        .connect()
        .expect("the socket accepts before it checks");
    // Sent, and possibly never read: the worker checks credentials before it
    // reads a byte, so this write may land in a socket nobody is listening to.
    let _ = client.send_control(&ControlMessage::Attach {
        protocol: PROTOCOL_VERSION,
        mode: AttachMode::Watch,
        steal: false,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: "stranger".to_owned(),
        },
        size: size(80, 24),
    });

    assert!(
        client.next_frame(SETTLE).is_none(),
        "a peer that is not the owner is closed without a reply; there is nothing to negotiate \
         with a connection that is not permitted to be here, and an error frame would only \
         confirm the socket is real"
    );
    assert!(
        session.is_live(),
        "a refused peer must not take the session down with it"
    );
    assert!(
        !session.paths().exit().exists(),
        "the child must be entirely unaffected by a connection it never saw"
    );
}

// -- handshake -------------------------------------------------------------

#[test]
fn an_attach_is_answered_with_attached_and_then_the_screen() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut client = session.connect().expect("the socket should accept");
    let reply = client.attach(AttachMode::Control, false, size(120, 40));

    let ControlMessage::Attached {
        protocol,
        session: info,
        controller,
        attachments,
    } = reply
    else {
        panic!("expected `attached`, got {reply:?}");
    };
    assert_eq!(protocol, PROTOCOL_VERSION);
    assert_eq!(info.state, SessionState::Running);
    assert_eq!(
        info.size,
        size(120, 40),
        "the session's size is the controller's size the moment it takes control"
    );
    assert!(
        controller.is_some(),
        "a control attach with nobody holding control gets it"
    );
    assert_eq!(
        attachments.len(),
        1,
        "the new attachment appears in its own presence list"
    );

    let next = client
        .next_frame(SETTLE)
        .expect("the screen should follow the acceptance");
    assert_eq!(
        next.frame_type,
        FrameType::TerminalOutput,
        "the snapshot is not a message: it arrives as ordinary terminal output, which is what \
         makes reconnect just attach"
    );
}

#[test]
fn an_unsupported_protocol_version_gets_an_error_and_a_close() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut client = session.connect().expect("the socket should accept");
    let reply = client.attach_with_protocol(
        PROTOCOL_VERSION + 99,
        AttachMode::Watch,
        false,
        size(80, 24),
    );

    match reply {
        ControlMessage::Error { code, .. } => assert_eq!(
            code, "unsupported_protocol",
            "version negotiation rides `attach` and has exactly one failing outcome"
        ),
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(
        client.next_frame(BRIEF).is_none(),
        "an unsupported version is answered and closed, never served best-effort"
    );
    assert!(session.is_live(), "the session keeps serving everyone else");
}

#[test]
fn a_watcher_never_reaches_the_child() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while read line; do echo GOT:$line; done");

    let mut watcher = session.attach(AttachMode::Watch, false, size(80, 24));
    watcher
        .send_input(b"from-a-watcher\n")
        .expect("the input should be delivered to the socket");

    let output = watcher.output_for(Duration::from_millis(500));
    assert!(
        !String::from_utf8_lossy(&output).contains("GOT:from-a-watcher"),
        "a watcher's input must not reach the child; exactly one attachment may write"
    );
}

#[test]
fn a_controller_reaches_the_child() {
    let harness = Harness::new();
    let session = harness.spawn_sh("while read line; do echo GOT:$line; done");

    let mut controller = session.attach(AttachMode::Control, false, size(80, 24));
    controller
        .send_input(b"from-the-controller\n")
        .expect("the input should be delivered");

    controller.wait_for_output("GOT:from-the-controller", SETTLE);
}

// -- exclusive write -------------------------------------------------------

#[test]
fn a_second_control_attach_is_refused_with_control_busy() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let _incumbent = session.attach(AttachMode::Control, false, size(200, 50));

    let mut challenger = session.connect().expect("the socket should accept");
    let reply = challenger.attach(AttachMode::Control, false, size(40, 20));
    match reply {
        ControlMessage::Error { code, .. } => assert_eq!(code, "control_busy"),
        other => panic!("a second controller must be refused, got {other:?}"),
    }
}

#[test]
fn a_refused_controller_does_not_silently_become_a_watcher() {
    let mut registry = Registry::new(size(200, 50));
    let incumbent = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "desk",
            size(200, 50),
        ))
        .expect("the first controller is admitted");

    let rejection = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "phone",
            size(40, 20),
        ))
        .expect_err("a second controller is refused");
    assert!(matches!(rejection, Rejection::ControlBusy { .. }));

    assert_eq!(
        registry.len(),
        1,
        "a refusal is terminal for that connection: a client that asked to type and was \
         refused must not end up attached believing it can"
    );
    assert_eq!(registry.controller(), Some(incumbent.id));
}

#[test]
fn stealing_demotes_the_incumbent_and_tells_everyone() {
    let mut registry = Registry::new(size(200, 50));
    let desk = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "desk",
            size(200, 50),
        ))
        .expect("the desk takes control");
    let watcher = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(80, 24),
        ))
        .expect("a watcher attaches");

    let phone = registry
        .attach(attach_request(
            AttachMode::Control,
            true,
            "phone",
            size(40, 20),
        ))
        .expect("a steal is admitted");

    assert_eq!(
        registry.controller(),
        Some(phone.id.clone()),
        "the stealer holds control"
    );

    let announcements: Vec<_> = broadcasts(&phone.effects)
        .into_iter()
        .filter(|message| matches!(message, ControlMessage::ControlState { .. }))
        .collect();
    assert_eq!(
        announcements.len(),
        1,
        "control moving is announced exactly once, to everyone"
    );
    match announcements[0] {
        ControlMessage::ControlState { controller_id, .. } => assert_eq!(
            controller_id.as_deref(),
            Some(phone.id.as_str()),
            "the broadcast names the new controller"
        ),
        other => panic!("expected control.state, got {other:?}"),
    }

    // The demoted client learns from the same broadcast every other client sees.
    // A private "you were demoted" message would be a second code path that
    // could disagree with the first.
    assert!(
        sent_to(&phone.effects, &desk.id).is_empty(),
        "the incumbent is told by the broadcast, not by a message only it receives"
    );
    let _ = watcher;
}

#[test]
fn control_state_reaches_every_attachment_over_the_wire() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let mut desk = session.attach(AttachMode::Control, false, size(200, 50));
    let mut onlooker = session.attach(AttachMode::Watch, false, size(100, 30));
    let mut phone = session.connect().expect("the socket should accept");
    phone.attach(AttachMode::Control, true, size(40, 20));

    for (who, client) in [("desk", &mut desk), ("onlooker", &mut onlooker)] {
        let state = wait_for_control_state(client)
            .unwrap_or_else(|| panic!("{who} should be told that control moved"));
        match state {
            ControlMessage::ControlState { controller_id, .. } => assert!(
                controller_id.is_some(),
                "{who} should learn who holds control now"
            ),
            other => panic!("expected control.state, got {other:?}"),
        }
    }
}

#[test]
fn control_is_released_when_the_controller_closes_its_socket() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    let phone = session.attach(AttachMode::Control, false, size(40, 20));
    let mut onlooker = session.attach(AttachMode::Watch, false, size(200, 50));

    // No goodbye message exists, and none is needed: every real departure looks
    // exactly like this. There is no lease to expire, because locally the
    // operating system already reports the departure precisely.
    phone.abandon();

    let state = wait_for_control_state(&mut onlooker)
        .expect("the remaining attachment should learn control was released");
    match state {
        ControlMessage::ControlState { controller_id, .. } => assert!(
            controller_id.is_none(),
            "with the controller gone, nobody holds control"
        ),
        other => panic!("expected control.state, got {other:?}"),
    }

    let mut successor = session.connect().expect("the socket should accept");
    let reply = successor.attach(AttachMode::Control, false, size(200, 50));
    assert!(
        matches!(reply, ControlMessage::Attached { .. }),
        "control released by a close is available immediately, without a steal: {reply:?}"
    );
}

#[test]
fn a_repeated_control_request_from_the_holder_changes_nothing() {
    let mut registry = Registry::new(size(200, 50));
    let desk = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "desk",
            size(200, 50),
        ))
        .expect("the desk takes control");

    let effects = registry
        .request_control(&desk.id, false)
        .expect("asking for control you already hold is not an error");

    assert!(
        effects.is_empty(),
        "a client retrying a request it is unsure landed must not produce a second broadcast"
    );
    assert_eq!(registry.controller(), Some(desk.id));
}

// -- presence and merging --------------------------------------------------

#[test]
fn a_title_change_carries_only_the_title() {
    let mut registry = Registry::new(size(200, 50));
    registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(80, 24),
        ))
        .expect("a watcher attaches");

    let effects = registry.set_title(Some("Authentication refactor".to_owned()));
    let updates = broadcasts(&effects);
    assert_eq!(updates.len(), 1, "a title change is announced once");
    match updates[0] {
        ControlMessage::SessionUpdate {
            state,
            attachments,
            title,
        } => {
            assert_eq!(
                title.clone(),
                Some(Some("Authentication refactor".to_owned()))
            );
            assert!(
                state.is_none() && attachments.is_none(),
                "session.update merges: fields that did not change are absent, not resent — \
                 a present list replaces the client's entirely"
            );
        }
        other => panic!("expected session.update, got {other:?}"),
    }
}

#[test]
fn clearing_a_title_is_distinguishable_from_leaving_it_alone() {
    let mut registry = Registry::new(size(200, 50));
    registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(80, 24),
        ))
        .expect("a watcher attaches");
    registry.set_title(Some("temporary".to_owned()));

    let effects = registry.set_title(None);
    match broadcasts(&effects).first().copied() {
        Some(ControlMessage::SessionUpdate { title, .. }) => assert_eq!(
            title.clone(),
            Some(None),
            "an explicit clear is `Some(None)`; absent would mean unchanged, and conflating \
             the two is how a cleared title comes back"
        ),
        other => panic!("expected session.update, got {other:?}"),
    }
}

#[test]
fn a_new_attachment_updates_everyone_elses_presence_list() {
    let mut registry = Registry::new(size(200, 50));
    let first = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "first",
            size(80, 24),
        ))
        .expect("the first attaches");

    let second = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "second",
            size(80, 24),
        ))
        .expect("the second attaches");

    let updates: Vec<_> = broadcasts(&second.effects)
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::SessionUpdate {
                attachments: Some(list),
                state,
                title,
            } => Some((list.clone(), state.is_none(), title.is_none())),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1, "presence changes are announced once");
    let (list, no_state, no_title) = &updates[0];
    assert_eq!(list.len(), 2, "the list names everyone present");
    assert!(
        *no_state && *no_title,
        "presence is merged into the client's view; a presence change must not restate a title"
    );
    assert_ne!(first.id, second.id, "attachment ids are distinct");
}

#[test]
fn a_detach_is_idempotent() {
    let mut registry = Registry::new(size(200, 50));
    let watcher = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(80, 24),
        ))
        .expect("a watcher attaches");

    let first = registry.detach(&watcher.id);
    let second = registry.detach(&watcher.id);

    assert!(!first.is_empty(), "the first detach is announced");
    assert!(
        second.is_empty(),
        "detaching an attachment that is already gone is the ordinary race — a request in \
         flight from a connection that has closed — and must not be an error"
    );
    assert!(registry.is_empty());
}

#[test]
fn an_exit_tells_every_attachment_before_closing_it() {
    let mut registry = Registry::new(size(200, 50));
    let watcher = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(80, 24),
        ))
        .expect("a watcher attaches");

    let effects = registry.mark_exited(ControlMessage::SessionExited {
        code: Some(0),
        signal: None,
        at: "2026-08-06T12:00:00Z".to_owned(),
    });

    let exited_at = effects
        .iter()
        .position(|effect| {
            matches!(effect, Effect::Broadcast { message } if matches!(message, ControlMessage::SessionExited { .. }))
        })
        .expect("the exit is broadcast");
    let closed_at = effects
        .iter()
        .position(|effect| matches!(effect, Effect::Close { to } if *to == watcher.id))
        .expect("the attachment is closed");
    assert!(
        exited_at < closed_at,
        "no client may be dropped without first being told why"
    );
}

#[test]
fn a_client_watching_a_session_that_exits_is_told_over_the_wire() {
    let harness = Harness::new();
    let session = harness.spawn_sh("sleep 0.3; exit 3");

    let mut watcher = session.attach(AttachMode::Watch, false, size(80, 24));
    let mut seen = None;
    for _ in 0..40 {
        match watcher.next_control(SETTLE) {
            Some(ControlMessage::SessionExited { code, signal, at }) => {
                seen = Some((code, signal, at));
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }

    let (code, signal, at) = seen.expect("an attached client must be told the child exited");
    assert_eq!(code, Some(3));
    assert_eq!(signal, None);
    assert!(
        !at.is_empty(),
        "the exit is timestamped, matching exit.json"
    );
}

// -- snapshot on attach ----------------------------------------------------

#[test]
fn every_attach_is_answered_with_a_snapshot_effect() {
    let mut registry = Registry::new(size(200, 50));
    let first = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "first",
            size(80, 24),
        ))
        .expect("the first attaches");

    assert_eq!(
        snapshots_to(&first.effects, &first.id),
        1,
        "reconnect is just attach: the same path serves a first attach and a reattach, which \
         is what keeps mobile reattach from being a second thing to maintain"
    );

    let ordered: Vec<_> = first
        .effects
        .iter()
        .map(|effect| match effect {
            Effect::Send { message, .. } => message.discriminator(),
            Effect::Broadcast { message } => message.discriminator(),
            Effect::Resize { .. } => "resize-pty",
            Effect::Snapshot { .. } => "snapshot",
            Effect::Close { .. } => "close",
        })
        .collect();
    assert_eq!(
        ordered.first().copied(),
        Some("attached"),
        "the acceptance comes first: {ordered:?}"
    );
    assert_eq!(
        ordered.get(1).copied(),
        Some("snapshot"),
        "and the screen comes straight after it, before any live output: {ordered:?}"
    );
}

#[test]
fn a_reattach_reproduces_what_a_continuously_attached_client_shows() {
    let harness = Harness::new();
    let session =
        harness.spawn_sh("printf 'DISTINCTIVE-SCREEN-CONTENT\\r\\n'; while :; do sleep 0.2; done");

    let mut first = session.attach(AttachMode::Watch, false, size(80, 24));
    first.wait_for_output("DISTINCTIVE-SCREEN-CONTENT", SETTLE);
    first.abandon();

    let mut second = session.attach(AttachMode::Watch, false, size(80, 24));
    second.wait_for_output("DISTINCTIVE-SCREEN-CONTENT", SETTLE);
}

fn wait_for_control_state(client: &mut support::Client) -> Option<ControlMessage> {
    for _ in 0..20 {
        match client.next_control(SETTLE) {
            Some(message @ ControlMessage::ControlState { .. }) => return Some(message),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}
