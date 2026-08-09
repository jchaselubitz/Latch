//! Attach: connect, enter raw mode, forward bytes and resizes, restore on exit.
//!
//! Restoration must hold on every path — normal exit, `SIGINT`, `SIGTERM`,
//! `SIGHUP`, and a panic. A terminal left in raw mode is the kind of failure
//! that makes someone stop trusting the tool. The guard in [`crate::cli::term`]
//! is what makes restoration unskippable; this module is what uses it.
//!
//! # The transport is allowed to die (M2)
//!
//! M2 puts this client on the far end of an SSH tunnel from a phone, so the link
//! failing is ordinary rather than exceptional. Three properties follow, and all
//! three are about the client rather than the worker:
//!
//! * **Death is detected, never waited out.** Every read this client performs is
//!   bounded. A worker that accepts a connection and then says nothing is what a
//!   half-open transport looks like from here, and an unbounded read would turn
//!   that into a frozen phone screen instead of a report.
//! * **Reconnect is just attach.** The protocol has no replay round trip, so
//!   [`attach_with_retry`] loops over the *same* attach that a person runs by
//!   hand. There is no sequence-number bookkeeping and no resume handshake here;
//!   if reconnection ever appeared to need one, the worker's snapshot path would
//!   be the thing at fault.
//! * **Reconnection never escalates.** A watcher reconnects as a watcher, and a
//!   controller that did not ask to steal never starts asking. Retrying is a
//!   response to a broken link, not a change of intent.
//!
//! # Connection state is shown, not inferred
//!
//! On a phone, "is this frozen, or is the agent thinking?" is a real and
//! frequent question. It costs one line of status to answer, so this client
//! prints one on every connection-state change ([`ConnectionState`]). The line
//! is safe to write into the session's screen because a snapshot begins with a
//! hard reset: the next successful attach repaints from scratch and erases it.
//!
//! The absence of a line is meaningful too, and only because losses are reported
//! promptly: no status line means the link is alive and the quiet is the agent's.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use latch_protocol::control::{self, AttachMode, ClientInfo, ControlMessage};
use latch_protocol::frame::FrameType;
use latch_protocol::PROTOCOL_VERSION;

use crate::cli::term::{self, RawModeGuard};
use crate::worker::meta;
use crate::worker::paths::{LatchHome, SessionId};
use crate::worker::socket::{FrameStream, OwnedFrame, SocketError};

/// How long the worker has to answer an `attach` before the link is called dead.
///
/// Generous, because a phone's first attach can arrive alongside a cell-network
/// handoff, but finite, because the alternative to a bound is a hang.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a bounded read waits before checking its deadline again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How an attach was requested.
#[derive(Debug, Clone)]
pub struct AttachOptions {
    /// Where sessions live for this invocation.
    pub home: LatchHome,
    /// Session id or name. `None` means the most recently active session.
    pub session: Option<String>,
    /// Attach without taking input control.
    pub watch: bool,
    /// Take input control even if another attachment holds it.
    pub steal: bool,
}

/// How an attach ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
    /// The client detached; the session keeps running.
    Detached,
    /// The session's child exited while attached.
    SessionExited {
        /// Exit status, when the child exited normally.
        code: Option<i32>,
    },
    /// The session had already exited; its final screen was shown and nothing
    /// was attached to.
    ///
    /// Distinct from [`Self::SessionExited`] because nothing was ever live: no
    /// raw mode was entered, no input was forwarded, and there was no worker on
    /// the other end. A caller that reattaches in a loop needs to tell the two
    /// apart, or it will reattach forever to a session that ended before it
    /// arrived.
    AlreadyExited {
        /// Exit status, when the child exited normally.
        code: Option<i32>,
    },
}

