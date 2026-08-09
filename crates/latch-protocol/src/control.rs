//! The control-message vocabulary.
//!
//! Eight messages, deliberately. Every addition multiplies across the Rust and
//! TypeScript fixture suites, so a new message needs an argument, not just a
//! use case.
//!
//! ```text
//! attach          { protocol, mode: watch|control, steal, client: {kind,name}, size }
//! attached        { protocol, session, controller, attachments }
//! resize          { cols, rows }
//! control.request { steal }
//! control.state   { controller_id, controller_label }        // broadcast
//! session.update  { state?, attachments?, title? }           // presence + state, merged
//! session.exited  { code, signal, at }
//! error           { code, message }
//! ```
//!
//! **The screen snapshot is not a message.** After `attached`, the worker sends
//! the serialized screen as ordinary [`FrameType::TerminalOutput`] frames and
//! then continues with live output. Reconnecting is therefore just attaching:
//! no replay round trip, no second code path.
//!
//! # On the wire
//!
//! A control payload is a MessagePack **map with string keys**. The `t`
//! discriminator comes first, then the message's own fields in the order listed
//! above. That order is part of the contract rather than an implementation
//! detail: `fixtures/protocol/` asserts re-encoded bytes against recorded bytes,
//! which is the only way a Rust encoder and a TypeScript encoder can be shown
//! to agree rather than merely to interoperate with themselves.
//!
//! A payload with no `t`, or a `t` this build does not know, is an error and a
//! close. Guessing at an unknown message would mean acting on a message whose
//! meaning is by definition unknown.
//!
//! [`FrameType::TerminalOutput`]: crate::frame::FrameType::TerminalOutput

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::msgpack::{self, Value};
use crate::PROTOCOL_VERSION;

/// A terminal size in character cells.
///
/// Declared here rather than borrowed from `latch-term` because both crates are
/// leaves: neither may depend on the other (see `docs/ARCHITECTURE_RULES.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// Whether an attachment may write to the session.
///
/// At most one attachment holds [`Control`] at a time. Control is released by
/// socket close, not by timer expiry — locally the operating system reports
/// departure precisely, so a lease timer could only be wrong relative to it.
/// Timed leases arrive in M4, where a remote connection can hang without
/// closing (decision D5).
///
/// [`Control`]: AttachMode::Control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachMode {
    /// Receives output; never resizes the session and never writes input.
    Watch,
    /// Receives output, writes input, and sets the session size (decision D4).
    Control,
}

impl AttachMode {
    /// The string this mode is written as on the wire.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Control => "control",
        }
    }

    /// Parses a wire string, rejecting anything else.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "watch" => Some(Self::Watch),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
}

/// Session lifecycle state as the worker reports it.
///
/// State is *derived*, never stored (see `docs/ARCHITECTURE_RULES.md`): the
/// worker answers from what is true right now, so there is no registry to
/// diverge from reality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The worker is spawning the child process.
    Creating,
    /// The child is running.
    Running,
    /// A stop was requested and the child is winding down.
    Stopping,
    /// The child has exited; `exit.json` is present.
    Exited,
    /// Neither a live socket nor an `exit.json` — the worker died without
    /// recording an exit.
    Lost,
}

impl SessionState {
    /// The string this state is written as on the wire.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Exited => "exited",
            Self::Lost => "lost",
        }
    }

    /// Parses a wire string, rejecting anything else.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "creating" => Some(Self::Creating),
            "running" => Some(Self::Running),
            "stopping" => Some(Self::Stopping),
            "exited" => Some(Self::Exited),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }
}

/// Who is attaching, for display in other clients' presence lists.
///
/// `kind` is an open string rather than a closed enum on purpose: an M4 mobile
/// client must be able to identify itself to an M1 worker without that worker
/// treating it as a malformed message. Both fields are display metadata and are
/// sanitized to printable characters at ingest, at the boundary where they
/// arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// Broad client category — `cli`, `desktop`, `web`, `mobile`.
    pub kind: String,
    /// Human-readable client name, e.g. `iterm`.
    pub name: String,
}

