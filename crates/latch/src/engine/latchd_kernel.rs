//! The `latchd` kernel behind the engine seam.
//!
//! Every public verb in `engine.rs` dispatches here when `LATCH_KERNEL=latchd`
//! is selected. The shapes returned are the tmux-era ones (`SessionInfo`,
//! `SurfaceRelease`) so nothing above the seam changes; the mechanism is a
//! per-session daemon spoken to over its socket instead of tmux subprocesses.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use latchd::client::{self, ClientError};
use latchd::paths::KernelRecord;
use latchd::protocol::{ReleaseReason, Request, SnapshotFormat, Stat, State};

use super::{
    CapturePaneOptions, CreateRequest, CreateResult, PaneMetrics, PasteMessageRequest,
    ResizeRequest, SendKeysRequest, SessionInfo, SessionState, SurfaceRelease, FIRST_VIEWER_GRACE,
    FIRST_VIEWER_MAX_WAIT,
};
use crate::session::manifest::{self, TerminalSize};
use crate::session::meta::{self, MetaRequest};
use crate::session::paths::{LatchHome, SessionId, SessionPaths, SESSION_ID_ENV};
use crate::session::{timing, viewer};

/// Bundled daemon name next to `latch`.
pub const BUNDLED_LATCHD_NAME: &str = latchd::BINARY_NAME;
const LATCHD_OVERRIDE_ENV: &str = "LATCH_LATCHD_BIN";
/// How long `create` waits for the daemon to report readiness.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Control-call deadline for verbs the CLI runs without one of its own.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// Absolute path to the bundled daemon.
pub fn latchd_binary() -> Result<PathBuf> {
    if let Some(path) = cfg!(debug_assertions)
        .then(|| std::env::var_os(LATCHD_OVERRIDE_ENV))
        .flatten()
    {
        return fs::canonicalize(path).context("cannot resolve LATCH_LATCHD_BIN");
    }
    let executable =
        fs::canonicalize(std::env::current_exe().context("cannot locate the latch executable")?)?;
    let candidate = executable
        .parent()
        .expect("executable has a parent")
        .join(BUNDLED_LATCHD_NAME);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        bail!(
            "bundled kernel {} is missing; run `latch update` to repair the complete payload",
            candidate.display()
        )
    }
}

