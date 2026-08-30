//! The wire between `latchd` and its clients.
//!
//! One unix socket per session carries two kinds of connection:
//!
//! - A **control** connection exchanges length-prefixed JSON frames: one
//!   [`Request`] in, one [`Response`] out, repeated. A connection that sends
//!   [`Request::Subscribe`] then receives [`Event`] frames until it closes.
//! - A **surface** connection sends exactly one [`Request::Attach`] frame,
//!   receives one [`Response`] frame (`snapshot_len` bytes of current-frame
//!   escape stream follow it), and is then raw bytes in both directions until
//!   one side closes. Nothing on the live path is framed, parsed, or copied
//!   more than once.
//!
//! The frame is `u32` big-endian length followed by that many bytes of JSON.
//! Frames are bounded by [`MAX_FRAME`] so a hostile or confused peer cannot
//! make the daemon allocate without limit.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Kernel protocol version. A client and daemon that disagree refuse to talk.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest control frame either side accepts.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Largest column or row count a surface or resize may ask for.
///
/// The screen model keeps every row at full width, and scrollback keeps
/// thousands of rows, so a dimension is a memory multiplier: a client is
/// clamped (attach) or refused (resize) rather than allowed to size the
/// session into the gigabytes.
pub const MAX_DIMENSION: u16 = 2048;

/// Longest window title the daemon reports, in characters.
pub const MAX_TITLE_CHARS: usize = 512;

/// Why a surface was released, as the daemon records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    /// The client closed its own connection.
    Normal,
    /// Another attach took the surface.
    Stolen,
    /// The client could not drain output fast enough and was evicted.
    SlowClient,
    /// The child exited while this client held the surface.
    SessionExited,
}

/// Session lifecycle as the daemon reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The child is alive.
    Running,
    /// The child exited; the last frame is retained.
    Exited,
}

/// How the child ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// Exit status, when the child exited normally.
    pub status: Option<i32>,
    /// Terminating signal number, when it was signalled.
    pub signal: Option<i32>,
    /// Unix seconds when the daemon observed the exit.
    pub exited_at: u64,
}

/// Snapshot rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    /// Plain text, one line per row, trailing blanks trimmed.
    #[default]
    Text,
    /// Text with SGR attributes, one line per row.
    Styled,
    /// The self-contained escape stream that repaints the screen.
    Escape,
    /// Structured cells, cursor, and modes.
    Json,
}

/// A control request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Take the surface. Steals from any current holder.
    Attach {
        /// Client terminal width.
        cols: u16,
        /// Client terminal height.
        rows: u16,
        /// Protocol version the client speaks.
        protocol: u32,
    },
    /// Live facts.
    Stat,
    /// Write raw bytes to the child's input.
    Write {
        /// Bytes to write.
        bytes: Vec<u8>,
    },
    /// Press named keys (tmux `send-keys` names; unknown names are literal).
    Key {
        /// Key names, sent in order.
        keys: Vec<String>,
    },
    /// Paste text, bracketed when the child asked for bracketed paste.
    Paste {
        /// Text to paste.
        text: String,
    },
    /// Paste text and press Enter as one operation.
    Submit {
        /// Message text.
        text: String,
    },
    /// The current frame.
    Snapshot {
        /// Rendering.
        #[serde(default)]
        format: SnapshotFormat,
        /// Primary-screen scrollback lines to include above the screen
        /// (text and styled formats only).
        #[serde(default)]
        scrollback_lines: u32,
    },
    /// Primary-screen scrollback.
    History {
        /// Most recent lines to return.
        max: u32,
    },
    /// Change the child's terminal size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
        /// Stop attaches from resizing the session to their own geometry.
        #[serde(default)]
        pin: bool,
    },
    /// Signal the child's process group.
    Signal {
        /// Signal number.
        signal: i32,
    },
    /// End the daemon. The child, if alive, is killed.
    Kill,
    /// Block until a surface is attached, or until `timeout_ms` passes.
    AwaitSurface {
        /// Wait ceiling in milliseconds.
        timeout_ms: u64,
    },
    /// Why a surface connection ended.
    ReleaseReason {
        /// Surface id from the attach response.
        surface: u64,
    },
    /// Turn this connection into an event stream.
    Subscribe,
}

/// Live facts about a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    /// Session id.
    pub id: String,
    /// Protocol version the daemon speaks.
    pub protocol: u32,
    /// Daemon pid.
    pub daemon_pid: i32,
    /// Child pid.
    pub child_pid: i32,
    /// Lifecycle.
    pub state: State,
    /// Terminal width.
    pub cols: u16,
    /// Terminal height.
    pub rows: u16,
    /// Whether attaches resize the session.
    pub pinned: bool,
    /// Whether a surface currently holds the session.
    pub attached: bool,
    /// Unix seconds of the last child output or input.
    pub activity: u64,
    /// Exit facts once the child has ended.
    pub exit: Option<Exit>,
    /// Whether the alternate screen is active.
    pub alternate_screen: bool,
    /// Window title set by the child, if any.
    pub title: Option<String>,
    /// Bytes read from the child PTY since daemon start.
    #[serde(default)]
    pub bytes_from_child: u64,
    /// Live bytes successfully written to attached surfaces (snapshots excluded).
    #[serde(default)]
    pub bytes_to_surfaces: u64,
    /// Bytes waiting for the off-path parser now.
    #[serde(default)]
    pub parser_backlog_bytes: u64,
    /// Largest observed parser backlog in bytes.
    #[serde(default)]
    pub parser_backlog_peak_bytes: u64,
    /// Bytes queued for the current surface now.
    #[serde(default)]
    pub surface_queue_bytes: u64,
    /// Largest observed queue for any surface.
    #[serde(default)]
    pub surface_queue_peak_bytes: u64,
    /// Successful surface attachments since daemon start.
    #[serde(default)]
    pub surface_attaches: u64,
    /// Surface attachments that stole an existing holder.
    #[serde(default)]
    pub surface_steals: u64,
    /// Surfaces evicted for exceeding the queue bound.
    #[serde(default)]
    pub slow_client_evictions: u64,
    /// Rejected or malformed control-plane requests.
    #[serde(default)]
    pub control_failures: u64,
    /// Times the screen model panicked on child output and was rebuilt.
    #[serde(default)]
    pub parser_resets: u64,
    /// Event subscribers dropped for not draining their stream.
    #[serde(default)]
    pub subscriber_evictions: u64,
}