/// One attachment in the session's presence list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Worker-assigned identifier, stable for the life of the attachment.
    pub id: String,
    /// Whether this attachment may write.
    pub mode: AttachMode,
    /// Who it says it is.
    pub client: ClientInfo,
}

/// The session as a freshly attached client first sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// Session identifier, e.g. `ses_01J...`.
    pub id: String,
    /// Short auto-derived or user-given name.
    pub name: String,
    /// Optional human title; `None` when never set.
    pub title: Option<String>,
    /// Current derived state.
    pub state: SessionState,
    /// The session's authoritative size — the current controller's size
    /// (decision D4).
    pub size: Size,
}

/// A control message.
///
/// Variant order matches the vocabulary listing in the module documentation and
/// in `planning/IMPLEMENTATION_PLAN.md`. Field order within each variant is the
/// wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    /// `attach` — a client joins the session and negotiates the version.
    ///
    /// There is no separate hello: clients connect to a per-session socket, so
    /// there is no session to negotiate. An unsupported `protocol` produces
    /// [`ControlMessage::Error`] and a close, never a guess.
    Attach {
        /// Protocol version the client speaks.
        protocol: u32,
        /// Whether the client wants to write.
        mode: AttachMode,
        /// Take control from the current controller if one holds it. Without
        /// this, requesting control while it is held returns `control_busy`.
        steal: bool,
        /// Who is attaching.
        client: ClientInfo,
        /// The client's terminal size. Honored as the session size only when
        /// the client holds control.
        size: Size,
    },
    /// `attached` — the worker accepts, and describes the session.
    ///
    /// The screen snapshot follows as ordinary `terminal.output` frames, not as
    /// part of this message.
    Attached {
        /// Protocol version the worker will speak for this connection.
        protocol: u32,
        /// The session's identity and current state.
        session: SessionInfo,
        /// Attachment id currently holding control, or `None` if nobody does.
        controller: Option<String>,
        /// Everyone currently attached, including the new attachment.
        attachments: Vec<Attachment>,
    },
    /// `resize` — the controller's terminal changed size.
    ///
    /// From a watcher this is ignored: watchers never resize the session.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// `control.request` — an attached watcher asks to take input control.
    ControlRequest {
        /// Demote the current controller rather than failing with
        /// `control_busy`.
        steal: bool,
    },
    /// `control.state` — broadcast whenever control moves.
    ///
    /// Sent to every attachment so a transfer is visible to the client that
    /// lost control as well as the one that gained it. Both fields are `None`
    /// when no attachment holds control.
    ControlState {
        /// Attachment id of the controller.
        controller_id: Option<String>,
        /// Display label for the controller.
        controller_label: Option<String>,
    },
    /// `session.update` — a **merge** into the client's session view.
    ///
    /// Absent and null mean different things and must not be conflated. An
    /// absent field leaves the client's current value alone; a present field
    /// replaces it, and `title: null` clears the title. That distinction is why
    /// `title` is doubly optional: outer `None` is "not in this message", inner
    /// `None` is "explicitly cleared".
    SessionUpdate {
        /// New lifecycle state, if it changed.
        state: Option<SessionState>,
        /// The complete new presence list, if it changed. A present list
        /// replaces the client's list entirely; it is not a delta.
        attachments: Option<Vec<Attachment>>,
        /// `None` — unchanged. `Some(None)` — cleared. `Some(Some(s))` — set.
        title: Option<Option<String>>,
    },
    /// `session.exited` — the child process ended.
    SessionExited {
        /// Exit status, or `None` when the child was signalled.
        code: Option<i32>,
        /// Signal name, or `None` when the child exited normally.
        signal: Option<String>,
        /// RFC 3339 timestamp, matching `exit.json`'s `exited_at`.
        at: String,
    },
    /// `error` — a named failure, always followed by a close.
    ///
    /// `code` is a stable machine-readable name a client may branch on
    /// (`control_busy`, `unsupported_protocol`); `message` is for humans and
    /// may change.
    Error {
        /// Stable machine-readable error name.
        code: String,
        /// Human-readable detail.
        message: String,
    },
}

