//! The Latch wire protocol.
//!
//! One duplex byte stream carries a session. Locally that stream is a Unix
//! socket; from M3 onward the same frame vocabulary rides a WebSocket. Nothing
//! in this crate may assume which.
//!
//! ```text
//! u8   type
//! u32  length (big-endian)
//! [u8] payload
//! ```
//!
//! | Type   | Name              | Payload                                  |
//! | ------ | ----------------- | ---------------------------------------- |
//! | `0x01` | `terminal.output` | raw bytes (hot path, no structured decode) |
//! | `0x02` | `terminal.input`  | raw bytes (hot path)                     |
//! | `0x10` | `control`         | MessagePack object with a `t` discriminator |
//!
//! Output and input stay bare binary types so the interactive path is
//! allocation-free. The control vocabulary is deliberately small — eight
//! messages — because each one multiplies across the Rust and TypeScript
//! fixture suites.
//!
//! # Scaffold status
//!
//! Types are declared as milestone M1 lands them. See `planning/IMPLEMENTATION_PLAN.md`
//! (M1, "Worker" item 4) and `fixtures/README.md` for the cross-language
//! conformance contract this crate is held to.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod control;
pub mod frame;
mod msgpack;

/// The protocol version this build speaks.
///
/// Version negotiation rides the `attach` control message. An unsupported
/// version produces an `error` frame and a close — never a guess.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum accepted payload length for a single frame, in bytes.
///
/// A peer announcing a longer frame is malformed or hostile; the codec rejects
/// it rather than allocating on its word.
pub const MAX_FRAME_PAYLOAD: usize = 4 * 1024 * 1024;

/// Anything that can go wrong turning bytes into a message.
///
/// The two layers fail for unrelated reasons — a frame length is wrong, or a
/// control payload is — but a caller handles them identically: name the error
/// to the peer, then close. This type exists so that call site can be written
/// once.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The framing layer rejected the bytes.
    #[error(transparent)]
    Frame(#[from] frame::FrameError),
    /// The frame was well-formed but its control payload was not.
    #[error(transparent)]
    Control(#[from] control::ControlError),
}

impl ProtocolError {
    /// A stable machine-readable name for this error.
    ///
    /// The names are flat across both layers: a client branching on an error
    /// does not care which layer produced it, and `fixtures/protocol/` names
    /// rejection cases with exactly these strings so the Rust and TypeScript
    /// codecs must agree on *which* error the bytes are — not merely that they
    /// are one.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Frame(e) => e.code(),
            Self::Control(e) => e.code(),
        }
    }

    /// Whether encountering this error must close the connection.
    ///
    /// True for everything except a frame that is merely incomplete, which
    /// mid-stream means "read more" and only becomes fatal at end of stream.
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Frame(e) => e.is_fatal(),
            Self::Control(e) => e.is_fatal(),
        }
    }
}
