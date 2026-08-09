//! The session worker: the per-session PTY owner.
//!
//! One worker process owns one session. It holds the PTY, the child process and
//! its process group, a live headless screen model, a bounded journal, its own
//! control socket, and the attachment registry that decides who may write. It
//! outlives every client attached to it, including the one that created it.
//!
//! # Shape of the implementation
//!
//! The modules split along the lines the tests split along, which is also how
//! `planning/IMPLEMENTATION_PLAN.md` splits M1's "Worker" items:
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`paths`] | `~/.latch` layout and the `0700`/`0600` modes |
//! | [`manifest`] | launch material, which arrives over stdin and never over argv |
//! | [`meta`] | `meta.json` and `exit.json`, and display-metadata sanitization |
//! | [`journal`] | the bounded output journal |
//! | [`process`] | PTY allocation, the child's process group, signals, exit |
//! | [`spawn`] | starting a worker that is detached from whoever started it |
//! | [`socket`] | the control socket, framing, and peer credentials |
//! | [`registry`] | attachments, exclusive write, presence, broadcasts |
//! | [`resize`] | resize authority and revert-on-detach (decision D4) |
//! | [`queue`] | per-client bounded queues and snapshot resync |
//!
//! # Two invariants the structure exists to guarantee
//!
//! **No stored PID is ever consulted for a kill.** `latch stop` is a message to
//! a live worker, and the worker signals *its own* child's process group — a
//! number it holds in memory because it created the group. A session whose
//! socket does not answer cannot be stopped, because there is nothing left to
//! stop. Nothing in this module reads a PID from disk; there is no API here that
//! could.
//!
//! **The PTY read never blocks on a client.** Output is published into bounded
//! per-client queues ([`queue::Fanout`]); a queue that overflows is dropped and
//! its client is resynchronized with a fresh snapshot. A phone on a cell network
//! is the ordinary case for this product, and a client that could apply
//! backpressure to the PTY would freeze the child for everyone attached.

use std::path::Path;
use std::time::{Duration, Instant};

use latch_protocol::control::{ClientInfo, ControlMessage, SessionState, Size};
use latch_protocol::frame::FrameType;
use latch_term::{HistoryPolicy, Screen, Terminal, TerminalConfig};

use crate::worker::registry::{Effect, Registry};

pub mod journal;
pub mod manifest;
pub mod meta;
pub mod paths;
pub mod process;
pub mod queue;
pub mod registry;
pub mod resize;
pub mod socket;
pub mod spawn;

pub use manifest::{LaunchManifest, LaunchSpec, TerminalSize, WorkerTuning};
pub use paths::{LatchHome, SessionId, SessionPaths};
pub use process::ChildExit;