impl ControlMessage {
    /// The `t` discriminator this message is written with.
    pub fn discriminator(&self) -> &'static str {
        match self {
            Self::Attach { .. } => "attach",
            Self::Attached { .. } => "attached",
            Self::Resize { .. } => "resize",
            Self::ControlRequest { .. } => "control.request",
            Self::ControlState { .. } => "control.state",
            Self::SessionUpdate { .. } => "session.update",
            Self::SessionExited { .. } => "session.exited",
            Self::Error { .. } => "error",
        }
    }

    /// Every discriminator this build knows, in vocabulary order.
    ///
    /// Exposed so a test can assert the vocabulary has exactly eight members —
    /// the count is a design constraint, so it should fail loudly rather than
    /// drift.
    pub const DISCRIMINATORS: [&'static str; 8] = [
        "attach",
        "attached",
        "resize",
        "control.request",
        "control.state",
        "session.update",
        "session.exited",
        "error",
    ];
}

/// Errors produced while decoding a control payload.
///
/// All of them are fatal. A control payload the decoder cannot fully understand
/// is not partially applied, because the alternative is acting on a message
/// whose meaning is unknown — and the messages in this vocabulary move input
/// control and session size.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ControlError {
    /// The payload is not well-formed MessagePack, or not a map with string
    /// keys.
    #[error("malformed MessagePack control payload: {detail}")]
    MalformedPayload {
        /// Decoder detail. Not part of the contract — match on
        /// [`ControlError::code`] instead.
        detail: String,
    },
    /// The payload is a valid map but carries no `t` discriminator.
    #[error("control payload has no `t` discriminator")]
    MissingDiscriminator,
    /// The `t` value names no message this build knows.
    #[error("unknown control message: {t}")]
    UnknownMessage {
        /// The discriminator as received.
        t: String,
    },
    /// A known message is missing a required field, or a field has the wrong
    /// type or an out-of-range value.
    #[error("control message `{message}` has an invalid `{field}` field: {detail}")]
    InvalidField {
        /// Discriminator of the message being decoded.
        message: String,
        /// Field at fault.
        field: String,
        /// What was wrong with it.
        detail: String,
    },
    /// An `attach` named a protocol version this build does not speak.
    ///
    /// Version negotiation rides `attach` and has exactly one outcome when it
    /// fails: an `error` frame and a close.
    #[error("unsupported protocol version {requested}; this build speaks {supported}")]
    UnsupportedProtocol {
        /// Version the peer asked for.
        requested: u32,
        /// Version this build speaks.
        supported: u32,
    },
}

impl ControlError {
    /// A stable machine-readable name for this error.
    ///
    /// These strings appear in `fixtures/protocol/` rejection cases and in the
    /// `code` field of the [`ControlMessage::Error`] the peer is sent before
    /// the close.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MalformedPayload { .. } => "malformed_payload",
            Self::MissingDiscriminator => "missing_discriminator",
            Self::UnknownMessage { .. } => "unknown_message",
            Self::InvalidField { .. } => "invalid_field",
            Self::UnsupportedProtocol { .. } => "unsupported_protocol",
        }
    }

    /// Whether encountering this error must close the connection.
    ///
    /// Always true. It is a method rather than a constant so call sites read
    /// the same for frame and control errors, and so a future non-fatal control
    /// error has somewhere to be expressed.
    pub fn is_fatal(&self) -> bool {
        true
    }
}

/// Encodes a control message into a MessagePack payload.
///
/// The result is the payload of a [`FrameType::Control`] frame, not a whole
/// frame; wrap it with [`crate::frame::encode`].
///
/// [`FrameType::Control`]: crate::frame::FrameType::Control
pub fn encode(message: &ControlMessage) -> Vec<u8> {
    let mut payload = Vec::new();
    Wire(message)
        .serialize(&mut rmp_serde::Serializer::new(&mut payload))
        .expect("MessagePack serialization into a Vec has no failure mode");
    payload
}

// ------------------------------------------------------------------ encoding
//
// The wire form is written through explicit `serialize_map` calls rather than
// through derived impls on the public types. Field order and the exact set of
// keys present are the contract — `fixtures/protocol/` compares re-encoded
// bytes against recorded bytes — and a derive would leave both to whatever the
// struct definition happens to look like. Writing them out means changing the
// wire form requires changing this file, which is where a reviewer looks.
//
// The values themselves go through `rmp-serde`, whose writers already choose
// the smallest representation for every integer and string.