/// Reports the bundled daemon version.
pub fn latchd_version() -> Result<String> {
    let output = Command::new(latchd_binary()?)
        .arg("version")
        .output()
        .context("cannot run the bundled kernel")?;
    if !output.status.success() {
        bail!("bundled kernel did not report a version");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The socket a session's daemon listens on, from its kernel record.
fn socket(home: &LatchHome, id: &SessionId) -> Result<Option<PathBuf>> {
    let paths = home.session(id);
    match KernelRecord::read(paths.dir()) {
        Ok(Some(record)) => Ok(Some(record.socket)),
        Ok(None) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read the kernel record for {id}")),
    }
}

fn socket_or_gone(home: &LatchHome, id: &SessionId) -> Result<PathBuf> {
    socket(home, id)?.ok_or_else(|| anyhow!("session {id} is not available in the Latch kernel"))
}

fn is_gone(error: &ClientError) -> bool {
    matches!(error, ClientError::Connect(_))
}

pub(super) fn create(request: CreateRequest) -> Result<CreateResult> {
    let mut watch = timing::Stopwatch::start();
    request.home.ensure()?;
    let daemon = latchd_binary()?;
    let mut manifest = request.manifest;
    super::materialize_environment(&mut manifest);
    crate::observer::prepare_claude_launch(&request.home, &mut manifest)?;
    let prepare = watch.lap();

    let (id, paths) = loop {
        let id = SessionId::generate();
        let paths = request.home.session(&id);
        if !paths.dir().exists() {
            break (id, paths);
        }
    };
    paths.ensure()?;
    crate::observer::record_launch_source_binding(&paths, &manifest)?;
    let created_at = super::now_rfc3339();
    let metadata = meta::derive(MetaRequest {
        id: id.as_str(),
        launch: &manifest.launch,
        display: &manifest.display,
        created_at: &created_at,
    });
    meta::write_once(&paths, &metadata)?;
    timing::record(&paths, "create.prepare", prepare, None);

    let cleanup = |fifo: Option<&PathBuf>| {
        if let Some(fifo) = fifo {
            let _ = fs::remove_file(fifo);
        }
        let _ = fs::remove_dir_all(paths.dir());
    };

    let fifo = paths.launch_fifo();
    if let Err(error) = super::make_fifo(&fifo) {
        cleanup(None);
        return Err(error);
    }
    let socket_path = match latchd::paths::socket_path(request.home.root(), id.as_str()) {
        Ok(path) => path,
        Err(error) => {
            cleanup(Some(&fifo));
            return Err(error).context("cannot place the session socket");
        }
    };
    let executable = std::env::current_exe().context("cannot locate the latch executable")?;
    let size = manifest.launch.size;
    let mut command = Command::new(&daemon);
    command
        .arg("run")
        .args(["--id", id.as_str()])
        .arg("--socket")
        .arg(&socket_path)
        .arg("--session-dir")
        .arg(paths.dir())
        .arg("--cwd")
        .arg(&manifest.launch.cwd)
        .args([
            "--cols",
            &size.cols.to_string(),
            "--rows",
            &size.rows.to_string(),
        ])
        .arg("--env")
        .arg(format!("{SESSION_ID_ENV}={id}"))
        .arg("--")
        .arg(&executable)
        .arg("__launch")
        .arg("--manifest-fifo")
        .arg(&fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cleanup(Some(&fifo));
            return Err(error).context("cannot start the bundled kernel");
        }
    };
    let ready = wait_ready(&mut child);
    if let Err(error) = ready {
        let _ = child.kill();
        let _ = child.wait();
        cleanup(Some(&fifo));
        return Err(error);
    }
    timing::record(&paths, "create.kernel_start", watch.lap(), None);

    let launch_result = (|| -> Result<()> {
        let mut writer = super::open_fifo_writer(&fifo)?;
        manifest::write(&mut writer, &manifest)?;
        writer.flush()?;
        Ok(())
    })();
    let _ = fs::remove_file(&fifo);
    if let Err(error) = launch_result {
        let _ = kill_session(&request.home, &id);
        cleanup(None);
        return Err(error);
    }
    timing::record(&paths, "create.launch_handoff", watch.lap(), None);
    timing::record(&paths, "create.total", watch.total(), None);

    Ok(CreateResult {
        id,
        paths,
        meta: metadata,
    })
}

/// Waits for the daemon's `ready` line. The daemon closes its stdout right
/// after, so the parent never holds a pipe to a long-lived process.
fn wait_ready(child: &mut std::process::Child) -> Result<()> {
    let stdout = child
        .stdout
        .take()
        .context("kernel stdout is not captured")?;
    let mut stderr = child
        .stderr
        .take()
        .context("kernel stderr is not captured")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    let line = receiver.recv_timeout(READY_TIMEOUT).unwrap_or_default();
    if line.trim() == "ready" {
        // Stop holding the daemon's stderr so its later writes cannot block.
        drop(stderr);
        return Ok(());
    }
    let mut diagnostic = String::new();
    let _ = stderr.read_to_string(&mut diagnostic);
    let _ = child.wait();
    let diagnostic = diagnostic.trim();
    if diagnostic.is_empty() {
        bail!("the session kernel did not start")
    }
    bail!("the session kernel did not start: {diagnostic}")
}

pub(super) fn await_first_viewer(
    home: &LatchHome,
    id: &SessionId,
    paths: &SessionPaths,
) -> &'static str {
    let Ok(Some(socket)) = socket(home, id) else {
        return "unavailable";
    };
    let started = Instant::now();
    let wait = |timeout: Duration| -> Option<bool> {
        client::call(
            &socket,
            &Request::AwaitSurface {
                timeout_ms: timeout.as_millis() as u64,
            },
        )
        .ok()?
        .attached
    };
    match wait(FIRST_VIEWER_GRACE) {
        Some(true) => return "attached",
        Some(false) => {}
        None => return "unavailable",
    }
    if !viewer::is_pending(paths) {
        return "headless";
    }
    let remaining = FIRST_VIEWER_MAX_WAIT.saturating_sub(started.elapsed());
    match wait(remaining) {
        Some(true) => "attached",
        Some(false) => "viewer_timeout",
        None => "unavailable",
    }
}

pub(super) fn attach_exclusive(home: &LatchHome, id: &SessionId) -> Result<SurfaceRelease> {
    let socket = socket_or_gone(home, id)?;
    match client::attach_tty(&socket) {
        Ok(reason) => Ok(match reason {
            ReleaseReason::Normal => SurfaceRelease::Normal,
            ReleaseReason::Stolen => SurfaceRelease::Stolen,
            ReleaseReason::SlowClient => SurfaceRelease::SlowClient,
            ReleaseReason::SessionExited => SurfaceRelease::SessionExited,
        }),
        Err(error) if is_gone(&error) => {
            bail!("session {id} is not available in the Latch kernel")
        }
        Err(error) => Err(error).with_context(|| format!("cannot attach to session {id}")),
    }
}

fn stat(home: &LatchHome, id: &SessionId) -> Result<Option<Stat>> {
    let Some(socket) = socket(home, id)? else {
        return Ok(None);
    };
    match client::stat(&socket) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if is_gone(&error) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot inspect session {id}")),
    }
}

