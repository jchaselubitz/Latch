//! The control socket: bind, peer credentials, framed I/O.
//!
//! One Unix socket per session, `0600`, inside a `0700` directory. The socket
//! answering is what "the session is alive" means — there is no registry to
//! consult and no PID to check — so binding it correctly is load-bearing rather
//! than hygiene.
//!
//! Filesystem permissions are the first check and the peer-credential check is
//! the second. They are not redundant: modes are what a reader of the directory
//! sees, and peer credentials are what the kernel says about the process on the
//! other end of an already-open connection. On a machine where the mode was ever
//! wrong — a restored backup, a shared `/tmp`, a future remote transport — the
//! credential check is the one still standing.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use latch_protocol::frame::{self, FrameDecoder, FrameType};

use crate::worker::paths::SOCKET_MODE;

/// Anything that can go wrong with the socket.
#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    /// Binding, accepting, or re-moding failed.
    #[error("{path}: {source}")]
    Io {
        /// The socket path.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
    /// A live worker already owns this socket.
    ///
    /// Distinguished from a stale socket file, which is removed and rebound: a
    /// socket that answers belongs to somebody.
    #[error("{path} is already owned by a live worker")]
    AlreadyBound {
        /// The socket that answered.
        path: PathBuf,
    },
    /// The peer is not the owning user.
    ///
    /// The connection is closed without a reply. There is nothing to negotiate
    /// with a peer that is not permitted to be here, and an error frame would
    /// only confirm that the socket exists.
    #[error("connection from uid {peer_uid} refused; this session belongs to uid {owner_uid}")]
    PeerRejected {
        /// The uid that connected.
        peer_uid: u32,
        /// The uid permitted to.
        owner_uid: u32,
    },
    /// Peer credentials could not be read at all.
    ///
    /// Fatal for the connection: an unidentifiable peer is refused, never
    /// admitted on the assumption that it is probably fine.
    #[error("cannot read peer credentials: {0}")]
    NoPeerCredentials(#[source] io::Error),
    /// A frame could not be decoded.
    #[error(transparent)]
    Frame(#[from] latch_protocol::frame::FrameError),
    /// A control payload could not be decoded.
    #[error(transparent)]
    Control(#[from] latch_protocol::control::ControlError),
}

/// What the kernel says about the process on the other end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Peer's effective uid.
    pub uid: u32,
    /// Peer's effective gid.
    pub gid: u32,
    /// Peer's pid, for display in presence lists and `latch inspect`.
    ///
    /// Display only. It is never signalled — see the module documentation on
    /// `worker`.
    pub pid: i32,
}

/// Reads the credentials of a connected peer.
///
/// Two implementations, because the two target platforms disagree about how a
/// process asks. Both answer the only question that matters — which uid is on
/// the other end — and neither trusts anything the peer said about itself.
#[cfg(target_os = "linux")]
pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, SocketError> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the socket fd is valid and the output buffer has the advertised size.
    if unsafe {
        libc::getsockopt(
            std::os::fd::AsRawFd::as_raw_fd(stream),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    } == -1
    {
        return Err(SocketError::NoPeerCredentials(io::Error::last_os_error()));
    }
    // SAFETY: getsockopt filled the `ucred` value above.
    let credentials = unsafe { credentials.assume_init() };
    Ok(PeerCredentials {
        uid: credentials.uid,
        gid: credentials.gid,
        pid: credentials.pid,
    })
}

/// Reads the credentials of a connected peer.
///
/// macOS has no `SO_PEERCRED`. `getpeereid` answers the uid and gid, and the pid
/// comes separately from `LOCAL_PEERPID` — which is allowed to fail, because the
/// pid is display metadata for `latch inspect` and the authorization decision
/// does not depend on it.
#[cfg(target_os = "macos")]
pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, SocketError> {
    use std::os::fd::AsRawFd;

    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the socket fd is valid and both out-params are initialized storage
    // of the types `getpeereid` writes.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == -1 {
        return Err(SocketError::NoPeerCredentials(io::Error::last_os_error()));
    }

    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: as above; the output buffer has the advertised size.
    let pid = if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::addr_of_mut!(pid).cast(),
            &mut length,
        )
    } == -1
    {
        0
    } else {
        pid
    };

    Ok(PeerCredentials { uid, gid, pid })
}

