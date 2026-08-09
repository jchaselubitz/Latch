//! Starting a worker that outlives whoever started it.
//!
//! The worker detaches *itself*. [`ensure_detached`] runs at the top of worker
//! mode, before the session directory is touched, and it is idempotent: a worker
//! that is already a session leader does nothing.
//!
//! Putting detachment in the worker rather than in the spawning CLI is
//! deliberate. If the spawner were responsible, then a worker started any other
//! way — by a test, by a future integration, by hand while debugging — would be
//! subtly less durable than one started by the CLI, and the difference would only
//! show up when somebody's terminal closed. Detaching in one place makes the
//! guarantee a property of the worker rather than of its caller.
//!
//! The consequence the tests pin: killing, or hanging, the process that started a
//! session does not signal the child. Those are two separate facts. A killed
//! launcher tests process-group membership; a stopped launcher tests that nothing
//! in the worker's loop is waiting on it.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::worker::manifest::LaunchManifest;
use crate::worker::paths::{SessionId, SessionPaths};

/// How long [`spawn_detached`] waits for the new worker's socket by default.
///
/// Startup is a PTY allocation and a bind. A second is generous; the value
/// exists so a machine under heavy load reports a timeout rather than a
/// mysterious "session not found".
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// What this process's detachment achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detachment {
    /// The session id this process now leads.
    pub session_id: i32,
    /// The process group this process now leads.
    pub group_id: i32,
    /// Whether the process was already detached and nothing was done.
    pub already_detached: bool,
}

