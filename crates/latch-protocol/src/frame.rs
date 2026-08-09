//! Frame type tags and the length-prefixed codec.
//!
//! ```text
//! u8   type
//! u32  length (big-endian)
//! [u8] payload
//! ```
//!
//! The codec is transport-agnostic by construction: it reads and writes byte
//! slices and never touches a socket. The same functions serve the Unix socket
//! at M1 and the WebSocket at M3 without modification, and the way that stays
//! true is that they never learn what they are riding on.
//!
//! # Two decode entry points, deliberately
//!
//! [`decode_frame`] and [`decode_exact`] decode from a buffer that is already
//! known to be complete — a fixture, a WebSocket message, a test. A short
//! buffer is an error there, because nothing more is coming.
//!
//! [`FrameDecoder`] decodes from a stream, where a read boundary falls wherever
//! it falls. A short buffer is `Ok(None)` there, because more is coming — until
//! [`FrameDecoder::finish`] says it is not.
//!
//! Neither will partially accept. A decoder that misreads a length does not
//! drop one frame; it desynchronizes every frame after it, and the damage
//! surfaces somewhere unrelated much later. So every error other than "read
//! more" is terminal for the connection.
//!
//! # Conformance
//!
//! `fixtures/protocol/` defines this codec. Both directions are asserted for
//! every case there, so the encoding is pinned rather than merely valid.

use crate::MAX_FRAME_PAYLOAD;

/// Bytes of frame header preceding every payload: one tag byte plus a
/// big-endian `u32` length.
pub const HEADER_LEN: usize = 5;

/// Wire tag for a frame.
///
/// The numeric values are protocol constants and may not be reordered or
/// reassigned; `fixtures/protocol/` asserts them from the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameType {
    /// `0x01` — raw PTY output bytes, forwarded without structured decoding.
    TerminalOutput = 0x01,
    /// `0x02` — raw client input bytes, forwarded without structured decoding.
    TerminalInput = 0x02,
    /// `0x10` — a MessagePack control object with a `t` discriminator.
    Control = 0x10,
}

/// One decoded frame, borrowing its payload from the caller's buffer.
///
/// Borrowing rather than owning is the point on the hot path: every keystroke
/// and every byte of agent output passes through here, and an allocation per
/// frame is the difference between a session that feels native and one that
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Which of the three frame types this is.
    pub frame_type: FrameType,
    /// The payload bytes, excluding the header.
    pub payload: &'a [u8],
}

/// Errors produced while encoding or decoding a frame.
///
/// Every variant except [`Incomplete`] is fatal: the connection closes rather
/// than resynchronizes, because after a length the decoder could not trust,
/// every subsequent byte offset is a guess.
///
/// [`Incomplete`]: FrameError::Incomplete
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    /// The tag byte does not name a known frame type.
    #[error("unknown frame type: 0x{0:02x}")]
    UnknownType(u8),
    /// The announced payload length exceeds the enforced limit.
    ///
    /// Produced before allocating, never after: the limit exists precisely so a
    /// peer's word about a length is not turned into memory on trust.
    #[error("frame payload of {len} bytes exceeds the {max}-byte limit")]
    Oversized {
        /// Length the peer announced.
        len: usize,
        /// Limit this build enforces.
        max: usize,
    },
    /// The buffer ended before a whole frame was available.
    ///
    /// Mid-stream this only means "read more". At end of stream it means the
    /// peer truncated a frame, and the frame is dropped rather than partially
    /// accepted.
    #[error("incomplete frame: have {have} bytes, need {need}")]
    Incomplete {
        /// Bytes available.
        have: usize,
        /// Bytes the frame requires in total. While only part of the header has
        /// arrived the payload length is not yet known, so this is
        /// [`HEADER_LEN`].
        need: usize,
    },
    /// Bytes remained after a frame that was required to be the whole buffer.
    ///
    /// Only [`decode_exact`] produces this. In a stream, bytes after a frame
    /// are the next frame.
    #[error("{len} trailing bytes after a complete frame")]
    TrailingBytes {
        /// Number of bytes left over.
        len: usize,
    },
}