/// Anything that can stop a worker from running.
///
/// Deliberately an enum over the layers rather than `anyhow`: a caller — the
/// spawning CLI, or a test — needs to distinguish "this session directory is
/// already owned by a live worker" from "the child could not be launched", and a
/// string does not support that.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The session directory or the `~/.latch` root could not be prepared.
    #[error(transparent)]
    Paths(#[from] paths::PathsError),
    /// The launch manifest could not be read or was not usable.
    #[error(transparent)]
    Manifest(#[from] manifest::ManifestError),
    /// `meta.json` or `exit.json` could not be written or read.
    #[error(transparent)]
    Meta(#[from] meta::MetaError),
    /// The journal could not be opened or appended to.
    #[error(transparent)]
    Journal(#[from] journal::JournalError),
    /// The PTY or the child process failed.
    #[error(transparent)]
    Process(#[from] process::ProcessError),
    /// The control socket failed.
    #[error(transparent)]
    Socket(#[from] socket::SocketError),
    /// A live worker already owns this session directory.
    ///
    /// Distinct from an ordinary bind failure because the recovery differs: a
    /// stale socket is removed and rebound, a live one is left alone.
    #[error("session {id} is already owned by a live worker")]
    AlreadyOwned {
        /// The session whose socket answered.
        id: String,
    },
}

/// Everything the worker needs that is not the launch material itself.
///
/// Assembled from [`SessionPaths`] plus the manifest's [`WorkerTuning`], so the
/// process that spawns a worker can tune it without a second channel and without
/// putting anything on the command line.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// The directory this worker owns.
    pub paths: SessionPaths,
    /// Journal cap and file placement.
    pub journal: journal::JournalConfig,
    /// Per-client queue bounds.
    pub queue: queue::QueueConfig,
    /// How a stop escalates from graceful to forced.
    pub stop: process::StopPolicy,
    /// Screen-model scrollback bound, in lines.
    pub scrollback_lines: usize,
    /// How much of that scrollback accompanies the snapshot on attach.
    ///
    /// Separate from `scrollback_lines` because they answer different
    /// questions: one is what the session keeps, the other is what a link
    /// carries every time a phone comes back from the background. See
    /// `docs/DECISION_SCROLLBACK.md`.
    pub attach_history: latch_term::HistoryPolicy,
    /// The only uid permitted to connect to the control socket.
    ///
    /// Normally the current effective uid. It is a parameter rather than an
    /// implicit `geteuid()` so the rejection path is reachable in a test without
    /// a second user account: bind for an owner that is not the caller, connect,
    /// and the connection must be refused.
    pub owner_uid: u32,
}

/// How long a `poll` waits before the loop looks around on its own.
///
/// The loop is event-driven; this only bounds how late it can notice something
/// no descriptor reports — a child that exited without closing the PTY, or a
/// stop escalation coming due.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the worker keeps serving after the socket file is gone.
///
/// A client that connected while the socket still existed is in the accept
/// backlog and is owed an answer, even though the session it wanted is over.
/// Without this window its `attach` gets neither a reply nor a close, which from
/// the client's side is indistinguishable from a worker that hung.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(250);

/// How long a caller keeps asking before it calls a session dead.
///
/// Sized against the longest a live worker can go without accepting: it flushes
/// its clients for [`BRIEF_FLUSH`] while ending, and otherwise returns to its
/// accept loop within [`POLL_INTERVAL`]. Comfortably past their sum, so the only
/// sessions that spend it are the ones genuinely gone.
pub const LIVENESS_PATIENCE: Duration = Duration::from_millis(500);

/// How many reads one pass takes off the PTY before handing the loop back.
///
/// A child that writes as fast as the worker reads — `while :; do echo x; done`
/// is enough — leaves the PTY perpetually readable, and draining it until
/// `WouldBlock` then never returns. Every other thing the loop does lives after
/// that drain: the session keeps a live worker that answers nothing. `latch
/// stop` is never read off the socket, connections are never accepted, and the
/// session reads as `lost` — so the one command that could end the flood is the
/// one the flood prevents, and `prune` offers to reclaim a running session.
///
/// Bounding the pass costs no throughput, because `poll` reports the PTY ready
/// again on the next turn. It only stops one descriptor from starving the rest.
const PTY_READS_PER_PASS: usize = 1;

/// Runs a worker to completion in the current process.
///
/// Returns when the child has exited, `exit.json` has been written, and the
/// socket has been removed — in that order, because a client that sees the
/// socket disappear must already be able to find the exit record.
pub fn run(config: WorkerConfig, mut manifest: LaunchManifest) -> Result<ChildExit, WorkerError> {
    config.paths.ensure()?;
    let id = config
        .paths
        .dir()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session")
        .to_owned();
    // Nesting is detectable from inside the session. Export before spawn so the
    // child's environment — and every process it starts — sees it.
    manifest
        .launch
        .env
        .insert(paths::SESSION_ID_ENV.to_owned(), id.clone());
    let meta = meta::derive(&id, &manifest.launch, &manifest.display, &timestamp());
    meta::write_once(&config.paths, &meta)?;

    let mut journal = journal::Journal::open(&config.paths.journal(), config.journal)?;
    let mut child = process::ChildProcess::spawn(&manifest.launch)?;
    let mut screen = Terminal::new(
        TerminalConfig::new(manifest.launch.size.into())
            .with_scrollback_limit(config.scrollback_lines),
    );

    let mut registry = Registry::new(Size {
        cols: manifest.launch.size.cols,
        rows: manifest.launch.size.rows,
    });
    registry.set_identity(meta.id.clone(), meta.name.clone());
    // Dropped rather than applied: nobody is attached yet, so the broadcast has
    // no recipients. It is called for the state it sets, not the effect.
    let _ = registry.set_title(meta.title.clone());

    let listener =
        socket::ControlListener::bind_for_owner(&config.paths.socket(), config.owner_uid)?;
    listener
        .as_listener()
        .set_nonblocking(true)
        .map_err(|source| socket::SocketError::Io {
            path: config.paths.socket(),
            source,
        })?;

    let mut clients: Vec<Client> = Vec::new();
    let mut output = queue::Fanout::new(config.queue);

    let mut pending_input = Vec::<u8>::new();

    // When the graceful signal was sent and its grace runs out, and whether the
    // forced one has followed. `None` is a session nobody has asked to stop.
    let mut stopping: Option<Instant> = None;
    let mut escalated = false;

    let exit = loop {
        wait_for_work(&listener, &child, &clients, !pending_input.is_empty());

        let mut stop_requested = None;
        for index in 0..clients.len() {
            let received = clients[index].conn.receive();
            for frame in received.frames {
                // Applied as each message is interpreted rather than batched at
                // the end of the pass. Two clients attaching in the same read
                // would otherwise see the second one's presence broadcast go out
                // before the first one's `attached` — a client learning that it
                // is in the session before being told it is in the session.
                let was_attached = clients[index].id.is_some();
                match handle_frame(index, frame, &mut clients, &mut registry) {
                    Handled::Effects(mut effects) => {
                        apply(
                            &mut effects,
                            &mut clients,
                            &mut child,
                            &mut screen,
                            config.attach_history,
                        )?;
                    }
                    Handled::Input(bytes) => queue_input(&mut pending_input, &bytes),
                    Handled::Stop { force } => {
                        stop_requested = Some(stop_requested.unwrap_or(false) || force)
                    }
                    Handled::Nothing => {}
                }
                if !was_attached {
                    if let Some(id) = clients[index].id.clone() {
                        output.register(id);
                    }
                }
            }
            let departed = received.closed || received.fatal.is_some();
            if let Some(error) = received.fatal {
                // Terminal for the connection and for nothing else. The child
                // never learns that a client sent something the codec refused.
                clients[index].conn.enqueue(
                    FrameType::Control,
                    &latch_protocol::control::encode(&ControlMessage::Error {
                        code: frame_error_code(&error).to_owned(),
                        message: error.to_string(),
                    }),
                );
                clients[index].conn.close_after_flush();
            } else if received.closed {
                clients[index].conn.close_after_flush();
            }
            // Settled here, the moment the departure is observed, rather than
            // when the connection is finally dropped. Clients are read in the
            // order they arrived, so a reconnect — the old socket closing and
            // the new one attaching, which the kernel reports together — reads
            // the departure first. Deferring the detach until after this pass
            // would answer the new attach `control_busy` on behalf of a client
            // this loop had already watched leave.
            if departed {
                if let Some(id) = clients[index].id.take() {
                    output.unregister(&id);
                    let mut effects = registry.detach(&id);
                    apply(
                        &mut effects,
                        &mut clients,
                        &mut child,
                        &mut screen,
                        config.attach_history,
                    )?;
                }
            }
        }

        // After the read pass, so everything one client sent in a single burst
        // reaches the child in one go rather than a keystroke per loop turn.
        drain_input(&mut child, &mut pending_input);
        pump_pty(
            &mut child,
            &mut screen,
            &mut journal,
            &mut clients,
            &mut output,
            &mut registry,
        )?;
        deliver_output(&mut clients, &mut output);
        // Catches the departures the read pass could not see: a client whose
        // socket failed on the way out rather than on the way in.
        reap(
            &mut clients,
            &mut output,
            &mut registry,
            &mut child,
            &mut screen,
        )?;
        // Arrivals are admitted last, so a connection accepted here has its
        // first message read on the next pass — after every departure already
        // visible to this one has been settled.
        accept_new(&listener, &mut clients);

        // A stop runs *through* the loop rather than instead of it.
        //
        // Waiting for the child inside a blocking call means nothing drains the
        // pty while the wait runs, and a child killed with a full pty buffer
        // cannot finish exiting until somebody reads it. The worker would then
        // wait for a child that is waiting for the worker: the session stops
        // answering, `latch stop` reports it `lost`, and the escalation lands on
        // a process group that can no longer be signalled. Keeping the loop
        // turning keeps the pty drained, so the child can die and be reaped by
        // the ordinary `try_wait` below.
        if let Some(force) = stop_requested.filter(|_| stopping.is_none()) {
            let mut announcement = registry.set_state(SessionState::Stopping);
            apply(
                &mut announcement,
                &mut clients,
                &mut child,
                &mut screen,
                config.attach_history,
            )?;
            if force {
                child.signal_group(config.stop.forced)?;
                stopping = Some(Instant::now());
                escalated = true;
            } else {
                child.signal_group(config.stop.graceful)?;
                stopping = Some(Instant::now() + config.stop.grace);
            }
        }
        // The grace interval has run out with the child still there: escalate,
        // once, and go on draining until it is gone.
        if let Some(deadline) = stopping {
            if !escalated && Instant::now() >= deadline {
                child.signal_group(config.stop.forced)?;
                escalated = true;
            }
        }
        if let Some(exit) = child.try_wait()? {
            // Drained first: the bytes the child wrote just before exiting are
            // the ones a person most wants to read.
            pump_pty(
                &mut child,
                &mut screen,
                &mut journal,
                &mut clients,
                &mut output,
                &mut registry,
            )?;
            deliver_output(&mut clients, &mut output);
            break exit;
        }
        flush(&mut clients, Duration::ZERO);
    };

    finish(
        &listener,
        &config,
        exit,
        &mut clients,
        &mut registry,
        &mut child,
        &mut screen,
    )?;
    Ok(exit)
}

/// A connected client, and its attachment if the handshake has landed.
struct Client {
    conn: socket::ClientConnection,
    /// `None` until `attach` succeeds. A connection that has not attached
    /// receives nothing: it is not in the registry, so no broadcast names it.
    id: Option<registry::AttachmentId>,
}

/// What one inbound frame asked the worker to do.
enum Handled {
    Effects(Vec<Effect>),
    Input(Vec<u8>),
    Stop { force: bool },
    Nothing,
}

/// Most unwritten controller input the worker will hold, in bytes.
///
/// A megabyte is a very large paste and far more than anyone types. Past it the
/// child is not reading its terminal, and the kernel's own line discipline
/// drops input in exactly that situation — this bound is the same policy, held
/// one layer further out so the memory is bounded here too.
const MAX_PENDING_INPUT: usize = 1024 * 1024;

/// Adds controller input to what is waiting for the child.
///
/// Buffered rather than written on the spot because the PTY master is
/// non-blocking: a direct write can accept part of a chunk, or none of it, and a
/// worker that ignores the returned count silently eats keystrokes. Dropped
/// input in a terminal is the kind of bug that gets blamed on the network.
fn queue_input(pending: &mut Vec<u8>, bytes: &[u8]) {
    if pending.len().saturating_add(bytes.len()) > MAX_PENDING_INPUT {
        return;
    }
    pending.extend_from_slice(bytes);
}

/// Writes as much pending input as the PTY will take.
///
/// Whatever is left stays queued and the next `poll` waits for the PTY to become
/// writable, so a child that is not reading slows its own input and nothing
/// else.
fn drain_input(child: &mut process::ChildProcess, pending: &mut Vec<u8>) {
    let mut written = 0;
    while written < pending.len() {
        match child.write(&pending[written..]) {
            Ok(0) => break,
            Ok(wrote) => written += wrote,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            // Includes `WouldBlock`. The remainder stays queued; it is not lost
            // and it is not retried in a spin.
            Err(_) => break,
        }
    }
    pending.drain(..written);
}

/// Waits until a descriptor has something, or the interval elapses.
fn wait_for_work(
    listener: &socket::ControlListener,
    child: &process::ChildProcess,
    clients: &[Client],
    input_pending: bool,
) {
    let mut fds = Vec::with_capacity(clients.len() + 2);
    fds.push(libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(listener.as_listener()),
        events: libc::POLLIN,
        revents: 0,
    });
    fds.push(libc::pollfd {
        fd: child.pty_fd(),
        events: if input_pending {
            libc::POLLIN | libc::POLLOUT
        } else {
            libc::POLLIN
        },
        revents: 0,
    });
    for client in clients {
        let mut events = libc::POLLIN;
        if client.conn.has_pending_writes() {
            events |= libc::POLLOUT;
        }
        fds.push(libc::pollfd {
            fd: client.conn.fd(),
            events,
            revents: 0,
        });
    }
    // SAFETY: `fds` is a valid array of the stated length for the call's
    // duration, and every descriptor in it is owned by a live value.
    unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            fds.len() as libc::nfds_t,
            POLL_INTERVAL.as_millis() as libc::c_int,
        );
    }
}

/// Accepts everything waiting, refusing peers that are not the owner.
fn accept_new(listener: &socket::ControlListener, clients: &mut Vec<Client>) {
    loop {
        match listener.accept() {
            Ok(accepted) => match socket::ClientConnection::new(accepted) {
                Ok(conn) => clients.push(Client { conn, id: None }),
                Err(_) => continue,
            },
            // Closed already, and answered with nothing. There is nothing to
            // negotiate with a peer that is not permitted to be here.
            Err(socket::SocketError::PeerRejected { .. }) => continue,
            Err(socket::SocketError::NoPeerCredentials(_)) => continue,
            Err(_) => return,
        }
    }
}

/// Interprets one frame from one client.
fn handle_frame(
    index: usize,
    frame: socket::OwnedFrame,
    clients: &mut [Client],
    registry: &mut Registry,
) -> Handled {
    match frame.frame_type {
        FrameType::TerminalInput => {
            // Exactly one attachment may write. A watcher's input is discarded
            // here rather than filtered downstream, so there is one place where
            // "two uncontrolled writers" is made impossible.
            match &clients[index].id {
                Some(id) if registry.controller().as_ref() == Some(id) => {
                    Handled::Input(frame.payload)
                }
                _ => Handled::Nothing,
            }
        }
        // A client has no reason to send terminal output. Ignored rather than
        // treated as an error: it costs nothing and an unknown-but-well-formed
        // frame is the shape every forward-compatible change takes.
        FrameType::TerminalOutput => Handled::Nothing,
        FrameType::Control => match latch_protocol::control::decode(&frame.payload) {
            Ok(message) => handle_control(index, message, clients, registry),
            Err(error) => {
                refuse(&mut clients[index], error.code(), &error.to_string());
                Handled::Nothing
            }
        },
    }
}

/// Interprets one control message from one client.
fn handle_control(
    index: usize,
    message: ControlMessage,
    clients: &mut [Client],
    registry: &mut Registry,
) -> Handled {
    let attached = clients[index].id.clone();
    match message {
        ControlMessage::Attach {
            protocol,
            mode,
            steal,
            client,
            size,
        } => {
            if attached.is_some() {
                // Confused, not dangerous. The first attachment is left exactly
                // as it was and this connection is told why it is going away.
                refuse(
                    &mut clients[index],
                    "already_attached",
                    "this connection is already attached",
                );
                return Handled::Nothing;
            }
            match registry.attach(registry::AttachRequest {
                protocol,
                mode,
                steal,
                client: sanitize(client),
                size,
            }) {
                Ok(outcome) => {
                    clients[index].id = Some(outcome.id);
                    Handled::Effects(outcome.effects)
                }
                Err(rejection) => {
                    refuse(
                        &mut clients[index],
                        rejection.code(),
                        &rejection.to_string(),
                    );
                    Handled::Nothing
                }
            }
        }
        ControlMessage::Resize { cols, rows, pin } => match (attached, pin) {
            (Some(_), Some(pin)) => Handled::Effects(registry.set_size(Size { cols, rows }, pin)),
            (Some(id), None) => Handled::Effects(registry.resize(&id, Size { cols, rows })),
            (None, _) => Handled::Nothing,
        },
        ControlMessage::ControlRequest { steal } => match attached {
            Some(id) => match registry.request_control(&id, steal) {
                Ok(effects) => Handled::Effects(effects),
                Err(rejection) => {
                    refuse(
                        &mut clients[index],
                        rejection.code(),
                        &rejection.to_string(),
                    );
                    Handled::Nothing
                }
            },
            None => Handled::Nothing,
        },
        // `latch stop` is this and nothing else. There is no ninth control
        // message, and no pid is read from anywhere to serve it.
        message if is_stop_request(&message) => Handled::Stop {
            force: stop_request_force(&message).unwrap_or(false),
        },
        // Everything else in the vocabulary is worker-to-client. A client that
        // sends one is ignored rather than disconnected.
        _ => Handled::Nothing,
    }
}

/// Sends an `error` and closes, which is the only way an error ever ends.
fn refuse(client: &mut Client, code: &str, message: &str) {
    client.conn.enqueue(
        FrameType::Control,
        &latch_protocol::control::encode(&ControlMessage::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    );
    client.conn.close_after_flush();
}

/// Performs a registry's effects, in the order it returned them.
fn apply(
    effects: &mut Vec<Effect>,
    clients: &mut [Client],
    child: &mut process::ChildProcess,
    screen: &mut Terminal,
    history: HistoryPolicy,
) -> Result<(), WorkerError> {
    for effect in effects.drain(..) {
        match effect {
            Effect::Send { to, message } => {
                let payload = latch_protocol::control::encode(&message);
                if let Some(client) = find(clients, &to) {
                    client.conn.enqueue(FrameType::Control, &payload);
                }
            }
            Effect::Broadcast { message } => {
                let payload = latch_protocol::control::encode(&message);
                for client in clients.iter_mut().filter(|client| client.id.is_some()) {
                    client.conn.enqueue(FrameType::Control, &payload);
                }
            }
            Effect::Resize { size } => {
                child.resize(size)?;
                screen.resize(to_term_size(size));
            }
            // The snapshot is not a message. It goes out as ordinary terminal
            // output, which is what lets a reconnecting client run the same code
            // path as a first-time one.
            //
            // This effect is produced by `Registry::attach` and by nothing else,
            // which is why the history policy can be applied here unconditionally.
            // The other place a snapshot is sent — a client whose queue
            // overflowed — calls `Screen::snapshot` directly and deliberately
            // carries no history: that client already received it when it
            // attached, and a link slow enough to overflow is the last one to
            // send scrollback to twice.
            Effect::Snapshot { to } => {
                let bytes = screen.attach_snapshot(history);
                if let Some(client) = find(clients, &to) {
                    client.conn.enqueue(FrameType::TerminalOutput, &bytes);
                }
            }
            Effect::Close { to } => {
                if let Some(client) = find(clients, &to) {
                    client.conn.close_after_flush();
                }
            }
        }
    }
    Ok(())
}

fn find<'a>(clients: &'a mut [Client], id: &registry::AttachmentId) -> Option<&'a mut Client> {
    clients
        .iter_mut()
        .find(|client| client.id.as_ref() == Some(id))
}