/// A control response: `ok` plus either an `error` or the reply fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Whether the request succeeded.
    pub ok: bool,
    /// Human-readable reason when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    /// Reply fields when `ok` is true.
    #[serde(flatten)]
    pub reply: Reply,
}

impl Response {
    /// A successful response carrying `reply`.
    pub fn ok(reply: Reply) -> Self {
        Self {
            ok: true,
            error: None,
            reply,
        }
    }

    /// An empty successful response.
    pub fn done() -> Self {
        Self::ok(Reply::default())
    }

    /// A failed response.
    pub fn err(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            reply: Reply::default(),
        }
    }

    /// Converts into a result.
    pub fn into_result(self) -> Result<Reply, String> {
        if self.ok {
            Ok(self.reply)
        } else {
            Err(self
                .error
                .unwrap_or_else(|| "kernel reported an unspecified error".into()))
        }
    }
}

/// The payload of a successful response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Reply {
    /// Surface id (attach).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub surface: Option<u64>,
    /// Bytes of escape stream following this frame (attach).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub snapshot_len: Option<usize>,
    /// Live facts (stat).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stat: Option<Stat>,
    /// Rendered text (snapshot text/styled).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub text: Option<String>,
    /// Escape stream (snapshot escape).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bytes: Option<Vec<u8>>,
    /// Structured screen (snapshot json).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub screen: Option<serde_json::Value>,
    /// Scrollback lines, oldest first (history).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub lines: Option<Vec<String>>,
    /// Lines dropped from the ring over the session's life (history).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dropped: Option<u64>,
    /// Whether a surface was attached (await_surface).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attached: Option<bool>,
    /// Release reason (release_reason).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<ReleaseReason>,
}

/// A pushed event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// The child ended.
    ChildExited {
        /// Exit facts.
        exit: Exit,
    },
    /// A surface took the session.
    SurfaceAttached {
        /// Surface id.
        surface: u64,
    },
    /// A surface was released.
    SurfaceDetached {
        /// Surface id.
        surface: u64,
        /// Why.
        reason: ReleaseReason,
    },
    /// The window title changed.
    TitleChanged {
        /// New title.
        title: Option<String>,
    },
    /// The alternate screen was entered or left.
    AltScreen {
        /// Whether it is now active.
        active: bool,
    },
    /// No output for `ms` milliseconds after the last output.
    OutputQuiet {
        /// Quiet duration.
        ms: u64,
    },
}

/// Writes one frame.
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds the protocol maximum",
        ));
    }
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    writer.write_all(&out)?;
    writer.flush()
}

/// Reads one frame, or `None` at a clean end of stream.
pub fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds the protocol maximum",
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &Request::Attach {
                cols: 80,
                rows: 24,
                protocol: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        write_frame(&mut buffer, &Response::err("no")).unwrap();
        let mut cursor = std::io::Cursor::new(buffer);
        let first: Request = read_frame(&mut cursor).unwrap().unwrap();
        assert!(matches!(
            first,
            Request::Attach {
                cols: 80,
                rows: 24,
                ..
            }
        ));
        let second: Response = read_frame(&mut cursor).unwrap().unwrap();
        assert!(!second.ok);
        let end: Option<Request> = read_frame(&mut cursor).unwrap();
        assert!(end.is_none());
    }

    #[test]
    fn oversized_frames_are_refused_before_allocation() {
        let mut header = ((MAX_FRAME as u32) + 1).to_be_bytes().to_vec();
        header.extend_from_slice(b"{}");
        let mut cursor = std::io::Cursor::new(header);
        let error = read_frame::<Request>(&mut cursor).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let huge = Request::Submit {
            text: "x".repeat(MAX_FRAME + 1),
        };
        let error = write_frame(&mut Vec::new(), &huge).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        let body = b"{\"op\":\"nope\"}";
        let mut framed = (body.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(body);
        let mut cursor = std::io::Cursor::new(framed);
        let error = read_frame::<Request>(&mut cursor).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn responses_carry_a_boolean_ok_and_flattened_reply() {
        let ok = serde_json::to_string(&Response::done()).unwrap();
        assert_eq!(ok, r#"{"ok":true}"#);
        let err = serde_json::to_string(&Response::err("no")).unwrap();
        assert_eq!(err, r#"{"ok":false,"error":"no"}"#);
        let request = serde_json::to_string(&Request::Stat).unwrap();
        assert_eq!(request, r#"{"op":"stat"}"#);
    }
}
