//! The attachment registry: who is here, and who may write.
//!
//! Everything the protocol says about attachments lives here as a state machine
//! that performs no I/O. Calls return [`Effect`]s describing what the worker
//! should then do — send this, broadcast that, resize the PTY, push a snapshot.
//!
//! The separation is what makes the hard cases testable. "A steal races a
//! detach", "a process exit races an attach", "control transfers are visible to
//! both parties" are statements about ordering, and a state machine that returns
//! its effects lets a test assert the ordering directly instead of inferring it
//! from bytes that happened to arrive in a helpful sequence.
//!
//! Two rules are enforced here and nowhere else:
//!
//! - **At most one attachment holds control.** A second `control` attach gets
//!   `control_busy` unless it sets `steal`, in which case the incumbent is
//!   demoted and told so.
//! - **Control is released by socket close, not by timer expiry.** There is no
//!   lease and no clock in this module. Locally the operating system reports
//!   departure precisely, so a timer could only be wrong relative to it. Leases
//!   arrive in M4, where a remote connection can hang without closing (D5).

use latch_protocol::control::{
    AttachMode, Attachment, ClientInfo, ControlMessage, SessionInfo, SessionState, Size,
};

use crate::worker::queue::AttachmentStats;
use crate::worker::resize::ResizeAuthority;

/// Identifier the worker assigns to one attachment.
///
/// Assigned by the worker rather than claimed by the client: it appears in every
/// other client's presence list, and an identifier a client chooses is an
/// identifier a client can collide with or impersonate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentId(String);

impl AttachmentId {
    /// Wraps a worker-assigned identifier.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The identifier as it appears on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A client's `attach`, after decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRequest {
    /// Protocol version the client speaks.
    pub protocol: u32,
    /// Whether it wants to write.
    pub mode: AttachMode,
    /// Whether it will take control from an incumbent.
    pub steal: bool,
    /// Who it says it is, already sanitized at ingest.
    pub client: ClientInfo,
    /// Its terminal size, honored only while it holds control.
    pub size: Size,
}

/// Why an attach or a control request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The client speaks a version this build does not.
    UnsupportedProtocol {
        /// Version requested.
        requested: u32,
        /// Version supported.
        supported: u32,
    },
    /// Another attachment holds control and `steal` was not set.
    ControlBusy {
        /// Who holds it, for a message a human can act on.
        controller_label: Option<String>,
    },
    /// The attachment is not, or is no longer, known.
    ///
    /// The ordinary cause is a race rather than a bug: a request arriving from a
    /// connection the worker has already reaped because its socket closed.
    UnknownAttachment,
}

impl Rejection {
    /// The stable machine-readable code a client may branch on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedProtocol { .. } => "unsupported_protocol",
            Self::ControlBusy { .. } => "control_busy",
            Self::UnknownAttachment => "unknown_attachment",
        }
    }

    /// The `error` message this rejection is delivered as.
    pub fn to_message(&self) -> ControlMessage {
        ControlMessage::Error {
            code: self.code().to_owned(),
            message: self.to_string(),
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProtocol {
                requested,
                supported,
            } => write!(
                f,
                "unsupported protocol version {requested}; this build speaks {supported}"
            ),
            Self::ControlBusy { controller_label } => match controller_label {
                Some(label) => write!(f, "input control is held by {label}"),
                None => f.write_str("input control is held by another attachment"),
            },
            Self::UnknownAttachment => f.write_str("no such attachment"),
        }
    }
}

/// Something the worker must do as a result of a registry call.
///
/// Ordered: the worker performs them in the order returned. A `Resize` that
/// arrives after the `control.state` announcing the new controller is a screen
/// that reflows after the client learns why, which is the order a human expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send one control message to one attachment.
    Send {
        /// Recipient.
        to: AttachmentId,
        /// What to send.
        message: ControlMessage,
    },
    /// Send one control message to every attachment.
    ///
    /// Including the attachment that caused it. A controller that stole control
    /// and a controller that lost it both learn the same fact from the same
    /// message, so there is no second code path that could disagree.
    Broadcast {
        /// What to send.
        message: ControlMessage,
    },
    /// Set the session size: the PTY window size and the screen model.
    Resize {
        /// The new authoritative size.
        size: Size,
    },
    /// Send the current screen to one attachment as `terminal.output` frames.
    ///
    /// The snapshot is not a message. A fresh attach and a resync after an
    /// overflowing queue emit exactly this same effect, which is what keeps
    /// reconnect from being a second code path.
    Snapshot {
        /// Recipient.
        to: AttachmentId,
    },
    /// Close one attachment's connection, after anything already queued for it.
    Close {
        /// Whose connection.
        to: AttachmentId,
    },
}