/// How reconnection is paced, and where it stops.
///
/// The bound is the point. A client that retries forever is indistinguishable
/// from a frozen one, which is precisely the confusion this milestone exists to
/// remove — so `--retry` always ends, and says so when it does.
///
/// The defaults suit the M2 transport, which is a local Unix socket reached over
/// somebody else's SSH tunnel: a socket that has not come back within a few
/// seconds is not coming back, because nothing about it is subject to network
/// weather. M4's transport is genuinely networked and will want a longer, more
/// patient policy; that is a reason to keep this a value rather than constants
/// baked into the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Reconnection attempts made after a loss before giving up.
    pub attempts: u32,
    /// The wait before the first reconnection attempt.
    pub initial_backoff: Duration,
    /// The ceiling the backoff doubles up to.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// Never reconnects; the first loss is reported and the client exits.
    pub const NONE: Self = Self {
        attempts: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
    };

    /// The wait before the 1-based `attempt`, doubling up to [`Self::max_backoff`].
    ///
    /// Backoff exists so a reconnect does not race the worker's own cleanup: a
    /// link that dropped is frequently still holding an attachment the worker has
    /// not reaped yet, and hammering it just collects `control_busy` against
    /// oneself.
    pub fn backoff(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let doubling = self
            .initial_backoff
            .checked_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX))
            .unwrap_or(self.max_backoff);
        doubling.min(self.max_backoff)
    }

    /// The longest this policy can spend waiting between attempts.
    ///
    /// Reported when giving up, because "it stopped trying" is only reassuring
    /// alongside how long it tried for.
    pub fn budget(&self) -> Duration {
        (1..=self.attempts).map(|n| self.backoff(n)).sum()
    }
}

/// What the client is doing, in the words shown to the person watching.
///
/// One line each. This is the whole of the connection-state surface: anything
/// larger would have to be painted and maintained inside the session's own
/// screen, and a client that draws furniture over the agent's output has made
/// the terminal experience worse to describe itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// A connection was accepted and the screen is on its way.
    Connected {
        /// The session attached to.
        session: String,
        /// Whether this attachment can type.
        watching: bool,
    },
    /// The link failed and no reconnection will be attempted.
    Lost {
        /// The session that is still running without us.
        session: String,
        /// What the transport reported.
        reason: String,
    },
    /// The link failed and a reconnection is scheduled.
    Reconnecting {
        /// 1-based attempt about to be made.
        attempt: u32,
        /// How many attempts the policy allows.
        of: u32,
        /// The wait before making it.
        delay: Duration,
        /// What the transport reported.
        reason: String,
    },
    /// A reconnection succeeded; the screen is being restored.
    Reconnected {
        /// Which attempt worked.
        attempt: u32,
    },
    /// Reconnection is over and the client is exiting.
    GaveUp {
        /// How many attempts were made.
        attempts: u32,
        /// What the last one reported.
        reason: String,
    },
    /// The session had already ended; its last screen is what is above.
    Ended {
        /// The session that ended.
        session: String,
        /// How it ended, in words.
        ended: String,
    },
}

impl ConnectionState {
    /// The line as it appears on the screen, without decoration.
    pub fn line(&self) -> String {
        match self {
            Self::Connected { session, watching } => format!(
                "attached to {session}{} — waiting for the screen",
                if *watching { " (watching)" } else { "" }
            ),
            Self::Lost { session, reason } => format!(
                "connection lost: {reason} — {session} is still running; \
                 reattach with `latch attach {session}`"
            ),
            Self::Reconnecting {
                attempt,
                of,
                delay,
                reason,
            } => format!(
                "connection lost: {reason} — reconnecting, attempt {attempt} of {of} in {}",
                human(*delay)
            ),
            Self::Reconnected { attempt } => {
                format!("reconnected on attempt {attempt} — restoring the screen")
            }
            Self::GaveUp { attempts, reason } => {
                format!("gave up after {attempts} reconnection attempts: {reason}")
            }
            Self::Ended { session, ended } => format!(
                "{session} {ended}; this is its last screen — \
                 reclaim it with `latch prune`"
            ),
        }
    }
}

/// Attaches this terminal to a session, once.
///
/// Enters raw mode, forwards input and `SIGWINCH`, renders output, and restores
/// the terminal on every exit path. From the terminal's perspective this must
/// behave exactly like a directly running interactive program.
///
/// A transport failure is an error rather than a quiet return: the session
/// outlives it, so the difference between "you detached" and "the link died"
/// is the difference between being done and needing to reattach.
pub fn attach(options: AttachOptions) -> anyhow::Result<AttachOutcome> {
    attach_with_retry(options, RetryPolicy::NONE)
}