/// Decides whether a peer may attach.
///
/// Same uid, and nothing else. Group membership is not enough: a session carries
/// live terminal input to a process running as the owner, so anything short of
/// "is the owner" is a privilege boundary being crossed.
pub fn authorize(peer: &PeerCredentials, owner_uid: u32) -> Result<(), SocketError> {
    if peer.uid == owner_uid {
        Ok(())
    } else {
        Err(SocketError::PeerRejected {
            peer_uid: peer.uid,
            owner_uid,
        })
    }
}

/// A connection that passed the credential check.
#[derive(Debug)]
pub struct Accepted {
    /// The connection.
    pub stream: UnixStream,
    /// Who is on the other end.
    pub peer: PeerCredentials,
}

/// The session's listening socket.
pub struct ControlListener {
    listener: UnixListener,
    path: PathBuf,
    owner_uid: u32,
}

impl std::fmt::Debug for ControlListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlListener").finish_non_exhaustive()
    }
}

impl ControlListener {
    /// Binds at `path` for the current effective uid.
    pub fn bind(path: &Path) -> Result<Self, SocketError> {
        // SAFETY: geteuid cannot fail.
        Self::bind_for_owner(path, unsafe { libc::geteuid() })
    }

    /// Binds at `path` for an explicit owner uid.
    ///
    /// The owner is a parameter rather than an implicit `geteuid()` so the
    /// rejection path can be reached in a test without a second user account:
    /// bind for an owner that is not the caller, connect, and the connection
    /// must be refused while the worker carries on serving everyone else.
    pub fn bind_for_owner(path: &Path, owner_uid: u32) -> Result<Self, SocketError> {
        if path.exists() {
            if is_live(path) {
                return Err(SocketError::AlreadyBound {
                    path: path.to_owned(),
                });
            }
            fs::remove_file(path).map_err(|source| SocketError::Io {
                path: path.to_owned(),
                source,
            })?;
        }
        // Bound under a umask that already excludes everyone else, because
        // `bind` then `chmod` leaves a window in which the socket exists at its
        // real name with whatever mode the caller's umask allowed. The window is
        // short, and on a loaded machine "short" is long enough to be observed —
        // which is the same as saying it is long enough to be connected to.
        // The enclosing directory is `0700`, so this is the second lock on the
        // door rather than the first, and it is still worth shutting.
        let listener = {
            // SAFETY: `umask` is a process-global setter with no preconditions.
            // The worker binds once, before it has any threads.
            // `mode_t` is `u16` on Darwin and `u32` on Linux, so the mask is
            // narrowed to whatever this platform's type is rather than assuming.
            let previous = unsafe { libc::umask((!SOCKET_MODE & 0o777) as libc::mode_t) };
            let bound = UnixListener::bind(path);
            // SAFETY: as above, restoring what was read a moment ago.
            unsafe { libc::umask(previous) };
            bound.map_err(|source| SocketError::Io {
                path: path.to_owned(),
                source,
            })?
        };
        // Belt and braces: umask is advisory for sockets on some platforms, and
        // the mode is asserted rather than assumed (`docs/ARCHITECTURE_RULES.md`).
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE)).map_err(|source| {
            SocketError::Io {
                path: path.to_owned(),
                source,
            }
        })?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            owner_uid,
        })
    }

    /// The bound path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The uid permitted to connect.
    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// The underlying listener, for polling alongside the PTY.
    pub fn as_listener(&self) -> &UnixListener {
        &self.listener
    }

    /// Accepts one connection and checks its credentials.
    ///
    /// A refused peer produces [`SocketError::PeerRejected`] and its connection
    /// is already closed on return. The listener stays usable: one hostile
    /// connection must not end the session.
    pub fn accept(&self) -> Result<Accepted, SocketError> {
        let (stream, _) = self.listener.accept().map_err(|source| SocketError::Io {
            path: self.path.clone(),
            source,
        })?;
        let peer = peer_credentials(&stream)?;
        authorize(&peer, self.owner_uid)?;
        Ok(Accepted { stream, peer })
    }

    /// Removes the socket file.
    ///
    /// Called after `exit.json` is written, so a client that notices the socket
    /// disappear can already find the exit record. Ordering the other way makes
    /// a session momentarily `lost` on every clean exit.
    pub fn remove(&self) -> Result<(), SocketError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(SocketError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

/// Whether a socket path has a live worker behind it.
///
/// Connect and see. The file existing proves nothing — a worker killed with
/// `SIGKILL` leaves one behind — and this is the primitive the whole derived-state
/// rule rests on.
///
/// A success is conclusive; a refusal is not. See [`is_live_within`].
pub fn is_live(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Whether a socket path has a live worker behind it, allowing for one that is
/// briefly not accepting.
///
/// A refused connection does not distinguish "nobody is listening" from "the
/// listener is alive but has not called `accept` recently enough" — both are
/// `ECONNREFUSED`, on Darwin especially, where a backlog fills fast under a
/// caller that probes in a loop. A worker is legitimately not accepting while it
/// flushes its clients, so a single refusal answers a different question than
/// the one being asked.
///
/// The distinction matters because of what is done with a "no": a session
/// reported [`SessionState::Lost`](latch_protocol::control::SessionState::Lost)
/// is one `latch prune` reclaims, and a running session is not something to
/// reclaim because one probe arrived at an awkward moment. So callers whose
/// "no" is costly pay for a firmer answer, and callers on the happy path pay
/// nothing: a live worker still answers on the first attempt.
pub fn is_live_within(path: &Path, patience: Duration) -> bool {
    let deadline = std::time::Instant::now() + patience;
    loop {
        if is_live(path) {
            return true;
        }
        // A missing file is conclusive on its own: `remove` has already run, so
        // no amount of waiting will produce a listener.
        if !path.exists() {
            return false;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A frame this end owns, decoded off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFrame {
    /// The frame's type.
    pub frame_type: FrameType,
    /// Its payload.
    pub payload: Vec<u8>,
}

/// What one non-blocking read off a connection produced.
///
/// All three facts are reported together because they can be true at once: a
/// peer can deliver two good frames, then a malformed third, then close, all
/// within a single read. Collapsing that into one outcome loses the good frames,
/// and losing them is how an `attach` sent immediately before a disconnect turns
/// into a session that never registered the client at all.
#[derive(Debug, Default)]
pub struct Received {
    /// Whole frames decoded, in arrival order.
    pub frames: Vec<OwnedFrame>,
    /// The peer closed its write half. Nothing more is coming.
    pub closed: bool,
    /// The stream is unusable and must not be read again.
    ///
    /// A frame error is terminal by construction: after a length the decoder
    /// could not trust, every later byte offset is a guess.
    pub fatal: Option<SocketError>,
}

/// One accepted connection, read framed and written without ever blocking.
///
/// The write side is a queue rather than a `write_all`, and that is the whole
/// point of the type. A worker that writes straight to a socket is a worker that
/// stops reading the PTY as soon as one client stops reading its socket — which
/// is a phone on a cell network freezing the agent for the desk session.
pub struct ClientConnection {
    stream: UnixStream,
    decoder: FrameDecoder,
    peer: PeerCredentials,
    /// Encoded frames waiting for the socket to accept them.
    outbound: std::collections::VecDeque<Vec<u8>>,
    /// Bytes of the front chunk already accepted by the kernel.
    written: usize,
    /// Set once the connection should go away after its queue drains.
    closing: bool,
    /// Set when the socket itself failed; nothing more can be done with it.
    broken: bool,
}

impl std::fmt::Debug for ClientConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConnection").finish_non_exhaustive()
    }
}

impl ClientConnection {
    /// Takes ownership of an accepted connection and makes it non-blocking.
    pub fn new(accepted: Accepted) -> Result<Self, SocketError> {
        accepted
            .stream
            .set_nonblocking(true)
            .map_err(SocketError::NoPeerCredentials)?;
        Ok(Self {
            stream: accepted.stream,
            decoder: FrameDecoder::new(),
            peer: accepted.peer,
            outbound: std::collections::VecDeque::new(),
            written: 0,
            closing: false,
            broken: false,
        })
    }

    /// Who is on the other end.
    pub fn peer(&self) -> PeerCredentials {
        self.peer
    }

    /// The descriptor, for polling.
    pub fn fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.stream)
    }

    /// Whether anything is waiting to be written.
    pub fn has_pending_writes(&self) -> bool {
        !self.outbound.is_empty()
    }

    /// Whether this connection should be dropped.
    ///
    /// A connection asked to close stays alive until its queue drains, so the
    /// `error` explaining the close is delivered before the close itself.
    pub fn is_finished(&self) -> bool {
        self.broken || (self.closing && self.outbound.is_empty())
    }

    /// Asks for this connection to go away once everything queued is written.
    pub fn close_after_flush(&mut self) {
        self.closing = true;
    }

    /// Drops the connection immediately, discarding anything queued.
    pub fn abandon(&mut self) {
        self.broken = true;
        self.outbound.clear();
    }

    /// Discards frames which have not reached the kernel yet.
    ///
    /// A terminal snapshot replaces stale output after a client falls behind.
    /// This is intentionally separate from [`Self::abandon`]: the connection
    /// remains usable and will receive the current screen on its next write.
    ///
    /// A frame whose first bytes are already on the wire is *not* discarded. The
    /// peer is mid-frame: it has a length prefix and is counting payload bytes
    /// toward it. Dropping the tail would leave it reading the snapshot's header
    /// as the remainder of a payload, and every byte offset after that is a
    /// guess — a permanently desynchronized client on the one path that exists
    /// to rescue a client. So the in-flight frame is finished and only whole
    /// frames behind it are dropped, which is what "has not reached the kernel"
    /// meant all along.
    pub fn discard_pending_writes(&mut self) {
        if self.written == 0 {
            self.outbound.clear();
            return;
        }
        let in_flight = self.outbound.pop_front();
        self.outbound.clear();
        match in_flight {
            Some(frame) => self.outbound.push_front(frame),
            // Unreachable: a non-zero write offset implies a front frame. If it
            // is ever reached, the offset belongs to a frame that no longer
            // exists and must not be applied to whatever is queued next.
            None => self.written = 0,
        }
    }

    /// Reads whatever has arrived and decodes as many frames as it completes.
    ///
    /// Never blocks and never fails: everything that can go wrong is reported in
    /// the returned [`Received`] so one bad connection is handled by the caller
    /// rather than by unwinding the worker.
    pub fn receive(&mut self) -> Received {
        let mut received = Received::default();
        if self.broken {
            received.closed = true;
            return received;
        }
        let mut buffer = [0u8; 8192];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    received.closed = true;
                    break;
                }
                Ok(read) => self.decoder.feed(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    received.closed = true;
                    received.fatal = Some(SocketError::Io {
                        path: PathBuf::new(),
                        source: error,
                    });
                    break;
                }
            }
        }
        loop {
            match self.decoder.next_frame() {
                Ok(Some(frame)) => received.frames.push(OwnedFrame {
                    frame_type: frame.frame_type,
                    payload: frame.payload.to_vec(),
                }),
                Ok(None) => break,
                Err(error) => {
                    received.fatal = Some(SocketError::Frame(error));
                    break;
                }
            }
        }
        received
    }

    /// Queues one frame for this client.
    ///
    /// A payload larger than the codec accepts is split rather than refused.
    /// Terminal output has no message boundaries — it is a byte stream — so a
    /// screen snapshot of an unusually large window is several frames and the
    /// client cannot tell the difference.
    pub fn enqueue(&mut self, frame_type: FrameType, payload: &[u8]) {
        if self.broken {
            return;
        }
        if payload.is_empty() {
            self.push_encoded(frame_type, payload);
            return;
        }
        for chunk in payload.chunks(latch_protocol::MAX_FRAME_PAYLOAD) {
            self.push_encoded(frame_type, chunk);
        }
    }

    fn push_encoded(&mut self, frame_type: FrameType, payload: &[u8]) {
        match frame::encode(frame_type, payload) {
            Ok(bytes) => self.outbound.push_back(bytes),
            // Unreachable: `enqueue` already chunked to the limit. Dropping the
            // frame rather than panicking keeps a future caller's mistake from
            // taking the session's child down with it.
            Err(_) => self.broken = true,
        }
    }

    /// Writes as much of the queue as the socket will take right now.
    ///
    /// Stops at the first short write. A peer that has stopped reading leaves
    /// bytes queued here and nowhere else — never in the PTY.
    pub fn flush_some(&mut self) {
        while let Some(front) = self.outbound.front() {
            match self.stream.write(&front[self.written..]) {
                Ok(0) => {
                    self.broken = true;
                    return;
                }
                Ok(wrote) => {
                    self.written += wrote;
                    if self.written >= front.len() {
                        self.outbound.pop_front();
                        self.written = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.broken = true;
                    return;
                }
            }
        }
    }
}