impl FrameError {
    /// A stable machine-readable name for this error.
    ///
    /// `fixtures/protocol/` names errors with these strings, so the Rust and
    /// TypeScript codecs are held to producing the *same* error for the same
    /// bytes — not merely to failing somehow.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownType(_) => "unknown_type",
            Self::Oversized { .. } => "oversized",
            Self::Incomplete { .. } => "incomplete",
            Self::TrailingBytes { .. } => "trailing_bytes",
        }
    }

    /// Whether encountering this error must close the connection.
    ///
    /// True for everything but [`Incomplete`], which mid-stream means only that
    /// the next read has not happened yet. [`FrameDecoder::finish`] is what
    /// turns a still-incomplete buffer at end of stream into a fatal one.
    ///
    /// [`Incomplete`]: FrameError::Incomplete
    pub fn is_fatal(&self) -> bool {
        !matches!(self, Self::Incomplete { .. })
    }
}

impl FrameType {
    /// Returns the frame type for a wire tag byte.
    pub fn from_tag(tag: u8) -> Result<Self, FrameError> {
        match tag {
            0x01 => Ok(Self::TerminalOutput),
            0x02 => Ok(Self::TerminalInput),
            0x10 => Ok(Self::Control),
            other => Err(FrameError::UnknownType(other)),
        }
    }

    /// Returns the wire tag byte for this frame type.
    pub fn tag(self) -> u8 {
        self as u8
    }
}

/// Returns the encoded size of a frame carrying `payload_len` payload bytes.
pub fn encoded_len(payload_len: usize) -> usize {
    HEADER_LEN + payload_len
}

/// Appends an encoded frame to `out`.
///
/// This is the form the hot path uses: an attachment owns one output buffer for
/// its lifetime and appends every frame into it, so steady-state framing
/// allocates nothing.
///
/// Fails with [`FrameError::Oversized`] if the payload exceeds
/// [`crate::MAX_FRAME_PAYLOAD`], leaving `out` unchanged.
pub fn encode_into(
    frame_type: FrameType,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), FrameError> {
    // Validated before a single byte is written, so a rejected frame cannot
    // leave half a header in a buffer the caller is going to keep using.
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(FrameError::Oversized {
            len: payload.len(),
            max: MAX_FRAME_PAYLOAD,
        });
    }

    out.reserve(encoded_len(payload.len()));
    out.push(frame_type.tag());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Encodes one frame into a fresh buffer.
///
/// Convenience over [`encode_into`] for control frames and tests. The hot path
/// should not use it.
pub fn encode(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(encoded_len(payload.len()));
    encode_into(frame_type, payload, &mut out)?;
    Ok(out)
}

/// Reads the header at the front of `buf` and locates the frame it announces.
///
/// Returns the frame type, the payload length, and the total encoded length.
/// This is the whole of the decoder's logic; the three public entry points
/// differ only in what they do with [`FrameError::Incomplete`] and with bytes
/// left over afterwards.
///
/// The order of the checks is deliberate. The tag is validated as soon as one
/// byte exists, before completeness: bytes that can never form a valid frame
/// must not be waited on. The length is checked against the limit before it is
/// compared with what has arrived, so an announced size is refused without ever
/// being turned into an allocation.
fn parse_header(buf: &[u8], max_payload: usize) -> Result<(FrameType, usize, usize), FrameError> {
    let Some(&tag) = buf.first() else {
        return Err(FrameError::Incomplete {
            have: 0,
            need: HEADER_LEN,
        });
    };
    let frame_type = FrameType::from_tag(tag)?;

    if buf.len() < HEADER_LEN {
        return Err(FrameError::Incomplete {
            have: buf.len(),
            need: HEADER_LEN,
        });
    }

    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len > max_payload {
        return Err(FrameError::Oversized {
            len,
            max: max_payload,
        });
    }

    let total = HEADER_LEN + len;
    if buf.len() < total {
        return Err(FrameError::Incomplete {
            have: buf.len(),
            need: total,
        });
    }

    Ok((frame_type, len, total))
}

/// Decodes the frame at the front of `buf`, returning it and the number of
/// bytes it consumed.
///
/// For buffers already known to be complete. A short buffer is
/// [`FrameError::Incomplete`] rather than a request for more; use
/// [`FrameDecoder`] when reading from a stream.
pub fn decode_frame(buf: &[u8]) -> Result<(Frame<'_>, usize), FrameError> {
    let (frame_type, payload_len, total) = parse_header(buf, MAX_FRAME_PAYLOAD)?;
    Ok((
        Frame {
            frame_type,
            payload: &buf[HEADER_LEN..HEADER_LEN + payload_len],
        },
        total,
    ))
}