/// Attaches, reconnecting on transport failure until `policy` runs out.
///
/// The loop body is [`attach`]'s body: reconnect is just attach, so a client
/// that survives a dropped link and a person who types the command again arrive
/// through exactly the same code, and there is no second path for a broken
/// snapshot to hide in.
pub fn attach_with_retry(
    options: AttachOptions,
    policy: RetryPolicy,
) -> anyhow::Result<AttachOutcome> {
    let mut attempt = 0u32;
    loop {
        match attach_once(&options, (attempt > 0).then_some(attempt)) {
            Ok(outcome) => return Ok(outcome),
            Err(Interruption::Fatal(error)) => return Err(error),
            Err(Interruption::Transport { session, reason }) => {
                attempt += 1;
                if attempt > policy.attempts {
                    if policy.attempts == 0 {
                        show(&ConnectionState::Lost {
                            session: session.clone(),
                            reason: reason.clone(),
                        });
                        bail!(
                            "connection to session {session} lost: {reason}; \
                             the session keeps running — reattach with `latch attach {session}`"
                        );
                    }
                    show(&ConnectionState::GaveUp {
                        attempts: policy.attempts,
                        reason: reason.clone(),
                    });
                    bail!(
                        "--retry gave up after {} reconnection attempts over {}: {reason}; \
                         session {session} was not reachable — reattach with \
                         `latch attach {session}` when the link is back",
                        policy.attempts,
                        human(policy.budget())
                    );
                }
                let delay = policy.backoff(attempt);
                show(&ConnectionState::Reconnecting {
                    attempt,
                    of: policy.attempts,
                    delay,
                    reason,
                });
                std::thread::sleep(delay);
            }
        }
    }
}

/// Why an attach stopped, split by whether reconnecting could possibly help.
enum Interruption {
    /// The link failed. The session is unaffected, so another attach may work.
    Transport {
        /// The session this client was talking to.
        session: String,
        /// What the transport reported.
        reason: String,
    },
    /// Nothing about the transport is wrong, so retrying would only repeat it.
    Fatal(anyhow::Error),
}

impl Interruption {
    fn transport(session: &SessionId, reason: impl std::fmt::Display) -> Self {
        Self::Transport {
            session: session.to_string(),
            reason: reason.to_string(),
        }
    }
}

fn attach_once(
    options: &AttachOptions,
    reconnect_attempt: Option<u32>,
) -> Result<AttachOutcome, Interruption> {
    let id = match options.session.as_deref() {
        Some(session) => resolve_session(&options.home, session),
        None => most_recent_session(&options.home),
    }
    .map_err(Interruption::Fatal)?;
    let paths = options.home.session(&id);

    // An exited session is not a broken link, and retrying one would turn a
    // finished session into a client that waits for it to come back. It is
    // still worth showing: the last thing an agent printed is frequently the
    // whole reason you picked up the phone, and it is still on disk until
    // `latch prune` reclaims it. Read-only, because there is nothing left to
    // type at.
    if let Ok(Some(exit)) = meta::read_exit(&paths) {
        return show_final_screen(&id, &paths, &exit).map_err(Interruption::Fatal);
    }

    let stream = std::os::unix::net::UnixStream::connect(paths.socket())
        .map_err(|error| Interruption::transport(&id, error))?;
    let mut stream = FrameStream::new(stream);
    let mode = if options.watch {
        AttachMode::Watch
    } else {
        AttachMode::Control
    };
    let request = ControlMessage::Attach {
        protocol: PROTOCOL_VERSION,
        mode,
        // Never escalated on reconnect: `steal` is what the person asked for,
        // and a link that dropped did not change their mind about it.
        steal: options.steal,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: std::env::var("TERM").unwrap_or_else(|_| "terminal".to_owned()),
        },
        size: term::terminal_size(libc::STDOUT_FILENO),
    };
    send(
        &mut stream,
        &id,
        FrameType::Control,
        &control::encode(&request),
    )?;

    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let Some(reply) = recv_before(&mut stream, &id, deadline)? else {
        return Err(Interruption::transport(
            &id,
            "the session closed before accepting the attachment",
        ));
    };
    if reply.frame_type != FrameType::Control {
        return Err(Interruption::Fatal(anyhow::anyhow!(
            "session {id} sent output before accepting the attachment"
        )));
    }
    let message = control::decode(&reply.payload)
        .with_context(|| format!("session {id} sent an undecodable attach response"))
        .map_err(Interruption::Fatal)?;
    match message {
        ControlMessage::Attached { .. } => {
            match reconnect_attempt {
                Some(attempt) => show(&ConnectionState::Reconnected { attempt }),
                None => show(&ConnectionState::Connected {
                    session: id.to_string(),
                    watching: options.watch,
                }),
            }
            if stdin_is_terminal() {
                run_interactive(stream, &id, options.watch)
            } else {
                Ok(AttachOutcome::Detached)
            }
        }
        // A busy controller is transient by nature: the ordinary cause on
        // reconnect is the worker not having reaped this client's own previous
        // attachment yet. Retrying waits it out; escalating to `steal` would
        // take control that was never asked for.
        ControlMessage::Error { ref code, .. } if code == "control_busy" => {
            Err(Interruption::transport(&id, message_text(&message)))
        }
        ControlMessage::Error { code, message } => {
            Err(Interruption::Fatal(anyhow::anyhow!("{code}: {message}")))
        }
        message => Err(Interruption::Fatal(anyhow::anyhow!(
            "session {id} sent `{}` instead of an attachment response",
            message.discriminator()
        ))),
    }
}