struct Wire<'a>(&'a ControlMessage);

impl Serialize for Wire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            ControlMessage::Attach {
                protocol,
                mode,
                steal,
                client,
                size,
            } => {
                let mut map = serializer.serialize_map(Some(6))?;
                map.serialize_entry("t", "attach")?;
                map.serialize_entry("protocol", protocol)?;
                map.serialize_entry("mode", mode.as_wire())?;
                map.serialize_entry("steal", steal)?;
                map.serialize_entry("client", &WireClient(client))?;
                map.serialize_entry("size", &WireSize(size))?;
                map.end()
            }
            ControlMessage::Attached {
                protocol,
                session,
                controller,
                attachments,
            } => {
                let mut map = serializer.serialize_map(Some(5))?;
                map.serialize_entry("t", "attached")?;
                map.serialize_entry("protocol", protocol)?;
                map.serialize_entry("session", &WireSession(session))?;
                map.serialize_entry("controller", controller)?;
                map.serialize_entry("attachments", &WireAttachments(attachments))?;
                map.end()
            }
            ControlMessage::Resize { cols, rows } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("t", "resize")?;
                map.serialize_entry("cols", cols)?;
                map.serialize_entry("rows", rows)?;
                map.end()
            }
            ControlMessage::ControlRequest { steal } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("t", "control.request")?;
                map.serialize_entry("steal", steal)?;
                map.end()
            }
            ControlMessage::ControlState {
                controller_id,
                controller_label,
            } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("t", "control.state")?;
                map.serialize_entry("controller_id", controller_id)?;
                map.serialize_entry("controller_label", controller_label)?;
                map.end()
            }
            // The only message whose key *set* varies. An absent key means
            // "leave the client's value alone", so emitting a null for an
            // untouched field would turn a no-op merge into one that wipes the
            // title.
            ControlMessage::SessionUpdate {
                state,
                attachments,
                title,
            } => {
                let len = 1 + usize::from(
                    state.is_some() as u8 + attachments.is_some() as u8 + title.is_some() as u8,
                );
                let mut map = serializer.serialize_map(Some(len))?;
                map.serialize_entry("t", "session.update")?;
                if let Some(state) = state {
                    map.serialize_entry("state", state.as_wire())?;
                }
                if let Some(attachments) = attachments {
                    map.serialize_entry("attachments", &WireAttachments(attachments))?;
                }
                if let Some(title) = title {
                    // Inner `None` is the explicit clear, and it is written as
                    // a present null.
                    map.serialize_entry("title", title)?;
                }
                map.end()
            }
            ControlMessage::SessionExited { code, signal, at } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("t", "session.exited")?;
                map.serialize_entry("code", code)?;
                map.serialize_entry("signal", signal)?;
                map.serialize_entry("at", at)?;
                map.end()
            }
            ControlMessage::Error { code, message } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("t", "error")?;
                map.serialize_entry("code", code)?;
                map.serialize_entry("message", message)?;
                map.end()
            }
        }
    }
}

struct WireClient<'a>(&'a ClientInfo);

impl Serialize for WireClient<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("kind", &self.0.kind)?;
        map.serialize_entry("name", &self.0.name)?;
        map.end()
    }
}

struct WireSize<'a>(&'a Size);

impl Serialize for WireSize<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("cols", &self.0.cols)?;
        map.serialize_entry("rows", &self.0.rows)?;
        map.end()
    }
}

struct WireSession<'a>(&'a SessionInfo);

impl Serialize for WireSession<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("id", &self.0.id)?;
        map.serialize_entry("name", &self.0.name)?;
        map.serialize_entry("title", &self.0.title)?;
        map.serialize_entry("state", self.0.state.as_wire())?;
        map.serialize_entry("size", &WireSize(&self.0.size))?;
        map.end()
    }
}

struct WireAttachment<'a>(&'a Attachment);

impl Serialize for WireAttachment<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.0.id)?;
        map.serialize_entry("mode", self.0.mode.as_wire())?;
        map.serialize_entry("client", &WireClient(&self.0.client))?;
        map.end()
    }
}