/// Decodes `buf` as exactly one frame, rejecting anything left over.
///
/// Trailing bytes are [`FrameError::TrailingBytes`], not a silent partial
/// accept. A single WebSocket message and a single fixture case both need this:
/// the bytes are one frame, or they are an error.
pub fn decode_exact(buf: &[u8]) -> Result<Frame<'_>, FrameError> {
    let (frame, consumed) = decode_frame(buf)?;
    if consumed < buf.len() {
        return Err(FrameError::TrailingBytes {
            len: buf.len() - consumed,
        });
    }
    Ok(frame)
}

/// A streaming frame decoder that accepts partial buffers.
///
/// A socket read boundary falls wherever it falls — mid-header, mid-payload, or
/// across several frames at once — so the decoder buffers what it cannot yet
/// use and hands back whole frames as they complete.
///
/// Once a fatal error is produced the decoder is *poisoned*: it returns that
/// same error forever rather than attempting to resynchronize. After a length
/// it could not trust there is no offset to resume from that is better than a
/// guess, and a guess here corrupts the whole stream rather than one frame.
#[derive(Debug)]
pub struct FrameDecoder {
    /// Bytes fed but not yet consumed by a completed frame.
    buf: Vec<u8>,
    /// Read offset into `buf`; bytes before it belong to frames already
    /// returned and are reclaimed on the next [`FrameDecoder::feed`].
    cursor: usize,
    /// Payload limit enforced against a peer's announced length.
    max_payload: usize,
    /// Set once a fatal error is produced; replayed on every later call.
    poison: Option<FrameError>,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// Creates a decoder enforcing [`crate::MAX_FRAME_PAYLOAD`].
    pub fn new() -> Self {
        Self::with_max_payload(MAX_FRAME_PAYLOAD)
    }

    /// Creates a decoder enforcing a lower payload limit than the protocol
    /// default.
    ///
    /// Useful for a constrained peer, and for tests that exercise the oversize
    /// path without moving four megabytes around. The limit is clamped to
    /// [`crate::MAX_FRAME_PAYLOAD`]: a local build may tighten a protocol
    /// constant but not raise it.
    pub fn with_max_payload(max_payload: usize) -> Self {
        Self {
            buf: Vec::new(),
            cursor: 0,
            max_payload: max_payload.min(MAX_FRAME_PAYLOAD),
            poison: None,
        }
    }

    /// The payload limit this decoder enforces.
    pub fn max_payload(&self) -> usize {
        self.max_payload
    }

    /// Adds freshly read bytes to the decoder's buffer.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Reclaiming here rather than in `next_frame` is what lets a returned
        // frame borrow the buffer: the bytes under it stay put until the caller
        // asks for more, which it cannot do while still holding the frame.
        if self.cursor > 0 {
            self.buf.drain(..self.cursor);
            self.cursor = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next whole frame, or `None` if more bytes are needed.
    ///
    /// The frame borrows the decoder's internal buffer, so it must be consumed
    /// or copied before the next [`feed`] call.
    ///
    /// [`feed`]: FrameDecoder::feed
    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>, FrameError> {
        if let Some(err) = &self.poison {
            return Err(err.clone());
        }

        let (frame_type, payload_len, total) =
            match parse_header(&self.buf[self.cursor..], self.max_payload) {
                Ok(parsed) => parsed,
                // Mid-stream this is not an error at all: the next read has not
                // happened yet. Only `finish` turns it into one.
                Err(FrameError::Incomplete { .. }) => return Ok(None),
                Err(err) => {
                    self.poison = Some(err.clone());
                    return Err(err);
                }
            };

        let start = self.cursor + HEADER_LEN;
        self.cursor += total;
        Ok(Some(Frame {
            frame_type,
            payload: &self.buf[start..start + payload_len],
        }))
    }

    /// Bytes buffered but not yet formed into a frame.
    pub fn buffered_len(&self) -> usize {
        self.buf.len() - self.cursor
    }

    /// Whether a fatal error has been produced and the connection must close.
    pub fn is_poisoned(&self) -> bool {
        self.poison.is_some()
    }

    /// Asserts the stream ended on a frame boundary.
    ///
    /// A partial frame at end of stream is [`FrameError::Incomplete`] — the
    /// truncated frame is dropped, never partially accepted.
    pub fn finish(&self) -> Result<(), FrameError> {
        if let Some(err) = &self.poison {
            return Err(err.clone());
        }

        let rest = &self.buf[self.cursor..];
        if rest.is_empty() {
            return Ok(());
        }

        // Whole frames the caller has not drained yet are not a truncation: the
        // stream still ended on a frame boundary. Anything else is a frame the
        // peer began and did not finish, and it is dropped rather than guessed
        // at.
        parse_header(rest, self.max_payload).map(|_| ())
    }
}