/// Paints the screen an exited session left behind, then says how it ended.
///
/// The bytes are the same snapshot a live worker would have sent, so this path
/// renders through the same mechanism as an ordinary attach rather than
/// reconstructing anything. Nothing is entered into raw mode: there is no
/// child to type at, and a terminal put into raw mode for a read-only view is
/// a terminal that can be left broken for no benefit at all.
fn show_final_screen(
    id: &SessionId,
    paths: &crate::worker::paths::SessionPaths,
    exit: &meta::ExitRecord,
) -> anyhow::Result<AttachOutcome> {
    match meta::read_screen(paths) {
        Ok(Some(screen)) if stdout_is_terminal() => render(&screen)?,
        // A session from before this file existed, or one whose worker was
        // killed outright. The exit is still worth reporting.
        Ok(_) => {}
        Err(error) => eprintln!("latch: cannot read the final screen of {id}: {error}"),
    }
    show(&ConnectionState::Ended {
        session: id.to_string(),
        ended: describe_exit(exit),
    });
    Ok(AttachOutcome::AlreadyExited { code: exit.code })
}

fn describe_exit(exit: &meta::ExitRecord) -> String {
    match (&exit.signal, exit.code) {
        (Some(signal), _) => format!("killed by {signal}"),
        (None, Some(0)) => "exited cleanly".to_owned(),
        (None, Some(code)) => format!("exited with status {code}"),
        (None, None) => "ended".to_owned(),
    }
}

fn message_text(message: &ControlMessage) -> String {
    match message {
        ControlMessage::Error { code, message } => format!("{code}: {message}"),
        other => other.discriminator().to_owned(),
    }
}

fn run_interactive(
    mut stream: FrameStream,
    id: &SessionId,
    watch: bool,
) -> Result<AttachOutcome, Interruption> {
    use std::io::Read;
    use std::os::fd::AsRawFd;

    let stdin = std::io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let _raw_mode = RawModeGuard::enter(stdin_fd)
        .context("cannot put the terminal into raw mode")
        .map_err(Interruption::Fatal)?;
    stream
        .set_nonblocking(true)
        .map_err(|error| Interruption::transport(id, error))?;

    let mut previous_size = term::terminal_size(libc::STDOUT_FILENO);
    loop {
        // Anything the last read already decoded is dealt with before waiting
        // for the socket to become readable again. The snapshot in particular
        // arrives in the same read as the `attached` frame that preceded it, so
        // a loop that polled first would show nothing until the session next
        // produced output — indefinitely, if the agent is thinking.
        loop {
            match stream.next_buffered() {
                Ok(Some(frame)) => {
                    if let Some(outcome) = deliver(&frame)? {
                        return Ok(outcome);
                    }
                }
                Ok(None) => break,
                Err(error) => return Err(Interruption::transport(id, error)),
            }
        }

        let mut fds = [
            libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is valid writable storage and the descriptors are held
        // open for this iteration.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(Interruption::Fatal(error.into()));
            }
        }
        if let Some(signal) = term::take_received_signal() {
            return Err(Interruption::Fatal(anyhow::anyhow!(
                "attachment interrupted by signal {signal}"
            )));
        }

        let size = term::terminal_size(libc::STDOUT_FILENO);
        if size != previous_size {
            // Watchers never resize the session (D4). The worker enforces this
            // too, but a client that sends a resize it knows will be ignored is
            // a client whose intent has to be re-derived from the other end.
            if !watch {
                send(
                    &mut stream,
                    id,
                    FrameType::Control,
                    &control::encode(&ControlMessage::Resize {
                        cols: size.cols,
                        rows: size.rows,
                        pin: None,
                    }),
                )?;
            }
            previous_size = size;
        }

        if fds[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 8192];
            let read = stdin
                .lock()
                .read(&mut bytes)
                .map_err(|error| Interruption::Fatal(error.into()))?;
            if read == 0 {
                return Ok(AttachOutcome::Detached);
            }
            send(&mut stream, id, FrameType::TerminalInput, &bytes[..read])?;
        }

        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            match stream.recv() {
                Ok(Some(frame)) => {
                    if let Some(outcome) = deliver(&frame)? {
                        return Ok(outcome);
                    }
                }
                // The worker never closes an attachment it is still serving, so
                // a clean end-of-stream here is the transport going away rather
                // than a goodbye.
                Ok(None) => {
                    return Err(Interruption::transport(id, "the connection closed"));
                }
                Err(SocketError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(Interruption::transport(id, error)),
            }
        }
    }
}