/// What an accepted attach produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachOutcome {
    /// The identifier assigned to the new attachment.
    pub id: AttachmentId,
    /// The mode it was actually granted.
    ///
    /// Not always the mode it asked for: a `control` attach without `steal` is
    /// refused outright, but a `control` attach that arrives while nobody holds
    /// control is granted, and a caller should not have to re-derive which
    /// happened.
    pub granted: AttachMode,
    /// Everything to do, in order. Begins with the `attached` message and the
    /// snapshot; ends with any broadcast the new arrival caused.
    pub effects: Vec<Effect>,
}

/// One attached client, as the registry tracks it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: AttachmentId,
    /// The mode currently granted, which is not always the mode asked for: an
    /// incumbent demoted by a steal is a watcher from that moment on.
    mode: AttachMode,
    client: ClientInfo,
    /// This client's own terminal size, honored only while it holds control.
    size: Size,
    stats: AttachmentStats,
}

impl Entry {
    fn to_wire(&self) -> Attachment {
        Attachment {
            id: self.id.to_string(),
            mode: self.mode,
            client: self.client.clone(),
        }
    }

    /// How this client is named to a human who was refused control by it.
    fn label(&self) -> String {
        if self.client.name.is_empty() {
            self.client.kind.clone()
        } else {
            self.client.name.clone()
        }
    }
}

/// The registry of everyone attached to this session.
pub struct Registry {
    /// Attachments in arrival order, which is the order presence lists use.
    entries: Vec<Entry>,
    /// Who may write. At most one, ever.
    controller: Option<AttachmentId>,
    /// The session's size and how it changes hands (decision D4).
    resize: ResizeAuthority,
    /// Session identity, as `attached` reports it.
    id: String,
    name: String,
    title: Option<String>,
    state: SessionState,
    /// Monotonic counter behind attachment ids. Worker-assigned identifiers are
    /// never reused within a session, so a presence list cannot show two
    /// different clients under one name.
    next: u64,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

impl Registry {
    /// An empty registry for a session of `initial_size`.
    pub fn new(initial_size: Size) -> Self {
        Self {
            entries: Vec::new(),
            controller: None,
            resize: ResizeAuthority::new(initial_size),
            id: String::new(),
            name: String::new(),
            title: None,
            state: SessionState::Running,
            next: 0,
        }
    }

    /// Names the session this registry speaks for.
    ///
    /// Identity comes from `meta.json`, which the worker has already written by
    /// the time it starts accepting; the registry carries it only so that the
    /// `attached` message is assembled in one place rather than patched up by
    /// whoever happens to be sending it.
    pub fn set_identity(&mut self, id: impl Into<String>, name: impl Into<String>) {
        self.id = id.into();
        self.name = name.into();
    }

    /// Admits a client, or refuses it.
    ///
    /// A refusal is terminal for that connection: the worker sends the `error`
    /// and closes. There is no partial admission — a client that asked for
    /// control and was refused does not silently become a watcher, because it
    /// would then be a client that believes it can type and cannot.
    pub fn attach(&mut self, request: AttachRequest) -> Result<AttachOutcome, Rejection> {
        if request.protocol != latch_protocol::PROTOCOL_VERSION {
            return Err(Rejection::UnsupportedProtocol {
                requested: request.protocol,
                supported: latch_protocol::PROTOCOL_VERSION,
            });
        }
        let wants_control = request.mode == AttachMode::Control;
        if wants_control && !request.steal {
            if let Some(holder) = self.controller.clone() {
                return Err(Rejection::ControlBusy {
                    controller_label: self.entry(&holder).map(Entry::label),
                });
            }
        }

        self.next += 1;
        let id = AttachmentId::new(format!("att_{:08}", self.next));
        self.entries.push(Entry {
            id: id.clone(),
            mode: if wants_control {
                AttachMode::Control
            } else {
                AttachMode::Watch
            },
            client: request.client,
            size: request.size,
            stats: AttachmentStats::default(),
        });

        // The size moves before `attached` is assembled, so the session size the
        // new controller is told is the one it just caused.
        let mut control_moved = None;
        if wants_control {
            control_moved = Some(self.grant(&id, request.size));
        }

        let mut effects = Vec::new();
        // The reflow happens before anything is serialized, so the screen the
        // new controller receives is at the geometry it just imposed. Ordering
        // this after the snapshot is not a cosmetic difference: a 40-column
        // phone taking control of a 200-column desk session would be sent 200
        // columns of screen and paint it, wrapped, into 40 — every time, and
        // recovering only if the child happens to repaint on `SIGWINCH`. This
        // is the single instruction that decides whether reattach is exact.
        //
        // No frames are produced here, so the wire order a client observes is
        // still `attached` and then the screen.
        if let Some(Some(size)) = control_moved {
            effects.push(Effect::Resize { size });
        }
        effects.push(Effect::Send {
            to: id.clone(),
            message: ControlMessage::Attached {
                protocol: latch_protocol::PROTOCOL_VERSION,
                session: self.session_info(),
                controller: self.controller.as_ref().map(AttachmentId::to_string),
                attachments: self.attachments(),
            },
        });
        // The screen follows the acceptance immediately and before any live
        // output. It is not part of the message: it arrives as ordinary
        // `terminal.output`, which is what makes reconnect just attach.
        effects.push(Effect::Snapshot { to: id.clone() });
        if control_moved.is_some() {
            effects.push(Effect::Broadcast {
                message: self.control_state(),
            });
        }
        effects.push(Effect::Broadcast {
            message: self.presence(),
        });

        Ok(AttachOutcome {
            id,
            granted: if wants_control {
                AttachMode::Control
            } else {
                AttachMode::Watch
            },
            effects,
        })
    }

