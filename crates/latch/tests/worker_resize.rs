//! Resize authority: decision D4.
//!
//! The configuration under test is the real one, not a reduced example. A desk
//! session is 200 columns in iTerm; Termius on a phone is about 40. For this
//! product that pairing is the ordinary case, and it arrives at M2 — so the
//! numbers in these tests are 200 and 40 rather than 10 and 20.
//!
//! What must hold: the session's size is the current controller's size, it
//! reverts when that controller detaches, watchers never move it, nested
//! takeovers unwind in order, and an explicit pin outranks all of it.
//!
//! Nesting is the part a single "previous size" gets wrong. Desk, then phone,
//! then tablet: one remembered value gets the first unwind right and every later
//! one wrong, and the symptom is a desk session that stays at 40 columns for no
//! visible reason.

mod support;

use std::time::Duration;

use latch_protocol::control::AttachMode;

use latch::cli::manage::{self, ResizeRequest};
use latch::worker::registry::{AttachmentId, Effect, Registry};
use latch::worker::resize::ResizeAuthority;
use support::{attach_request, resizes, size, Harness, SETTLE};

const DESK: (u16, u16) = (200, 50);
const PHONE: (u16, u16) = (40, 20);
const TABLET: (u16, u16) = (80, 30);

fn attachment(name: &str) -> AttachmentId {
    AttachmentId::new(name)
}

#[test]
fn the_session_size_follows_the_controller() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    assert_eq!(authority.effective(), size(DESK.0, DESK.1));

    let changed = authority.take_control(&attachment("phone"), size(PHONE.0, PHONE.1));

    assert_eq!(changed, Some(size(PHONE.0, PHONE.1)));
    assert_eq!(
        authority.effective(),
        size(PHONE.0, PHONE.1),
        "the phone takes control and the session reflows so the agent is usable on it"
    );
}

#[test]
fn the_size_reverts_when_the_controller_leaves() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    authority.take_control(&phone, size(PHONE.0, PHONE.1));

    let reverted = authority.release(&phone);

    assert_eq!(reverted, Some(size(DESK.0, DESK.1)));
    assert_eq!(
        authority.effective(),
        size(DESK.0, DESK.1),
        "the phone disconnects and the desk session is intact — that is the whole promise"
    );
    assert_eq!(authority.depth(), 0);
}

#[test]
fn nested_takeovers_unwind_in_order() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    let tablet = attachment("tablet");

    authority.take_control(&phone, size(PHONE.0, PHONE.1));
    authority.take_control(&tablet, size(TABLET.0, TABLET.1));
    assert_eq!(authority.effective(), size(TABLET.0, TABLET.1));
    assert_eq!(authority.depth(), 2, "both takeovers are remembered");

    assert_eq!(
        authority.release(&tablet),
        Some(size(PHONE.0, PHONE.1)),
        "the tablet leaving returns to the phone, not to the desk"
    );
    assert_eq!(
        authority.release(&phone),
        Some(size(DESK.0, DESK.1)),
        "and the phone leaving returns to the desk"
    );
    assert_eq!(authority.depth(), 0);
}

#[test]
fn a_controller_that_already_lost_control_cannot_move_the_size_on_its_way_out() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    let tablet = attachment("tablet");
    authority.take_control(&phone, size(PHONE.0, PHONE.1));
    authority.take_control(&tablet, size(TABLET.0, TABLET.1));

    // The phone was stolen from and now closes its socket. It is no longer the
    // top of the stack, so its departure must not reflow the tablet's screen.
    let effect = authority.release(&phone);

    assert_eq!(
        effect, None,
        "a departure from the middle of the stack changes nothing"
    );
    assert_eq!(authority.effective(), size(TABLET.0, TABLET.1));
    assert_eq!(
        authority.release(&tablet),
        Some(size(DESK.0, DESK.1)),
        "and the remaining unwind skips the entry that already left"
    );
}

#[test]
fn a_watcher_never_moves_the_session() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    let onlooker = attachment("onlooker");
    authority.take_control(&phone, size(PHONE.0, PHONE.1));

    assert_eq!(
        authority.controller_resize(&onlooker, size(12, 4)),
        None,
        "a resize from an attachment that does not hold control is ignored"
    );
    assert_eq!(authority.effective(), size(PHONE.0, PHONE.1));
}