/// Moves whatever the child has written into the screen, the journal, and every
/// attachment.
///
/// The read is bounded by the buffer and by what the PTY has; it never waits on
/// a client, because every client's bytes go into that client's own queue.
fn pump_pty(
    child: &mut process::ChildProcess,
    screen: &mut Terminal,
    journal: &mut journal::Journal,
    clients: &mut [Client],
    output: &mut queue::Fanout,
    registry: &mut Registry,
) -> Result<(), WorkerError> {
    let mut buffer = [0u8; 8192];
    for _ in 0..PTY_READS_PER_PASS {
        match child.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                let bytes = &buffer[..read];
                // The screen first: it is the substrate every attachment reads
                // from, and a snapshot taken mid-chunk must never be behind the
                // bytes already sent live.
                screen.advance(bytes);
                let _ = journal.append(bytes);
                for id in output.publish(bytes) {
                    // Output accepted by the socket but not its kernel is stale
                    // as well. The snapshot replacing it is taken once after
                    // the pass — see `resync_stalled_clients`.
                    if let Some(client) = find(clients, &id) {
                        client.conn.discard_pending_writes();
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(process::ProcessError::Io(error).into()),
        }
    }
    // Budget spent, or the pty drained. Either way the loop is handed back to
    // the control plane; `poll` reports the pty ready again immediately, so the
    // only thing given up is this pass, not the bytes.
    resync_stalled_clients(screen, output);
    record_queue_stats(clients, output, registry);
    Ok(())
}

/// Gives every client that fell behind this pass the current screen.
///
/// Once per pass, not once per overflow. A flooding child overflows a slow
/// client's queue on every chunk read, and answering each one separately means
/// a full screen serialization per chunk per client — the worker spends its pass
/// building snapshots that the next chunk immediately invalidates, and a pass
/// that should take milliseconds takes seconds. One snapshot at the end of the
/// pass is also the *better* one: it is later, and the queue behind it was going
/// to be discarded regardless.
fn resync_stalled_clients(screen: &mut Terminal, output: &mut queue::Fanout) {
    let stalled = output.pending_resyncs();
    if stalled.is_empty() {
        return;
    }
    let snapshot = screen.snapshot();
    for id in stalled {
        output.resynchronize(&id, snapshot.clone());
    }
}

fn record_queue_stats(clients: &[Client], output: &queue::Fanout, registry: &mut Registry) {
    for id in clients.iter().filter_map(|client| client.id.as_ref()) {
        if let Some(stats) = output.stats(id) {
            registry.record_stats(id, stats);
        }
    }
}

/// Offers at most one queued output chunk to each socket.
///
/// Moving every chunk into `ClientConnection` would simply recreate an
/// unbounded queue below the bounded one. A client has at most its configured
/// queue plus one encoded frame awaiting the kernel.
fn deliver_output(clients: &mut [Client], output: &mut queue::Fanout) {
    for client in clients.iter_mut() {
        if client.id.is_some() && !client.conn.has_pending_writes() {
            if let Some(chunk) = client.id.as_ref().and_then(|id| output.pop(id)) {
                client.conn.enqueue(FrameType::TerminalOutput, &chunk);
            }
        }
    }
}

/// Writes what it can to every client and drops the ones that are done.
///
/// A departure is a detach, and a detach is what releases control. There is no
/// goodbye message and no lease timer: locally the operating system reports the
/// departure precisely, so a timer could only be wrong relative to it (D5).
fn reap(
    clients: &mut Vec<Client>,
    output: &mut queue::Fanout,
    registry: &mut Registry,
    child: &mut process::ChildProcess,
    screen: &mut Terminal,
) -> Result<(), WorkerError> {
    for client in clients.iter_mut() {
        client.conn.flush_some();
    }
    let mut departed = Vec::new();
    clients.retain(|client| {
        if !client.conn.is_finished() {
            return true;
        }
        if let Some(id) = &client.id {
            departed.push(id.clone());
        }
        false
    });
    for id in departed {
        output.unregister(&id);
        let mut effects = registry.detach(&id);
        // A departure produces broadcasts and possibly a resize, never a
        // snapshot: nobody arrived. `NONE` states that rather than threading a
        // policy through a path that cannot use one.
        apply(&mut effects, clients, child, screen, HistoryPolicy::NONE)?;
    }
    Ok(())
}

/// A short bound on how long the worker will wait for its own writes to land.
const BRIEF_FLUSH: Duration = Duration::from_millis(250);

/// Pushes queued bytes out, giving up after `budget`.
///
/// Bounded on purpose. A client that has stopped reading must not be able to
/// delay the worker's own shutdown any more than it can delay the PTY.
fn flush(clients: &mut [Client], budget: Duration) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let mut pending = false;
        for client in clients.iter_mut() {
            client.conn.flush_some();
            pending |= client.conn.has_pending_writes() && !client.conn.is_finished();
        }
        if !pending || std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Ends the session: tell everyone, record the exit, then stop answering.
///
/// The order is load-bearing. `session.exited` reaches attachments before they
/// are closed, `exit.json` exists before the socket disappears, and the socket
/// disappears before the worker stops answering — so a client that notices the
/// session end can already find out how it ended, whichever of the three it
/// noticed first.
fn finish(
    listener: &socket::ControlListener,
    config: &WorkerConfig,
    exit: ChildExit,
    clients: &mut Vec<Client>,
    registry: &mut Registry,
    child: &mut process::ChildProcess,
    screen: &mut Terminal,
) -> Result<(), WorkerError> {
    let paths = &config.paths;
    let record = meta::ExitRecord::new(&exit, timestamp());
    let mut effects = registry.mark_exited(ControlMessage::SessionExited {
        code: record.code,
        signal: record.signal.clone(),
        at: record.exited_at.clone(),
    });
    // `session.exited` and the closes that follow it. No client is attaching.
    apply(&mut effects, clients, child, screen, HistoryPolicy::NONE)?;
    flush(clients, BRIEF_FLUSH);

    // The last screen, written before the exit record so that a session which
    // reads as `exited` always has one. It is the same snapshot a live worker
    // would have sent a client attaching a moment earlier — the point of
    // keeping it is that an exited session can still be looked at without
    // replaying the journal into a fresh emulator, which is the byte-tail
    // reconstruction the screen model exists to avoid.
    //
    // A failure here is reported and does not stop the exit: the exit record is
    // what the rest of the system derives state from, and losing a courtesy is
    // not a reason to lose that.
    if let Err(error) = meta::write_screen(paths, &screen.attach_snapshot(config.attach_history)) {
        eprintln!("latch: cannot record the final screen: {error}");
    }
    meta::write_exit(paths, &record)?;
    listener.remove()?;

    // Whoever was already in the accept backlog is owed an answer. They cannot
    // be served — the session is over — but "refused" and "never replied" are
    // very different things to be on the receiving end of.
    let deadline = std::time::Instant::now() + DRAIN_AFTER_EXIT;
    while std::time::Instant::now() < deadline {
        accept_new(listener, clients);
        for client in clients.iter_mut() {
            let received = client.conn.receive();
            // Not decoded: the answer is the same whatever it asked for, and a
            // worker whose session has ended has no business parsing anything.
            let asked_for_something = received
                .frames
                .iter()
                .any(|frame| frame.frame_type == FrameType::Control);
            if asked_for_something {
                refuse(client, "session_exited", "the session's child has exited");
            } else if received.closed || received.fatal.is_some() {
                client.conn.close_after_flush();
            }
        }
        for client in clients.iter_mut() {
            client.conn.flush_some();
        }
        clients.retain(|client| !client.conn.is_finished());
        if clients.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Restricts externally supplied client metadata to printable characters.
///
/// Sanitized here, at the boundary where it arrives, rather than where it is
/// rendered. A client name flows into other clients' presence lists and later
/// into `latch inspect`; sanitizing at render means every future call site is a
/// new chance to forget.
fn sanitize(client: ClientInfo) -> ClientInfo {
    ClientInfo {
        kind: sanitize_field(&client.kind),
        name: sanitize_field(&client.name),
    }
}

/// Bound on a display field from a client, in characters.
const MAX_CLIENT_FIELD: usize = 64;

fn sanitize_field(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_FIELD)
        .collect()
}

/// The stable code a framing failure is reported to the peer under.
fn frame_error_code(error: &socket::SocketError) -> &'static str {
    match error {
        socket::SocketError::Frame(frame) => frame.code(),
        socket::SocketError::Control(control) => control.code(),
        _ => "io_error",
    }
}

/// Reads a launch manifest from stdin and runs a worker for `session_dir`.
///
/// This is what the hidden `latch worker` subcommand calls. The manifest arrives
/// on stdin rather than in argv because it carries launch material: environment,
/// tokens, anything the caller needs the child to see. `argv` is world-readable
/// on a shared machine; stdin is not.
///
/// The process detaches itself before doing anything else — see
/// [`spawn::ensure_detached`]. Detachment is the worker's own responsibility and
/// not the caller's, so that a worker started by any means is equally immune to
/// its launcher's fate.
pub fn run_from_stdin(session_dir: &Path) -> Result<ChildExit, WorkerError> {
    spawn::ensure_detached().map_err(|error| {
        WorkerError::Process(process::ProcessError::Pty(std::io::Error::other(
            error.to_string(),
        )))
    })?;
    let manifest = manifest::read(std::io::stdin())?;
    // SAFETY: geteuid cannot fail.
    let owner_uid = manifest
        .tuning
        .owner_uid
        .unwrap_or_else(|| unsafe { libc::geteuid() });
    let paths = SessionPaths::new(session_dir);
    run(
        WorkerConfig {
            journal: journal::JournalConfig {
                cap_bytes: manifest.tuning.journal_cap_bytes,
            },
            queue: queue::QueueConfig {
                capacity_bytes: manifest.tuning.queue_capacity_bytes,
                capacity_chunks: manifest.tuning.queue_capacity_chunks,
            },
            stop: process::StopPolicy {
                grace: Duration::from_millis(manifest.tuning.stop_grace_ms),
                ..process::StopPolicy::default()
            },
            scrollback_lines: manifest.tuning.scrollback_lines,
            attach_history: HistoryPolicy {
                max_lines: manifest.tuning.attach_history_lines,
                max_bytes: manifest.tuning.attach_history_bytes,
            },
            paths,
            owner_uid,
        },
        manifest,
    )
}

/// Derives a session's state from the filesystem and the socket.
///
/// State is derived, never stored (see `docs/ARCHITECTURE_RULES.md`):
///
/// ```text
/// socket accepts a connection      -> ask the worker (creating | running | stopping)
/// exit.json present, socket gone   -> exited
/// neither                          -> lost
/// ```
///
/// There is deliberately no function here that reads a status field, because
/// there is deliberately no status field to read.
pub fn derive_state(paths: &SessionPaths) -> Result<SessionState, WorkerError> {
    if socket::is_live(&paths.socket()) {
        return Ok(SessionState::Running);
    }
    if meta::read_exit(paths)?.is_some() {
        return Ok(SessionState::Exited);
    }
    // Neither answered, which is the one combination worth a second look:
    // `Lost` is the state `latch prune` reclaims, and a refused connection is
    // not proof of death (see `socket::is_live_within`). Both facts are read
    // again, because a worker that was mid-flush a moment ago may have finished
    // writing its exit record since.
    if socket::is_live_within(&paths.socket(), LIVENESS_PATIENCE) {
        return Ok(SessionState::Running);
    }
    if meta::read_exit(paths)?.is_some() {
        return Ok(SessionState::Exited);
    }
    Ok(SessionState::Lost)
}

/// Whether a control message from a client is a request to stop the session.
///
/// `latch stop` needs no ninth control message. The vocabulary is eight messages
/// on purpose — each one multiplies across the per-language fixture suites — and
/// `session.update` already carries a lifecycle state. A client sending
/// `session.update { state: stopping }` is asking this worker to stop its own
/// child's process group; `force: true` skips the graceful interval. The worker
/// answers by broadcasting the same state and, once the child is gone,
/// `session.exited`.
///
/// A `session.update` that carries no `state` is presence or title metadata and
/// is not a stop.
pub fn is_stop_request(message: &ControlMessage) -> bool {
    stop_request_force(message).is_some()
}

fn stop_request_force(message: &ControlMessage) -> Option<bool> {
    match message {
        ControlMessage::SessionUpdate {
            state: Some(SessionState::Stopping),
            force,
            ..
        } => Some(force.unwrap_or(false)),
        _ => None,
    }
}

/// Converts a wire size into a screen-model size.
///
/// The two leaf crates each declare their own `Size` because neither may depend
/// on the other. This crate depends on both, so the conversion belongs here and
/// nowhere else.
pub fn to_term_size(size: Size) -> latch_term::Size {
    latch_term::Size::new(size.cols, size.rows)
}

/// Converts a screen-model size into a wire size.
pub fn to_wire_size(size: latch_term::Size) -> Size {
    Size {
        cols: size.cols,
        rows: size.rows,
    }
}

fn timestamp() -> String {
    // SAFETY: `time` and `gmtime_r` write only to the supplied initialized
    // storage. UTC avoids the worker's display metadata changing with locale.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut utc = std::mem::zeroed::<libc::tm>();
        if libc::gmtime_r(&now, &mut utc).is_null() {
            return "1970-01-01T00:00:00Z".to_owned();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            utc.tm_year + 1900,
            utc.tm_mon + 1,
            utc.tm_mday,
            utc.tm_hour,
            utc.tm_min,
            utc.tm_sec
        )
    }
}