    /// Removes an attachment whose socket closed.
    ///
    /// Idempotent. Detaching an attachment that is already gone returns no
    /// effects rather than failing, because the ordinary cause is a race the
    /// worker cannot prevent: the client's connection closing while a request of
    /// its own is still in flight.
    pub fn detach(&mut self, id: &AttachmentId) -> Vec<Effect> {
        let Some(at) = self.entries.iter().position(|entry| &entry.id == id) else {
            return Vec::new();
        };
        self.entries.remove(at);

        let mut effects = Vec::new();
        // Control is released by the socket closing, and by nothing else. There
        // is no lease to expire here: locally the operating system reports the
        // departure precisely, so a timer could only be wrong relative to it.
        if self.controller.as_ref() == Some(id) {
            self.controller = None;
            effects.push(Effect::Broadcast {
                message: self.control_state(),
            });
        }
        // Released whether or not it was the controller. A departure from the
        // middle of the stack changes nothing, which is exactly what a demoted
        // incumbent leaving must do.
        effects.extend(self.resize.release(id).map(|size| Effect::Resize { size }));
        effects.push(Effect::Broadcast {
            message: self.presence(),
        });
        effects
    }

    /// Handles `control.request` from an attached client.
    ///
    /// Granting control to the attachment that already holds it is a no-op that
    /// returns no effects, so a client retrying a request it is unsure landed
    /// does not produce a second broadcast.
    pub fn request_control(
        &mut self,
        id: &AttachmentId,
        steal: bool,
    ) -> Result<Vec<Effect>, Rejection> {
        let Some(size) = self.entry(id).map(|entry| entry.size) else {
            return Err(Rejection::UnknownAttachment);
        };
        if self.controller.as_ref() == Some(id) {
            return Ok(Vec::new());
        }
        if !steal {
            if let Some(holder) = self.controller.clone() {
                return Err(Rejection::ControlBusy {
                    controller_label: self.entry(&holder).map(Entry::label),
                });
            }
        }

        let resized = self.grant(id, size);
        let mut effects = vec![Effect::Broadcast {
            message: self.control_state(),
        }];
        effects.extend(resized.map(|size| Effect::Resize { size }));
        effects.push(Effect::Broadcast {
            message: self.presence(),
        });
        Ok(effects)
    }

    /// Handles `resize` from an attached client.
    ///
    /// Returns no effects for a watcher. Watchers never resize the session, and
    /// a watcher's `resize` is not an error — a phone that attached to peek still
    /// rotates, and every rotation would otherwise be a protocol violation.
    pub fn resize(&mut self, id: &AttachmentId, size: Size) -> Vec<Effect> {
        let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id) else {
            return Vec::new();
        };
        // Remembered even for a watcher: if it later takes control, it takes it
        // at the size its window is now rather than the size it arrived with.
        entry.size = size;
        self.resize
            .controller_resize(id, size)
            .map(|size| Effect::Resize { size })
            .into_iter()
            .collect()
    }

    /// Applies an explicit size, as `latch resize` sets it.
    pub fn set_size(&mut self, size: Size, pin: bool) -> Vec<Effect> {
        let changed = if pin {
            self.resize.pin(size)
        } else {
            self.resize.set_base_unpinned(size)
        };
        changed
            .map(|size| Effect::Resize { size })
            .into_iter()
            .collect()
    }