#[test]
fn the_controllers_own_window_resize_is_honored() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    authority.take_control(&phone, size(PHONE.0, PHONE.1));

    assert_eq!(
        authority.controller_resize(&phone, size(60, 25)),
        Some(size(60, 25)),
        "a phone rotating is the controller resizing, and it applies"
    );
    assert_eq!(
        authority.release(&phone),
        Some(size(DESK.0, DESK.1)),
        "and the revert still returns to what was in effect before it took control, \
         not to the size it first arrived with"
    );
}

#[test]
fn an_unchanged_size_is_not_reported_as_a_change() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let desk = attachment("desk");

    assert_eq!(
        authority.take_control(&desk, size(DESK.0, DESK.1)),
        None,
        "an unnecessary SIGWINCH makes a full-screen application redraw for nothing"
    );
    assert_eq!(
        authority.controller_resize(&desk, size(DESK.0, DESK.1)),
        None
    );
}

#[test]
fn a_pinned_size_survives_a_controller_change() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");

    authority.pin(size(132, 43));
    assert!(authority.is_pinned());
    assert_eq!(authority.effective(), size(132, 43));

    assert_eq!(
        authority.take_control(&phone, size(PHONE.0, PHONE.1)),
        None,
        "a user who pinned a size said something about their intent that a phone attaching \
         must not undo"
    );
    assert_eq!(authority.effective(), size(132, 43));
    assert_eq!(authority.release(&phone), None);
    assert_eq!(authority.effective(), size(132, 43));
}

#[test]
fn unpinning_restores_whatever_the_stack_says() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    authority.pin(size(132, 43));
    authority.take_control(&phone, size(PHONE.0, PHONE.1));

    assert_eq!(
        authority.unpin(),
        Some(size(PHONE.0, PHONE.1)),
        "the stack kept tracking while the pin was in force, so releasing it lands on the \
         current controller rather than on whatever was in effect when the pin was set"
    );
    assert_eq!(authority.effective(), size(PHONE.0, PHONE.1));
}

#[test]
fn an_explicit_resize_changes_what_the_session_reverts_to() {
    let mut authority = ResizeAuthority::new(size(DESK.0, DESK.1));
    let phone = attachment("phone");
    authority.take_control(&phone, size(PHONE.0, PHONE.1));

    authority.set_base(size(120, 40));
    assert_eq!(
        authority.effective(),
        size(PHONE.0, PHONE.1),
        "an unpinned explicit resize does not evict the current controller"
    );
    assert_eq!(
        authority.release(&phone),
        Some(size(120, 40)),
        "but it is what the session returns to"
    );
}

// -- through the registry --------------------------------------------------

#[test]
fn the_registry_resizes_the_pty_when_control_moves() {
    let mut registry = Registry::new(size(DESK.0, DESK.1));
    let desk = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "desk",
            size(DESK.0, DESK.1),
        ))
        .expect("the desk takes control");

    let phone = registry
        .attach(attach_request(
            AttachMode::Control,
            true,
            "phone",
            size(PHONE.0, PHONE.1),
        ))
        .expect("the phone steals control");
    assert_eq!(
        resizes(&phone.effects),
        vec![size(PHONE.0, PHONE.1)],
        "a steal moves the session's size"
    );
    assert_eq!(registry.session_size(), size(PHONE.0, PHONE.1));

    let departure = registry.detach(&phone.id);
    assert_eq!(
        resizes(&departure),
        vec![size(DESK.0, DESK.1)],
        "and its departure moves it back"
    );
    assert_eq!(registry.session_size(), size(DESK.0, DESK.1));
    let _ = desk;
}

#[test]
fn a_watchers_resize_produces_no_effects_and_is_not_an_error() {
    let mut registry = Registry::new(size(DESK.0, DESK.1));
    let onlooker = registry
        .attach(attach_request(
            AttachMode::Watch,
            false,
            "onlooker",
            size(100, 30),
        ))
        .expect("a watcher attaches");

    let effects = registry.resize(&onlooker.id, size(37, 11));

    assert!(
        effects.is_empty(),
        "a phone that attached to peek still rotates; every rotation must not be a protocol \
         violation, and it must not move the session either"
    );
    assert_eq!(registry.session_size(), size(DESK.0, DESK.1));
}

// -- end to end ------------------------------------------------------------