struct WireAttachments<'a>(&'a [Attachment]);

impl Serialize for WireAttachments<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for attachment in self.0 {
            seq.serialize_element(&WireAttachment(attachment))?;
        }
        seq.end()
    }
}

/// Decodes a MessagePack control payload.
///
/// `payload` is the frame payload with the header already stripped. Rejects a
/// missing or unknown `t`, and an `attach` naming a version this build does not
/// speak, rather than accepting part of a message.
pub fn decode(payload: &[u8]) -> Result<ControlMessage, ControlError> {
    let value =
        msgpack::from_slice(payload).map_err(|detail| ControlError::MalformedPayload { detail })?;
    let fields =
        Fields::parse(&value, String::new()).map_err(|detail| ControlError::MalformedPayload {
            detail: format!("a control payload is a map with string keys: {detail}"),
        })?;

    // A payload with no usable discriminator is refused before anything else is
    // read. Inferring the message from which fields happen to be present is the
    // one thing the discriminator exists to prevent, and the fields of `resize`
    // are a subset of several other messages'.
    let t = match fields.get("t") {
        Some(Value::Str(t)) => t.as_str(),
        Some(_) | None => return Err(ControlError::MissingDiscriminator),
    };

    match t {
        "attach" => decode_attach(&fields, t),
        "attached" => decode_attached(&fields, t),
        "resize" => Ok(ControlMessage::Resize {
            cols: u16_field(&fields, t, "cols")?,
            rows: u16_field(&fields, t, "rows")?,
        }),
        "control.request" => Ok(ControlMessage::ControlRequest {
            steal: bool_field(&fields, t, "steal")?,
        }),
        "control.state" => Ok(ControlMessage::ControlState {
            controller_id: opt_string_field(&fields, t, "controller_id")?,
            controller_label: opt_string_field(&fields, t, "controller_label")?,
        }),
        "session.update" => decode_session_update(&fields, t),
        "session.exited" => Ok(ControlMessage::SessionExited {
            code: match fields.get("code") {
                None | Some(Value::Nil) => None,
                Some(_) => Some(i32_field(&fields, t, "code")?),
            },
            signal: opt_string_field(&fields, t, "signal")?,
            at: string_field(&fields, t, "at")?,
        }),
        "error" => Ok(ControlMessage::Error {
            code: string_field(&fields, t, "code")?,
            message: string_field(&fields, t, "message")?,
        }),
        unknown => Err(ControlError::UnknownMessage {
            t: unknown.to_string(),
        }),
    }
}

fn decode_attach(fields: &Fields<'_>, t: &str) -> Result<ControlMessage, ControlError> {
    // Version first. An attach this build cannot speak is refused as a version
    // failure rather than as whichever of its other fields happens to be read
    // first, because that is the error the peer has to act on.
    let protocol = u32_field(fields, t, "protocol")?;
    if protocol != PROTOCOL_VERSION {
        return Err(ControlError::UnsupportedProtocol {
            requested: protocol,
            supported: PROTOCOL_VERSION,
        });
    }

    let mode = string_field(fields, t, "mode")?;
    let mode = AttachMode::from_wire(&mode).ok_or_else(|| {
        invalid(
            t,
            "mode",
            format!("{mode:?} is neither `watch` nor `control`"),
        )
    })?;

    Ok(ControlMessage::Attach {
        protocol,
        mode,
        steal: bool_field(fields, t, "steal")?,
        client: client_field(fields, t, "client")?,
        size: size_field(fields, t, "size")?,
    })
}

fn decode_attached(fields: &Fields<'_>, t: &str) -> Result<ControlMessage, ControlError> {
    let session = map_field(fields, t, "session")?;
    let title = opt_string_field(&session, t, "title")?;
    let state = string_field(&session, t, "state")?;
    let state = SessionState::from_wire(&state).ok_or_else(|| {
        invalid(
            t,
            session.name("state"),
            format!("{state:?} names no state"),
        )
    })?;

    Ok(ControlMessage::Attached {
        // Not version-checked: negotiation rides `attach`, and a worker's reply
        // states the version it agreed to speak, which the client already
        // proposed.
        protocol: u32_field(fields, t, "protocol")?,
        session: SessionInfo {
            id: string_field(&session, t, "id")?,
            name: string_field(&session, t, "name")?,
            title,
            state,
            size: size_field(&session, t, "size")?,
        },
        controller: opt_string_field(fields, t, "controller")?,
        attachments: attachments_field(fields, t, "attachments")?,
    })
}