    /// Releases an explicit pin.
    pub fn unpin_size(&mut self) -> Vec<Effect> {
        self.resize
            .unpin()
            .map(|size| Effect::Resize { size })
            .into_iter()
            .collect()
    }

    /// Sets or clears the session title, broadcasting the change.
    ///
    /// `None` clears it, and the broadcast carries `title: Some(None)` — the
    /// distinction between "unchanged" and "cleared" is why the field is doubly
    /// optional on the wire, and it has to survive this call.
    pub fn set_title(&mut self, title: Option<String>) -> Vec<Effect> {
        self.title = title.clone();
        vec![Effect::Broadcast {
            message: ControlMessage::SessionUpdate {
                state: None,
                attachments: None,
                title: Some(title),
                force: None,
            },
        }]
    }

    /// Records a lifecycle change and broadcasts it.
    pub fn set_state(&mut self, state: SessionState) -> Vec<Effect> {
        self.state = state;
        vec![Effect::Broadcast {
            message: ControlMessage::SessionUpdate {
                state: Some(state),
                attachments: None,
                title: None,
                force: None,
            },
        }]
    }

    /// Broadcasts `session.exited` and closes every attachment.
    ///
    /// The close is part of the same call, and ordered after the broadcast, so
    /// no client can be dropped without first being told why.
    pub fn mark_exited(&mut self, exited: ControlMessage) -> Vec<Effect> {
        self.state = SessionState::Exited;
        self.controller = None;
        let mut effects = vec![Effect::Broadcast { message: exited }];
        effects.extend(self.entries.iter().map(|entry| Effect::Close {
            to: entry.id.clone(),
        }));
        self.entries.clear();
        effects
    }

    /// The attachment holding control, if any.
    pub fn controller(&self) -> Option<AttachmentId> {
        self.controller.clone()
    }

    /// The presence list as clients see it.
    pub fn attachments(&self) -> Vec<Attachment> {
        self.entries.iter().map(Entry::to_wire).collect()
    }

    /// How many attachments are present.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nobody is attached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The session's authoritative size.
    pub fn session_size(&self) -> Size {
        self.resize.effective()
    }

    /// The resize authority, for inspection.
    pub fn resize_authority(&self) -> &ResizeAuthority {
        &self.resize
    }

    /// Delivery statistics for one attachment.
    ///
    /// This is what makes a resynchronizing client distinguishable from a healthy
    /// one. Without it a bad link and a good one look identical from outside, and
    /// M2 is where that difference starts to matter.
    pub fn stats(&self, id: &AttachmentId) -> Option<AttachmentStats> {
        self.entry(id).map(|entry| entry.stats)
    }

    /// Records what the fanout knows about one attachment's delivery.
    ///
    /// The queues live outside the registry so that "publish never waits" can be
    /// asserted against them directly; this is how what they observe reaches
    /// `latch inspect`.
    pub fn record_stats(&mut self, id: &AttachmentId, stats: AttachmentStats) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id) {
            entry.stats = stats;
        }
    }

    /// Moves control to `id`, demoting whoever held it.
    ///
    /// The demoted incumbent is left on the resize stack rather than released:
    /// control nests, and its entry is what a later unwind returns to. It learns
    /// it was demoted from the same `control.state` broadcast every other client
    /// receives — a private "you were demoted" message would be a second code
    /// path that could disagree with the first.
    fn grant(&mut self, id: &AttachmentId, size: Size) -> Option<Size> {
        if let Some(previous) = self.controller.take() {
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == previous) {
                entry.mode = AttachMode::Watch;
            }
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id) {
            entry.mode = AttachMode::Control;
        }
        self.controller = Some(id.clone());
        self.resize.take_control(id, size)
    }

    fn entry(&self, id: &AttachmentId) -> Option<&Entry> {
        self.entries.iter().find(|entry| &entry.id == id)
    }

    fn session_info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            title: self.title.clone(),
            state: self.state,
            size: self.resize.effective(),
        }
    }

    fn control_state(&self) -> ControlMessage {
        ControlMessage::ControlState {
            controller_id: self.controller.as_ref().map(AttachmentId::to_string),
            controller_label: self
                .controller
                .as_ref()
                .and_then(|id| self.entry(id))
                .map(Entry::label),
        }
    }

    /// A `session.update` carrying only the presence list.
    ///
    /// Only the list. `session.update` merges, so restating a title here would
    /// make every arrival and departure a chance to overwrite one.
    fn presence(&self) -> ControlMessage {
        ControlMessage::SessionUpdate {
            state: None,
            attachments: Some(self.attachments()),
            title: None,
            force: None,
        }
    }
}