#[test]
fn the_child_sees_the_desk_size_then_the_phone_size_then_the_desk_size_again() {
    let harness = Harness::new();
    // The child reports the size the kernel gave the pty, so this asserts the
    // resize actually reached the process rather than only the worker's model.
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.1; done",
        latch::worker::manifest::TerminalSize::new(DESK.0, DESK.1),
    ));

    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output(&format!("{} {}", DESK.1, DESK.0), SETTLE);

    let phone = session.attach(AttachMode::Control, true, size(PHONE.0, PHONE.1));
    desk.wait_for_output(&format!("{} {}", PHONE.1, PHONE.0), SETTLE);

    phone.abandon();
    desk.wait_for_output(&format!("{} {}", DESK.1, DESK.0), SETTLE);
}

#[test]
fn a_watcher_attaching_at_a_different_size_leaves_the_child_alone() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.1; done",
        latch::worker::manifest::TerminalSize::new(DESK.0, DESK.1),
    ));

    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));
    desk.wait_for_output(&format!("{} {}", DESK.1, DESK.0), SETTLE);

    let _onlooker = session.attach(AttachMode::Watch, false, size(PHONE.0, PHONE.1));
    let seen = desk.output_for(Duration::from_millis(600));
    let text = String::from_utf8_lossy(&seen);

    assert!(
        !text.contains(&format!("{} {}", PHONE.1, PHONE.0)),
        "a watcher must never resize the session: {text}"
    );
}

#[test]
fn the_resize_command_reaches_the_worker_and_pin_survives_a_takeover() {
    let harness = Harness::new();
    let session = harness.spawn(support::sh_manifest(
        "while :; do stty size; sleep 0.1; done",
        latch::worker::manifest::TerminalSize::new(DESK.0, DESK.1),
    ));
    let mut desk = session.attach(AttachMode::Control, false, size(DESK.0, DESK.1));

    let report = manage::resize(ResizeRequest {
        home: harness.home().clone(),
        session: session.id().to_string(),
        cols: 132,
        rows: 43,
        pin: true,
    })
    .expect("the management resize should be delivered");
    assert!(report.pinned);
    desk.wait_for_output("43 132", SETTLE);

    let _phone = session.attach(AttachMode::Control, true, size(PHONE.0, PHONE.1));
    let seen = desk.output_for(Duration::from_millis(600));
    assert!(
        !String::from_utf8_lossy(&seen).contains(&format!("{} {}", PHONE.1, PHONE.0)),
        "a controller takeover must not override a management pin"
    );
}

#[test]
fn a_controller_that_reflows_the_session_is_sent_the_reflowed_screen() {
    // The snapshot is serialized from the screen model, so whether it is right
    // for its recipient is decided entirely by whether the reflow has already
    // happened when it is taken. Asserted on the effect order because that is
    // where the decision lives; `remote_attach.rs` asserts what a 40-column
    // phone actually receives because that is where it is felt.
    let mut registry = Registry::new(size(DESK.0, DESK.1));
    let phone = registry
        .attach(attach_request(
            AttachMode::Control,
            false,
            "phone",
            size(PHONE.0, PHONE.1),
        ))
        .expect("the phone takes control of an unheld session");

    let resize_at = phone
        .effects
        .iter()
        .position(|effect| matches!(effect, Effect::Resize { .. }))
        .expect("taking control at a new size resizes the session");
    let snapshot_at = phone
        .effects
        .iter()
        .position(|effect| matches!(effect, Effect::Snapshot { .. }))
        .expect("every attach is answered with the screen");

    assert!(
        resize_at < snapshot_at,
        "the session must reflow before its screen is serialized, or the new \
         controller is sent the previous controller's geometry: {:?}",
        phone.effects.iter().map(effect_name).collect::<Vec<_>>()
    );
    assert_eq!(
        resizes(&phone.effects),
        vec![size(PHONE.0, PHONE.1)],
        "and it reflows to exactly the size the new controller asked for"
    );
}

fn effect_name(effect: &Effect) -> &'static str {
    match effect {
        Effect::Send { .. } => "send",
        Effect::Broadcast { .. } => "broadcast",
        Effect::Resize { .. } => "resize",
        Effect::Snapshot { .. } => "snapshot",
        Effect::Close { .. } => "close",
    }
}