fn decode_session_update(fields: &Fields<'_>, t: &str) -> Result<ControlMessage, ControlError> {
    // The one place where "key absent" and "key present and null" must decode
    // differently. It is spelled out rather than folded into the shared
    // accessors, which deliberately treat the two alike everywhere else.
    let state = match fields.get("state") {
        None => None,
        Some(value) => {
            let name = as_str(value, t, "state".to_string())?;
            Some(
                SessionState::from_wire(name)
                    .ok_or_else(|| invalid(t, "state", format!("{name:?} names no state")))?,
            )
        }
    };

    let attachments = match fields.get("attachments") {
        None => None,
        Some(_) => Some(attachments_field(fields, t, "attachments")?),
    };

    let title = match fields.get("title") {
        None => None,
        Some(Value::Nil) => Some(None),
        Some(value) => Some(Some(as_str(value, t, "title".to_string())?.to_string())),
    };

    Ok(ControlMessage::SessionUpdate {
        state,
        attachments,
        title,
    })
}

// ------------------------------------------------------------------ decoding
//
// A control payload is read as a value tree rather than deserialized into a
// struct, so that a missing key, a key present and null, and a key of the wrong
// type are three distinguishable outcomes rather than one. See `crate::msgpack`.
//
// Unknown keys inside a *known* message are ignored. The discriminator and the
// documented fields carry the meaning; refusing a message because a newer peer
// attached a field this build has no use for would make every additive change
// a version break, which is a cost the protocol has no reason to pay. An
// unknown *message*, by contrast, is refused — there the meaning itself is the
// unknown.

/// The pairs of a MessagePack map, borrowed, with string keys.
///
/// `path` is the dotted prefix this map sits at inside the message, so an error
/// about a nested field names `session.size.cols` rather than `cols` — three of
/// the messages have a `cols`, and a report that does not say which is a report
/// a reader has to guess at.
struct Fields<'a> {
    pairs: Vec<(&'a str, &'a Value)>,
    path: String,
}

impl<'a> Fields<'a> {
    fn parse(value: &'a Value, path: String) -> Result<Self, String> {
        let Value::Map(pairs) = value else {
            return Err(format!("found {}", value.type_name()));
        };

        let mut fields: Vec<(&str, &Value)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            let Value::Str(key) = key else {
                return Err(format!("a key is {}, not a string", key.type_name()));
            };
            if fields.iter().any(|(seen, _)| *seen == key.as_str()) {
                // Which of the two a reader takes is a coin flip, and the
                // messages here move input control.
                return Err(format!("duplicate key `{key}`"));
            }
            fields.push((key.as_str(), value));
        }
        Ok(Self {
            pairs: fields,
            path,
        })
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.pairs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    }

    /// The dotted name `key` is reported as.
    fn name(&self, key: &str) -> String {
        format!("{}{key}", self.path)
    }
}

fn invalid(message: &str, field: impl Into<String>, detail: impl Into<String>) -> ControlError {
    ControlError::InvalidField {
        message: message.to_string(),
        field: field.into(),
        detail: detail.into(),
    }
}

fn required<'a>(fields: &Fields<'a>, message: &str, key: &str) -> Result<&'a Value, ControlError> {
    fields
        .get(key)
        .ok_or_else(|| invalid(message, fields.name(key), "missing"))
}

fn as_str<'a>(value: &'a Value, message: &str, field: String) -> Result<&'a str, ControlError> {
    match value {
        Value::Str(s) => Ok(s.as_str()),
        other => Err(invalid(
            message,
            field,
            format!("expected a string, found {}", other.type_name()),
        )),
    }
}

fn string_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<String, ControlError> {
    let value = required(fields, message, key)?;
    Ok(as_str(value, message, fields.name(key))?.to_string())
}