fn info(stat: Stat) -> SessionInfo {
    let exit = stat.exit;
    SessionInfo {
        id: stat.id,
        state: match stat.state {
            State::Running => SessionState::Running,
            State::Exited => SessionState::Exited,
        },
        size: TerminalSize::new(stat.cols, stat.rows),
        activity: UNIX_EPOCH + Duration::from_secs(stat.activity),
        attached: usize::from(stat.attached),
        pane_pid: stat.child_pid,
        exit_status: exit.as_ref().and_then(|exit| exit.status),
        exited_at: exit
            .as_ref()
            .map(|exit| UNIX_EPOCH + Duration::from_secs(exit.exited_at)),
        signal: exit.as_ref().and_then(|exit| exit.signal),
    }
}

pub(super) fn surface_attached(home: &LatchHome, id: &SessionId) -> bool {
    stat(home, id)
        .ok()
        .flatten()
        .is_some_and(|stat| stat.attached)
}

pub(super) fn has_session(home: &LatchHome, id: &SessionId) -> bool {
    stat(home, id).ok().flatten().is_some()
}

pub(super) fn list(home: &LatchHome) -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();
    for id in home.session_ids()? {
        if let Some(stat) = stat(home, &id)? {
            sessions.push(info(stat));
        }
    }
    Ok(sessions)
}

pub(super) fn inspect(home: &LatchHome, id: &SessionId) -> Result<Option<SessionInfo>> {
    Ok(stat(home, id)?.map(info))
}

pub(super) fn pane_metrics(home: &LatchHome, id: &SessionId) -> Result<PaneMetrics> {
    let stat = stat(home, id)?.ok_or_else(|| anyhow!("session {id} is not available"))?;
    Ok(PaneMetrics {
        cols: stat.cols,
        rows: stat.rows,
        alternate_screen: stat.alternate_screen,
    })
}

pub(super) fn capture_pane(
    home: &LatchHome,
    id: &SessionId,
    timeout: Duration,
    options: CapturePaneOptions,
) -> Result<String> {
    let socket = socket_or_gone(home, id)?;
    let reply = client::call_with_timeout(
        &socket,
        &Request::Snapshot {
            format: if options.styled {
                SnapshotFormat::Styled
            } else {
                SnapshotFormat::Text
            },
            scrollback_lines: options.scrollback_lines,
        },
        timeout,
    )
    .with_context(|| format!("cannot capture session {id}"))?;
    reply
        .text
        .ok_or_else(|| anyhow!("kernel returned no screen for session {id}"))
}

pub(super) fn paste_message(request: PasteMessageRequest<'_>, timeout: Duration) -> Result<()> {
    let socket = socket_or_gone(request.home, request.id)?;
    client::call_with_timeout(
        &socket,
        &Request::Submit {
            text: String::from_utf8_lossy(request.message).into_owned(),
        },
        timeout,
    )
    .with_context(|| format!("cannot submit a message to session {}", request.id))?;
    Ok(())
}

pub(super) fn send_keys(request: SendKeysRequest<'_>, timeout: Duration) -> Result<()> {
    if request.keys.is_empty() {
        bail!("at least one key is required");
    }
    let socket = socket_or_gone(request.home, request.id)?;
    client::call_with_timeout(
        &socket,
        &Request::Key {
            keys: request.keys.to_vec(),
        },
        timeout,
    )
    .with_context(|| format!("cannot send keys to session {}", request.id))?;
    Ok(())
}

pub(super) fn resize(request: ResizeRequest<'_>) -> Result<()> {
    let socket = socket_or_gone(request.home, request.id)?;
    client::call_with_timeout(
        &socket,
        &Request::Resize {
            cols: request.size.cols,
            rows: request.size.rows,
            pin: request.pin,
        },
        CONTROL_TIMEOUT,
    )
    .with_context(|| format!("cannot resize session {}", request.id))?;
    Ok(())
}

pub(super) fn kill_session(home: &LatchHome, id: &SessionId) -> Result<()> {
    let Some(socket) = socket(home, id)? else {
        return Ok(());
    };
    match client::call_with_timeout(&socket, &Request::Kill, CONTROL_TIMEOUT) {
        Ok(_) => {}
        // Already gone is the outcome we wanted.
        Err(error) if is_gone(&error) => {}
        Err(error) => return Err(error).with_context(|| format!("cannot remove session {id}")),
    }
    // The daemon exits right after answering; give the socket a moment to
    // disappear so a following `list` does not see a half-dead session.
    let deadline = Instant::now() + Duration::from_secs(2);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(&socket);
    Ok(())
}