/// Puts one frame on the screen, or reports that the attachment is over.
fn deliver(frame: &OwnedFrame) -> Result<Option<AttachOutcome>, Interruption> {
    match frame.frame_type {
        FrameType::TerminalOutput => {
            render(&frame.payload).map_err(|error| Interruption::Fatal(error.into()))?;
            Ok(None)
        }
        FrameType::Control => {
            let message = control::decode(&frame.payload)
                .map_err(|error| Interruption::Fatal(error.into()))?;
            match message {
                ControlMessage::SessionExited { code, .. } => {
                    Ok(Some(AttachOutcome::SessionExited { code }))
                }
                _ => Ok(None),
            }
        }
        FrameType::TerminalInput => Ok(None),
    }
}

/// Writes session output to the screen.
fn render(bytes: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

/// Sends one frame, treating a write failure as the link having died.
fn send(
    stream: &mut FrameStream,
    id: &SessionId,
    frame_type: FrameType,
    payload: &[u8],
) -> Result<(), Interruption> {
    stream
        .send(frame_type, payload)
        .and_then(|()| stream.flush())
        .map_err(|error| Interruption::transport(id, error))
}

/// Reads the next frame, or reports the link dead once `deadline` passes.
///
/// The handshake is the one place a blocking read would be indistinguishable
/// from a hang: a transport that accepted the connection and then stopped
/// carrying bytes leaves this client waiting on a worker that already answered.
fn recv_before(
    stream: &mut FrameStream,
    id: &SessionId,
    deadline: Instant,
) -> Result<Option<OwnedFrame>, Interruption> {
    stream
        .set_read_timeout(Some(POLL_INTERVAL))
        .map_err(|error| Interruption::transport(id, error))?;
    let outcome = loop {
        match stream.recv() {
            Ok(frame) => break Ok(frame),
            Err(SocketError::Io { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    break Err(Interruption::transport(
                        id,
                        "the session accepted the connection but sent nothing",
                    ));
                }
            }
            Err(error) => break Err(Interruption::transport(id, error)),
        }
    };
    // Restored so the interactive loop's own polling governs from here.
    stream
        .set_read_timeout(None)
        .map_err(|error| Interruption::transport(id, error))?;
    outcome
}

/// Prints one line of connection state to the screen the person is watching.
///
/// Safe to write over the session's screen: every snapshot starts with a hard
/// reset, so a successful attach repaints and erases this. When there is no
/// successful attach, the line is the last thing on screen — which is exactly
/// when it is worth having.
fn show(state: &ConnectionState) {
    if !stdout_is_terminal() {
        return;
    }
    let mut out = std::io::stdout().lock();
    // `\r\n` rather than `\n`: raw mode is in effect for most of these, and a
    // status line that starts halfway across the screen reads as corruption.
    let _ = write!(out, "\r\n\x1b[2m[latch] {}\x1b[0m\r\n", state.line());
    let _ = out.flush();
}

fn human(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

fn stdin_is_terminal() -> bool {
    // SAFETY: `isatty` reads descriptor metadata only.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn stdout_is_terminal() -> bool {
    // SAFETY: `isatty` reads descriptor metadata only.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Resolves a session id or name against the Latch home.
pub fn resolve_session(home: &LatchHome, session: &str) -> anyhow::Result<SessionId> {
    if let Ok(id) = SessionId::parse(session) {
        return Ok(id);
    }
    let matches = home
        .session_ids()?
        .into_iter()
        .filter_map(|id| {
            let meta = meta::read(&home.session(&id)).ok()?;
            (meta.name == session).then_some(id)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => bail!("no session named `{session}`"),
        _ => bail!("session name `{session}` is ambiguous; use its session id"),
    }
}

fn most_recent_session(home: &LatchHome) -> anyhow::Result<SessionId> {
    home.session_ids()?
        .into_iter()
        .last()
        .context("no sessions are available to attach")
}
