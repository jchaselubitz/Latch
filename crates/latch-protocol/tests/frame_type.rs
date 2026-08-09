//! The frame tag byte is a protocol constant, asserted from outside the crate.
//!
//! These values appear in `fixtures/protocol/` and in the TypeScript codec that
//! arrives at M3. Renumbering one is a wire break, so it should fail here
//! loudly rather than surface as a mystery in a client six months later.

use latch_protocol::frame::{FrameError, FrameType};
use latch_protocol::{MAX_FRAME_PAYLOAD, PROTOCOL_VERSION};

#[test]
fn tags_have_their_specified_values() {
    assert_eq!(FrameType::TerminalOutput.tag(), 0x01);
    assert_eq!(FrameType::TerminalInput.tag(), 0x02);
    assert_eq!(FrameType::Control.tag(), 0x10);
}

#[test]
fn known_tags_round_trip() {
    for ty in [
        FrameType::TerminalOutput,
        FrameType::TerminalInput,
        FrameType::Control,
    ] {
        assert_eq!(FrameType::from_tag(ty.tag()), Ok(ty));
    }
}

#[test]
fn unknown_tags_are_rejected_rather_than_guessed() {
    // 0x00 and 0x03 sit either side of the terminal frame types, and 0x11 is
    // the obvious place a ninth control message would land by accident.
    for tag in [0x00, 0x03, 0x0f, 0x11, 0xff] {
        assert_eq!(FrameType::from_tag(tag), Err(FrameError::UnknownType(tag)));
    }
}

#[test]
fn protocol_constants_are_what_clients_negotiate_against() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(MAX_FRAME_PAYLOAD, 4 * 1024 * 1024);
}

proptest::proptest! {
    /// Anything outside the three specified tags must be an error. A decoder
    /// that quietly accepts an unrecognized tag has no way to know how long the
    /// payload is, so it desynchronizes the whole stream rather than dropping
    /// one frame.
    #[test]
    fn only_specified_tags_decode(tag in proptest::num::u8::ANY) {
        let decoded = FrameType::from_tag(tag);
        match tag {
            0x01 | 0x02 | 0x10 => proptest::prop_assert!(decoded.is_ok()),
            _ => proptest::prop_assert_eq!(decoded, Err(FrameError::UnknownType(tag))),
        }
    }
}