/// Absent and null both read as "no value" everywhere except `session.update`,
/// which needs them apart and therefore does not use this.
fn opt_string_field(
    fields: &Fields<'_>,
    message: &str,
    key: &str,
) -> Result<Option<String>, ControlError> {
    match fields.get(key) {
        None | Some(Value::Nil) => Ok(None),
        Some(value) => Ok(Some(as_str(value, message, fields.name(key))?.to_string())),
    }
}

fn bool_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<bool, ControlError> {
    match required(fields, message, key)? {
        Value::Bool(b) => Ok(*b),
        // `"false"` is truthy in one of the two languages that implement this
        // protocol, so a coerced read here hands control to a client that did
        // not ask to steal it.
        other => Err(invalid(
            message,
            fields.name(key),
            format!("expected a boolean, found {}", other.type_name()),
        )),
    }
}

fn u32_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<u32, ControlError> {
    bounded_int(fields, message, key, 0, i64::from(u32::MAX)).map(|n| n as u32)
}

fn u16_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<u16, ControlError> {
    bounded_int(fields, message, key, 0, i64::from(u16::MAX)).map(|n| n as u16)
}

fn i32_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<i32, ControlError> {
    bounded_int(
        fields,
        message,
        key,
        i64::from(i32::MIN),
        i64::from(i32::MAX),
    )
    .map(|n| n as i32)
}

/// Reads an integer field and refuses one outside the range its type can hold.
///
/// Truncating instead would turn a 70000-column resize into a 4464-column one:
/// a value the peer never sent, applied silently.
fn bounded_int(
    fields: &Fields<'_>,
    message: &str,
    key: &str,
    min: i64,
    max: i64,
) -> Result<i64, ControlError> {
    let value = required(fields, message, key)?;
    let n = match value {
        Value::Int(n) => *n,
        // Larger than an i64 can hold, so larger than any field's range.
        Value::Uint(n) => {
            return Err(invalid(
                message,
                fields.name(key),
                format!("{n} is outside the accepted range {min}..={max}"),
            ))
        }
        other => {
            return Err(invalid(
                message,
                fields.name(key),
                format!("expected an integer, found {}", other.type_name()),
            ))
        }
    };
    if n < min || n > max {
        return Err(invalid(
            message,
            fields.name(key),
            format!("{n} is outside the accepted range {min}..={max}"),
        ));
    }
    Ok(n)
}

fn map_field<'a>(
    fields: &Fields<'a>,
    message: &str,
    key: &str,
) -> Result<Fields<'a>, ControlError> {
    let value = required(fields, message, key)?;
    Fields::parse(value, format!("{}.", fields.name(key))).map_err(|detail| {
        invalid(
            message,
            fields.name(key),
            format!("expected a map: {detail}"),
        )
    })
}

fn client_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<ClientInfo, ControlError> {
    let client = map_field(fields, message, key)?;
    Ok(ClientInfo {
        kind: string_field(&client, message, "kind")?,
        name: string_field(&client, message, "name")?,
    })
}

fn size_field(fields: &Fields<'_>, message: &str, key: &str) -> Result<Size, ControlError> {
    let size = map_field(fields, message, key)?;
    Ok(Size {
        cols: u16_field(&size, message, "cols")?,
        rows: u16_field(&size, message, "rows")?,
    })
}

fn attachments_field(
    fields: &Fields<'_>,
    message: &str,
    key: &str,
) -> Result<Vec<Attachment>, ControlError> {
    let Value::Array(items) = required(fields, message, key)? else {
        return Err(invalid(message, fields.name(key), "expected an array"));
    };

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let path = format!("{}[{index}].", fields.name(key));
            let attachment = Fields::parse(item, path).map_err(|detail| {
                invalid(
                    message,
                    format!("{}[{index}]", fields.name(key)),
                    format!("expected a map: {detail}"),
                )
            })?;
            let mode = string_field(&attachment, message, "mode")?;
            Ok(Attachment {
                id: string_field(&attachment, message, "id")?,
                mode: AttachMode::from_wire(&mode).ok_or_else(|| {
                    invalid(
                        message,
                        attachment.name("mode"),
                        format!("{mode:?} is neither `watch` nor `control`"),
                    )
                })?,
                client: client_field(&attachment, message, "client")?,
            })
        })
        .collect()
}