/// Framed reads and writes over one connection.
///
/// Wraps the transport-agnostic codec in `latch-protocol` with the buffering a
/// socket needs. The codec stays ignorant of sockets on purpose: the same frames
/// ride a WebSocket from M3.
pub struct FrameStream {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl std::fmt::Debug for FrameStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameStream").finish_non_exhaustive()
    }
}

impl FrameStream {
    /// Wraps a connected stream.
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    /// The underlying socket descriptor, for polling alongside terminal input.
    pub fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Changes whether reads and writes wait for the peer.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.stream.set_nonblocking(nonblocking)
    }

    /// Bounds how long a read waits, so a silent peer is reported not waited on.
    ///
    /// `None` restores waiting indefinitely. A timeout surfaces as a `WouldBlock`
    /// or `TimedOut` error from [`Self::recv`], leaving the decoder's partial
    /// frame intact so the read can simply be repeated.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// The next frame already decoded, without touching the socket.
    ///
    /// One read routinely delivers several frames, and a caller that polls for
    /// readability between frames would wait on a socket with nothing left to
    /// announce while holding the answer in its own buffer. That is invisible
    /// under continuous output and total when the session is idle — which is
    /// exactly the case where somebody is asking whether it has frozen.
    pub fn next_buffered(&mut self) -> Result<Option<OwnedFrame>, SocketError> {
        Ok(self.decoder.next_frame()?.map(|frame| OwnedFrame {
            frame_type: frame.frame_type,
            payload: frame.payload.to_vec(),
        }))
    }

    /// Reads the next complete frame, or `None` when the peer closed cleanly.
    ///
    /// A peer that closes mid-frame is an error, not a `None`: a client that
    /// announced a length and then vanished said something false, and silently
    /// treating it as a clean close would hide a truncating transport.
    pub fn recv(&mut self) -> Result<Option<OwnedFrame>, SocketError> {
        loop {
            if let Some(frame) = self.decoder.next_frame()? {
                return Ok(Some(OwnedFrame {
                    frame_type: frame.frame_type,
                    payload: frame.payload.to_vec(),
                }));
            }
            let mut buffer = [0u8; 8192];
            match self.stream.read(&mut buffer) {
                Ok(0) => return Ok(None),
                Ok(read) => self.decoder.feed(&buffer[..read]),
                Err(error) => {
                    return Err(SocketError::Io {
                        path: PathBuf::new(),
                        source: error,
                    })
                }
            }
        }
    }

    /// Writes one frame.
    pub fn send(&mut self, frame_type: FrameType, payload: &[u8]) -> Result<(), SocketError> {
        let bytes = frame::encode(frame_type, payload)?;
        self.stream
            .write_all(&bytes)
            .map_err(|source| SocketError::Io {
                path: PathBuf::new(),
                source,
            })
    }

    /// Flushes buffered writes.
    pub fn flush(&mut self) -> Result<(), SocketError> {
        self.stream.flush().map_err(|source| SocketError::Io {
            path: PathBuf::new(),
            source,
        })
    }

    /// Closes the write half, leaving reads open.
    pub fn shutdown_write(&mut self) -> Result<(), SocketError> {
        self.stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|source| SocketError::Io {
                path: PathBuf::new(),
                source,
            })
    }
}