/// Anything that can go wrong starting a worker.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The worker binary could not be located or executed.
    #[error("cannot run the worker binary {path}: {source}")]
    Exec {
        /// The binary that could not be run.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The manifest could not be handed to the new worker.
    #[error("cannot send the launch manifest to the worker: {0}")]
    Manifest(#[from] crate::worker::manifest::ManifestError),
    /// Detaching from the launching process's session failed.
    #[error("cannot detach the worker process: {0}")]
    Detach(#[source] std::io::Error),
    /// The worker started but its socket never answered.
    #[error("worker for {id} did not become ready within {timeout:?}")]
    NotReady {
        /// The session that never came up.
        id: String,
        /// How long it was given.
        timeout: Duration,
    },
    /// The worker exited before becoming ready.
    ///
    /// Separate from a timeout because the recovery is different: there is an
    /// exit status to report, and waiting longer would not have helped.
    #[error("worker for {id} exited during startup with status {status}")]
    DiedDuringStartup {
        /// The session that never came up.
        id: String,
        /// The worker's exit status.
        status: i32,
    },
    /// Preparing the session directory failed.
    #[error(transparent)]
    Paths(#[from] crate::worker::paths::PathsError),
}

/// A request to create a session.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Where the session directory goes.
    pub paths: SessionPaths,
    /// The session's identifier.
    pub id: SessionId,
    /// The `latch` binary to run in worker mode.
    ///
    /// Explicit rather than always `current_exe` so a test runs the binary it
    /// built, and so a future updater can hand off to a new build without
    /// re-execing itself.
    pub worker_binary: PathBuf,
    /// Everything the worker is told at startup, delivered over its stdin.
    pub manifest: LaunchManifest,
    /// How long to wait for the socket before giving up.
    pub ready_timeout: Duration,
}

impl SpawnRequest {
    /// A request with the default readiness timeout.
    pub fn new(
        paths: SessionPaths,
        id: SessionId,
        worker_binary: impl Into<PathBuf>,
        manifest: LaunchManifest,
    ) -> Self {
        Self {
            paths,
            id,
            worker_binary: worker_binary.into(),
            manifest,
            ready_timeout: DEFAULT_READY_TIMEOUT,
        }
    }
}

/// A worker that has started and whose socket answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedWorker {
    /// The session's identifier.
    pub id: SessionId,
    /// Where its files are.
    pub paths: SessionPaths,
}

/// Starts a worker for `request` and returns once its socket answers.
///
/// Deliberately returns no PID. The caller has no use for one: it talks to the
/// worker over the socket, and it stops the session by asking the worker rather
/// than by signalling it. A returned PID would be a stored PID waiting to happen.
///
/// The manifest is written to the new process's stdin and the pipe is closed
/// before waiting, so a worker blocked reading its manifest cannot deadlock
/// against a caller blocked waiting for its socket.
pub fn spawn_detached(request: SpawnRequest) -> Result<SpawnedWorker, SpawnError> {
    request.paths.ensure()?;
    let mut command = Command::new(&request.worker_binary);
    command
        .arg("worker")
        .arg("--session-dir")
        .arg(request.paths.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|source| SpawnError::Exec {
        path: request.worker_binary.clone(),
        source,
    })?;
    let stdin = child.stdin.take().expect("piped stdin is present");
    crate::worker::manifest::write(stdin, &request.manifest)?;
    let deadline = std::time::Instant::now() + request.ready_timeout;
    loop {
        if started(&request.paths) {
            return Ok(SpawnedWorker {
                id: request.id,
                paths: request.paths,
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err(SpawnError::NotReady {
                id: request.id.to_string(),
                timeout: request.ready_timeout,
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether a worker has got far enough that waiting longer cannot help.
///
/// Either its socket answers, or it has already recorded an exit. The second
/// case is not a failure and must not be waited out: `latch run -- true` starts
/// a session that finishes in microseconds, and a readiness check that only ever
/// looks for a live socket can poll straight past its whole lifetime and then
/// report that it never started. It did start. It is over.
fn started(paths: &SessionPaths) -> bool {
    crate::worker::socket::is_live(&paths.socket()) || paths.exit().exists()
}

/// Waits for a session's socket to accept a connection.
///
/// Readiness is the socket answering, not the file existing: a socket file with
/// nobody listening is exactly the stale case this must not be fooled by. A
/// session that has already exited counts as started — see [`started`].
pub fn wait_until_ready(paths: &SessionPaths, timeout: Duration) -> Result<(), SpawnError> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if started(paths) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(SpawnError::NotReady {
        id: paths
            .dir()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_owned(),
        timeout,
    })
}

/// Detaches the current process from its launcher's session and process group.
///
/// `setsid` after a fork, because a process that already leads its group cannot
/// call it. Idempotent: a process that is already a session leader reports
/// [`Detachment::already_detached`] and does nothing.
///
/// This function can therefore return in a *different* process than the one that
/// called it. That is the point of it, and it is why it runs before anything with
/// a lifetime — no file is open, no socket is bound, and nothing has been written
/// to the session directory when it runs.
pub fn ensure_detached() -> Result<Detachment, SpawnError> {
    // SAFETY: these calls only inspect process identity.
    let pid = unsafe { libc::getpid() };
    let group = unsafe { libc::getpgrp() };
    if pid == group {
        // SAFETY: getsid only reads kernel process state.
        return Ok(Detachment {
            session_id: unsafe { libc::getsid(0) },
            group_id: group,
            already_detached: true,
        });
    }
    // SAFETY: fork is followed by only async-signal-safe calls in the child.
    let forked = unsafe { libc::fork() };
    if forked == -1 {
        return Err(SpawnError::Detach(io::Error::last_os_error()));
    }
    if forked > 0 {
        // SAFETY: the parent worker stub must not retain the launcher's lifetime.
        unsafe { libc::_exit(0) };
    }
    // SAFETY: child is not a group leader because the pre-fork child inherited
    // the parent's group and had a distinct pid.
    let session = unsafe { libc::setsid() };
    if session == -1 {
        return Err(SpawnError::Detach(io::Error::last_os_error()));
    }
    Ok(Detachment {
        session_id: session,
        group_id: unsafe { libc::getpgrp() },
        already_detached: false,
    })
}
