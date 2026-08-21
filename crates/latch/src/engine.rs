//! Private tmux session kernel.
//!
//! Latch invokes one pinned, bundled tmux binary by absolute path and always
//! addresses a private socket with a generated configuration.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::session::manifest::{self, LaunchManifest, TerminalSize};
use crate::session::meta::{self, ExitRecord, MetaRequest, SessionMeta};
use crate::session::paths::{LatchHome, SessionId, SessionPaths, FILE_MODE, SESSION_ID_ENV};
use crate::session::{timing, viewer};

/// Public protocol contract version retained across the kernel swap.
pub const PROTOCOL_VERSION: u32 = 2;
/// Pinned tmux release bundled by the distribution.
pub const TMUX_VERSION: &str = "3.7b";
/// Deliberate child terminal type.
pub const DEFAULT_TERMINAL: &str = "xterm-256color";
/// Bundled binary name next to `latch`.
pub const BUNDLED_TMUX_NAME: &str = "latch-tmux";
/// Bundled remote-access helper name next to `latch`.
pub const BUNDLED_REMOTE_NAME: &str = "latch-remote";

/// tmux 3.7b stderr fragments used to classify empty-server outcomes.
/// Revisit these when bumping [`TMUX_VERSION`].
const TMUX_ERR_NO_SERVER: &str = "no server running";
const TMUX_ERR_NO_SESSION: &str = "can't find session";
const TMUX_ERR_NO_SESSIONS: &str = "no sessions";

/// Attempts a session query makes before a session tmux cannot yet describe
/// is reported as gone.
const SESSION_QUERY_ATTEMPTS: usize = 3;
/// Pause between those attempts; the window a session spends half-created or
/// half-destroyed is far shorter than this.
const SESSION_QUERY_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Stop polls tmux rather than sleeping on a single long timeout, so this
/// interval is the process-churn / wait-time tradeoff.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Graceful SIGTERM window (~5 s).
const STOP_GRACE_POLLS: usize = 100;
/// SIGKILL window (~2 s), used for `--force` and the post-TERM escalation.
const STOP_FORCE_POLLS: usize = 40;
/// How long an integration-created session waits for a viewer that has not yet
/// announced itself.
///
/// Overlord creates the durable session first and opens the viewer second. A
/// short gate makes that normal path match a direct terminal launch: shell
/// startup and agent trust/permission UI begin only once there is a terminal
/// capable of showing them. This bound only has to cover the gap between
/// `create` returning and `open` stamping its marker — one process spawn —
/// because an announced open extends the wait to [`FIRST_VIEWER_MAX_WAIT`].
/// Nothing waits longer than this when no viewer is coming at all, which is
/// what `create` without `open` (a background launch) looks like.
const FIRST_VIEWER_GRACE: Duration = Duration::from_secs(3);
/// Ceiling on the wait once `latch open` has announced an in-flight viewer.
///
/// A cold terminal application can take far longer to present a window than
/// the unannounced grace allows, and starting the agent into a pane nobody can
/// see yet is exactly the failure this gate exists to prevent. The ceiling
/// keeps a viewer that never arrives from stranding the session.
const FIRST_VIEWER_MAX_WAIT: Duration = Duration::from_secs(30);
const FIRST_VIEWER_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const ATTACH_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(1);
/// Gap between attachment checks while the tmux client starts. Each check is a
/// tmux query process, so it is spaced far wider than the launcher's `try_wait`
/// poll: a viewer that is opening does not need to be noticed within 20 ms, and
/// spawning fifty processes to find that out competes with the client itself.
const ATTACH_OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Why a tmux query produced no session rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TmuxMiss {
    /// No private server process.
    Server,
    /// Targeted session name is absent.
    Session,
    /// Server is up but has no sessions.
    Empty,
}

/// Classifies pinned-tmux stderr that means "nothing to query".
pub(crate) fn classify_tmux_stderr(diagnostic: &str) -> Option<TmuxMiss> {
    if diagnostic.contains(TMUX_ERR_NO_SERVER) {
        Some(TmuxMiss::Server)
    } else if diagnostic.contains(TMUX_ERR_NO_SESSION) {
        Some(TmuxMiss::Session)
    } else if diagnostic.contains(TMUX_ERR_NO_SESSIONS) {
        Some(TmuxMiss::Empty)
    } else {
        None
    }
}

const TMUX_OVERRIDE_ENV: &str = "LATCH_TMUX_BIN";
/// UTF-8 `LC_CTYPE` injected into pinned-tmux children when the caller's
/// effective locale is not UTF-8 (Finder/launchd/cron parents). Without it
/// tmux sanitizes the U+001F field separator to `_` in command output and
/// session rows stop parsing. Session commands keep the caller's original
/// locale: pane environments come from the launch manifest, not from this
/// override.
#[cfg(target_os = "macos")]
const UTF8_CTYPE: &str = "UTF-8";
#[cfg(not(target_os = "macos"))]
const UTF8_CTYPE: &str = "C.UTF-8";
/// Unit separator between tmux format fields. Tabs are invisible in errors and
/// get collapsed by some callers; this character cannot appear in Latch ids.
const SESSION_ROW_SEPARATOR: char = '\u{1f}';
const SESSION_INFO_FORMAT: &str =
    "#{session_name}\u{1f}#{pane_dead}\u{1f}#{pane_dead_status}\u{1f}#{window_width}\u{1f}#{window_height}\u{1f}#{session_activity}\u{1f}#{session_attached}\u{1f}#{pane_pid}\u{1f}#{pane_dead_time}\u{1f}#{pane_dead_signal}";
const CONFIG_BODY: &str = "\
set -g status off
set -g prefix None
set -g prefix2 None
unbind-key -a
unbind-key -a -T copy-mode
unbind-key -a -T copy-mode-vi
set -g remain-on-exit on
set -g window-size latest
set -g default-terminal \"xterm-256color\"
set -as terminal-features \",xterm-256color:RGB\"
set -g mouse off
set -g allow-rename off
set -g set-titles off
set -g history-limit 50000
";

/// Session lifecycle derived directly from tmux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Pane is live.
    Running,
    /// Pane exited and remains available for inspection.
    Exited,
    /// Metadata exists but tmux no longer has the session.
    Lost,
}

impl SessionState {
    /// Stable JSON spelling.
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Lost => "lost",
        }
    }

    /// Parses a stable JSON spelling.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }
}

/// Live facts queried from tmux.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Derived lifecycle state.
    pub state: SessionState,
    /// Current geometry.
    pub size: TerminalSize,
    /// Last tmux activity time.
    pub activity: SystemTime,
    /// Number of attached tmux clients.
    pub attached: usize,
    /// Pane process id, queried live and never persisted.
    pub pane_pid: i32,
    /// Exit status for a dead pane.
    pub exit_status: Option<i32>,
    /// Time the pane became dead.
    pub exited_at: Option<SystemTime>,
    /// Signal number for a signalled pane.
    pub signal: Option<i32>,
}

/// Named inputs for creating a session.
pub struct CreateRequest {
    /// State root.
    pub home: LatchHome,
    /// Validated launch material.
    pub manifest: LaunchManifest,
}

/// Result of session creation.
pub struct CreateResult {
    /// Generated id.
    pub id: SessionId,
    /// Session paths.
    pub paths: SessionPaths,
    /// Persisted display metadata.
    pub meta: SessionMeta,
}

/// Named inputs for changing session geometry.
pub struct ResizeRequest<'a> {
    /// State root.
    pub home: &'a LatchHome,
    /// Session id.
    pub id: &'a SessionId,
    /// Requested geometry.
    pub size: TerminalSize,
    /// Whether to switch the policy to manual.
    pub pin: bool,
}

/// Named inputs for stopping a session.
pub struct StopRequest<'a> {
    /// State root.
    pub home: &'a LatchHome,
    /// Session id.
    pub id: &'a SessionId,
    /// Whether to skip graceful termination.
    pub force: bool,
}

/// Named inputs for pasting and submitting one message.
pub struct PasteMessageRequest<'a> {
    /// State root.
    pub home: &'a LatchHome,
    /// Session id.
    pub id: &'a SessionId,
    /// Message bytes read from stdin by the public CLI.
    pub message: &'a [u8],
}

/// Named inputs for direct keypress injection.
pub struct SendKeysRequest<'a> {
    /// State root.
    pub home: &'a LatchHome,
    /// Session id.
    pub id: &'a SessionId,
    /// Validated tmux key names.
    pub keys: &'a [String],
}

/// Creates a detached tmux session and transfers launch material over a FIFO.
pub fn create(request: CreateRequest) -> Result<CreateResult> {
    // Creation is one of three processes that decide how long a launch takes,
    // so each of its phases is timed into the session's sidecar. The phases are
    // recorded once the session directory exists — the work before that is
    // covered by `create.total`.
    let mut watch = timing::Stopwatch::start();
    request.home.ensure()?;
    ensure_config(&request.home)?;
    ensure_tmux_available()?;
    let mut manifest = request.manifest;
    materialize_environment(&mut manifest);
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
    let created_at = now_rfc3339();
    let metadata = meta::derive(MetaRequest {
        id: id.as_str(),
        launch: &manifest.launch,
        display: &manifest.display,
        created_at: &created_at,
    });
    meta::write_once(&paths, &metadata)?;
    timing::record(&paths, "create.prepare", prepare, None);

    let fifo = paths.launch_fifo();
    if let Err(error) = make_fifo(&fifo) {
        let _ = fs::remove_dir_all(paths.dir());
        return Err(error);
    }
    let executable = std::env::current_exe().context("cannot locate the latch executable")?;
    let size = manifest.launch.size;
    let cols = size.cols.to_string();
    let rows = size.rows.to_string();
    let session_environment = format!("{SESSION_ID_ENV}={id}");
    let mut command = match tmux(&request.home) {
        Ok(command) => command,
        Err(error) => {
            let _ = fs::remove_file(&fifo);
            let _ = fs::remove_dir_all(paths.dir());
            return Err(error);
        }
    };
    let tmux_result = command
        .args([
            OsStr::new("new-session"),
            OsStr::new("-d"),
            OsStr::new("-s"),
            OsStr::new(id.as_str()),
            OsStr::new("-x"),
            OsStr::new(&cols),
            OsStr::new("-y"),
            OsStr::new(&rows),
            OsStr::new("-c"),
            manifest.launch.cwd.as_os_str(),
            OsStr::new("-e"),
            OsStr::new(&session_environment),
            OsStr::new("--"),
            executable.as_os_str(),
            OsStr::new("__launch"),
            OsStr::new("--manifest-fifo"),
            fifo.as_os_str(),
        ])
        .output();

    match tmux_result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let _ = fs::remove_file(&fifo);
            let _ = fs::remove_dir_all(paths.dir());
            return Err(tmux_error("create session", output));
        }
        Err(error) => {
            let _ = fs::remove_file(&fifo);
            let _ = fs::remove_dir_all(paths.dir());
            return Err(error).context("cannot start the bundled tmux");
        }
    }
    timing::record(&paths, "create.tmux_new_session", watch.lap(), None);

    let launch_result = (|| -> Result<()> {
        let mut writer = open_fifo_writer(&fifo)?;
        manifest::write(&mut writer, &manifest)?;
        writer.flush()?;
        Ok(())
    })();
    let _ = fs::remove_file(&fifo);
    if let Err(error) = launch_result {
        let _ = kill_session(&request.home, &id);
        let _ = fs::remove_dir_all(paths.dir());
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

/// Internal launcher entered under tmux; reads the FIFO and replaces itself.
pub fn launch_from_fifo(path: &Path) -> Result<()> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("cannot read launch pipe {}", path.display()))?;
    let mut manifest = manifest::read(&mut file)?;
    drop(file);
    let _ = fs::remove_file(path);
    wait_for_first_viewer(&manifest);
    ensure_interactive_login_shell(&mut manifest.launch.argv);

    let mut command = Command::new(&manifest.launch.argv[0]);
    command.args(&manifest.launch.argv[1..]);
    command.current_dir(&manifest.launch.cwd);
    if !manifest.launch.inherit_env {
        command.env_clear();
    }
    command.env_remove("TMUX");
    command.env("TERM", DEFAULT_TERMINAL);
    if let Some(id) = std::env::var_os(SESSION_ID_ENV) {
        command.env(SESSION_ID_ENV, id);
    }
    for (key, value) in &manifest.launch.env {
        if key != "TMUX" && key != "TERM" {
            command.env(key, value);
        }
    }
    let error = command.exec();
    Err(error).context("cannot execute session command")
}

/// Attaches the calling terminal to a session.
pub fn attach(home: &LatchHome, id: &SessionId) -> Result<()> {
    attach_with_mode(home, id, false)
}

/// Attaches a terminal as an observer. tmux's read-only client mode protects
/// the session even if a caller later writes to its PTY.
pub fn attach_read_only(home: &LatchHome, id: &SessionId) -> Result<()> {
    attach_with_mode(home, id, true)
}

fn attach_with_mode(home: &LatchHome, id: &SessionId, read_only: bool) -> Result<()> {
    ensure_config(home)?;
    let mut command = tmux(home)?;
    command.arg("attach-session");
    if read_only {
        command.arg("-r");
    }
    let mut child = command
        .args(["-t", id.as_str()])
        .spawn()
        .with_context(|| format!("cannot attach to session {id}"))?;
    let deadline = std::time::Instant::now() + ATTACH_OBSERVATION_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("cannot wait for attachment to session {id}"))?
        {
            if status.success() {
                signal_first_viewer(home, id);
                return Ok(());
            }
            bail!("tmux could not attach to session {id}");
        }
        if attached_clients(home, id).is_some_and(|attached| attached > 0) {
            signal_first_viewer(home, id);
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(ATTACH_OBSERVATION_POLL_INTERVAL);
    }
    let status = child
        .wait()
        .with_context(|| format!("cannot wait for attachment to session {id}"))?;
    if !status.success() {
        bail!("tmux could not attach to session {id}");
    }
    Ok(())
}

/// Overlord's create-then-open launch is deliberately two operations. Hold its
/// internal launcher so the shell and agent cannot present (or block on)
/// first-run UI before the viewer exists. This is best effort: inability to
/// coordinate with tmux must never turn a durable headless launch into a
/// failure.
fn wait_for_first_viewer(manifest: &LaunchManifest) {
    if manifest.display.source.kind != "overlord" {
        return;
    }
    let Ok(home) = LatchHome::from_env() else {
        return;
    };
    let Some(id) = std::env::var(SESSION_ID_ENV)
        .ok()
        .and_then(|value| SessionId::parse(&value).ok())
    else {
        return;
    };
    let paths = home.session(&id);
    let watch = timing::Stopwatch::start();
    let outcome = await_first_viewer(&home, &id, &paths);
    timing::record(
        &paths,
        "launch.first_viewer_wait",
        watch.total(),
        Some(outcome),
    );
    viewer::clear(&paths);
}

/// Blocks until the session's first viewer attaches, or until waiting stops
/// being justified. Returns the reason the wait ended, which is recorded beside
/// its duration so a slow launch can be attributed afterwards.
fn await_first_viewer(home: &LatchHome, id: &SessionId, paths: &SessionPaths) -> &'static str {
    let Ok(mut waiter) = tmux(home).and_then(|mut command| {
        command.args(["wait-for", &first_viewer_channel(id)]);
        command.spawn().context("cannot wait for the first viewer")
    }) else {
        return "unavailable";
    };
    let started = std::time::Instant::now();
    let ceiling = started + FIRST_VIEWER_MAX_WAIT;
    let mut deadline = started + FIRST_VIEWER_GRACE;
    let mut announced = false;
    loop {
        match waiter.try_wait() {
            Ok(Some(_)) => return "attached",
            Ok(None) => {}
            Err(_) => {
                stop_waiter(&mut waiter);
                return "unavailable";
            }
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            // `latch open` stamps its marker before asking the operating system
            // for a window, so an announced open means a terminal is genuinely
            // on its way and the grace was simply shorter than this machine's
            // viewer startup.
            if !announced && now < ceiling && viewer::is_pending(paths) {
                announced = true;
                deadline = ceiling;
                continue;
            }
            stop_waiter(&mut waiter);
            return if announced {
                "viewer_timeout"
            } else {
                "headless"
            };
        }
        std::thread::sleep(FIRST_VIEWER_WAIT_POLL_INTERVAL);
    }
}

fn stop_waiter(waiter: &mut std::process::Child) {
    let _ = waiter.kill();
    let _ = waiter.wait();
}

fn signal_first_viewer(home: &LatchHome, id: &SessionId) {
    let Ok(mut command) = tmux(home) else {
        return;
    };
    command.args(["wait-for", "-S", &first_viewer_channel(id)]);
    let _ = command.status();
}

fn first_viewer_channel(id: &SessionId) -> String {
    format!("latch-first-viewer-{id}")
}

/// Number of tmux clients attached to a session, or `None` when the private
/// server cannot answer. Used to confirm that a viewer really arrived rather
/// than that a viewer command merely exited.
pub fn attached_clients(home: &LatchHome, id: &SessionId) -> Option<usize> {
    let output = tmux(home)
        .ok()?
        .args([
            "display-message",
            "-p",
            "-t",
            id.as_str(),
            "-F",
            "#{session_attached}",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok()?.trim().parse().ok())
        .flatten()
}

/// Returns whether tmux still owns a named session.
pub fn has_session(home: &LatchHome, id: &SessionId) -> bool {
    let Ok(mut command) = tmux(home) else {
        return false;
    };
    command
        .args(["has-session", "-t", id.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Lists every session from the private server in one query.
///
/// A session caught mid-creation or mid-teardown answers with a row tmux could
/// not fully expand. The listing is retried while any row looks like that, so
/// a session in motion neither fails the whole query nor disappears from it
/// for one poll; a row still half-expanded after the retries is dropped, and
/// the caller reports that session from its own metadata.
pub fn list(home: &LatchHome) -> Result<Vec<SessionInfo>> {
    let mut described = Vec::new();
    for attempt in 0..SESSION_QUERY_ATTEMPTS {
        let (sessions, partial) = list_once(home)?;
        described = sessions;
        if !partial {
            break;
        }
        if attempt + 1 < SESSION_QUERY_ATTEMPTS {
            std::thread::sleep(SESSION_QUERY_RETRY_INTERVAL);
        }
    }
    Ok(described)
}

/// Returns the sessions tmux could describe, and whether any row was still
/// half-expanded.
fn list_once(home: &LatchHome) -> Result<(Vec<SessionInfo>, bool)> {
    ensure_config(home)?;
    let output = tmux(home)?
        .args(["list-sessions", "-F", SESSION_INFO_FORMAT])
        .output()
        .context("cannot query the bundled tmux")?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        if matches!(
            classify_tmux_stderr(&diagnostic),
            Some(TmuxMiss::Server | TmuxMiss::Empty)
        ) {
            return Ok((Vec::new(), false));
        }
        return Err(tmux_error("list sessions", output));
    }
    let mut sessions = Vec::new();
    let mut partial = false;
    for line in String::from_utf8(output.stdout)
        .context("tmux returned non-UTF-8 session data")?
        .lines()
        .filter(|line| !line.is_empty())
    {
        match parse_row(line)? {
            Some(info) => sessions.push(info),
            None => partial = true,
        }
    }
    Ok((sessions, partial))
}

/// Outcome of a single session query, keeping "tmux has no such session"
/// apart from "tmux has it but could not describe it yet".
enum SessionQuery {
    /// tmux does not own the session.
    Missing,
    /// tmux owns the session but returned a half-expanded row.
    Partial,
    /// Complete live facts.
    Found(SessionInfo),
}

/// Queries one session.
///
/// A session caught mid-creation or mid-teardown answers with a row tmux could
/// not fully expand. That state lasts a moment, so the query is retried before
/// the session is reported as gone; a caller must not see a live session blink
/// out because the query landed inside that window.
pub fn inspect(home: &LatchHome, id: &SessionId) -> Result<Option<SessionInfo>> {
    for attempt in 0..SESSION_QUERY_ATTEMPTS {
        match inspect_once(home, id)? {
            SessionQuery::Found(info) => return Ok(Some(info)),
            SessionQuery::Missing => return Ok(None),
            SessionQuery::Partial => {
                if attempt + 1 < SESSION_QUERY_ATTEMPTS {
                    std::thread::sleep(SESSION_QUERY_RETRY_INTERVAL);
                }
            }
        }
    }
    Ok(None)
}

fn inspect_once(home: &LatchHome, id: &SessionId) -> Result<SessionQuery> {
    ensure_config(home)?;
    let output = tmux(home)?
        .args([
            "display-message",
            "-p",
            "-t",
            id.as_str(),
            "-F",
            SESSION_INFO_FORMAT,
        ])
        .output()
        .context("cannot inspect the private tmux session")?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        if classify_tmux_stderr(&diagnostic).is_some() {
            return Ok(SessionQuery::Missing);
        }
        return Err(tmux_error("inspect session", output));
    }
    let row = String::from_utf8(output.stdout).context("tmux returned non-UTF-8 session data")?;
    Ok(match parse_row(row.trim_end_matches(['\r', '\n']))? {
        Some(info) => SessionQuery::Found(info),
        None => SessionQuery::Partial,
    })
}

/// Captures the currently visible pane for input-safety classification.
pub fn capture_pane(home: &LatchHome, id: &SessionId) -> Result<String> {
    ensure_config(home)?;
    let output = tmux(home)?
        .args(["capture-pane", "-p", "-J", "-t", id.as_str()])
        .output()
        .context("cannot capture the private tmux pane")?;
    if !output.status.success() {
        return Err(tmux_error("capture session screen", output));
    }
    String::from_utf8(output.stdout).context("tmux returned a non-UTF-8 screen capture")
}

/// Pastes a message through a private tmux buffer and presses Enter.
///
/// The message remains on stdin and never appears in a process argument.
pub fn paste_message(request: PasteMessageRequest<'_>) -> Result<()> {
    let buffer = format!(
        "latch-message-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut command = tmux(request.home)?;
    command
        .args(["load-buffer", "-b", &buffer, "-"])
        .stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .context("cannot load a private tmux paste buffer")?;
    child
        .stdin
        .take()
        .context("tmux paste buffer stdin is unavailable")?
        .write_all(request.message)?;
    let status = child.wait().context("cannot wait for tmux paste buffer")?;
    if !status.success() {
        bail!("cannot load a private tmux paste buffer");
    }

    let mut paste = tmux(request.home)?;
    paste.args([
        "paste-buffer",
        "-p",
        "-d",
        "-b",
        &buffer,
        "-t",
        request.id.as_str(),
    ]);
    if let Err(error) = run_tmux(paste, "paste message") {
        if let Ok(mut cleanup) = tmux(request.home) {
            cleanup.args(["delete-buffer", "-b", &buffer]);
            let _ = cleanup.status();
        }
        return Err(error);
    }
    send_keys(SendKeysRequest {
        home: request.home,
        id: request.id,
        keys: &["Enter".to_owned()],
    })
    .with_context(|| {
        "message was pasted into the composer but not submitted; recover in the terminal with Ctrl-U"
    })
}

/// Sends validated key names into one pane.
pub fn send_keys(request: SendKeysRequest<'_>) -> Result<()> {
    if request.keys.is_empty() {
        bail!("at least one key is required");
    }
    let mut command = tmux(request.home)?;
    command
        .args(["send-keys", "-t", request.id.as_str(), "--"])
        .args(request.keys);
    run_tmux(command, "send keys")
}

/// Resizes a session and optionally pins manual geometry.
pub fn resize(request: ResizeRequest<'_>) -> Result<()> {
    let mut command = tmux(request.home)?;
    command.args([
        "resize-window",
        "-t",
        request.id.as_str(),
        "-x",
        &request.size.cols.to_string(),
        "-y",
        &request.size.rows.to_string(),
    ]);
    run_tmux(command, "resize session")?;
    if request.pin {
        let mut command = tmux(request.home)?;
        command.args([
            "set-option",
            "-t",
            request.id.as_str(),
            "window-size",
            "manual",
        ]);
        run_tmux(command, "pin session size")?;
    }
    Ok(())
}

/// Gracefully terminates the pane process group, preserving its dead pane.
pub fn stop(request: StopRequest<'_>) -> Result<SessionState> {
    let info = inspect(request.home, request.id)?
        .ok_or_else(|| anyhow!("session {} is not running", request.id))?;
    if info.state == SessionState::Exited {
        return Ok(SessionState::Exited);
    }
    let signal = if request.force {
        libc::SIGKILL
    } else {
        libc::SIGTERM
    };
    signal_pane(&info, signal).with_context(|| format!("cannot signal session {}", request.id))?;
    let polls = if request.force {
        STOP_FORCE_POLLS
    } else {
        STOP_GRACE_POLLS
    };
    if wait_until_exited(request.home, request.id, polls)? {
        return Ok(SessionState::Exited);
    }
    if !request.force {
        signal_pane(&info, libc::SIGKILL)
            .with_context(|| format!("cannot force session {} to stop", request.id))?;
        if wait_until_exited(request.home, request.id, STOP_FORCE_POLLS)? {
            return Ok(SessionState::Exited);
        }
    }
    Ok(SessionState::Running)
}

fn wait_until_exited(home: &LatchHome, id: &SessionId, polls: usize) -> Result<bool> {
    for _ in 0..polls {
        if inspect(home, id)?.is_some_and(|current| current.state == SessionState::Exited) {
            return Ok(true);
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    Ok(false)
}

/// True when list/inspect failures are caused by a missing private server.
///
/// Used by `latch attach --retry` so "server not up yet" is retried and
/// "session is gone" is not.
pub(crate) fn tmux_server_is_absent(home: &LatchHome) -> bool {
    let Ok(mut command) = tmux(home) else {
        return false;
    };
    let Ok(output) = command.args(["list-sessions", "-F", "#S"]).output() else {
        return false;
    };
    if output.status.success() {
        return false;
    }
    classify_tmux_stderr(&String::from_utf8_lossy(&output.stderr)) == Some(TmuxMiss::Server)
}

/// True when `--retry` should try attach again after `error`.
pub(crate) fn attach_is_retryable(home: &LatchHome, id: &SessionId) -> bool {
    match inspect(home, id) {
        Ok(Some(info)) => info.state == SessionState::Running,
        Ok(None) | Err(_) => tmux_server_is_absent(home),
    }
}

fn signal_pane(info: &SessionInfo, signal: i32) -> std::io::Result<()> {
    // SAFETY: the pid was queried from the private tmux server. A negative pid
    // targets the pane's process group without persisting process identity.
    let result = unsafe { libc::kill(-info.pane_pid, signal) };
    if result == -1 {
        let direct = unsafe { libc::kill(info.pane_pid, signal) };
        if direct == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Removes a tmux session, including its retained dead pane.
pub fn kill_session(home: &LatchHome, id: &SessionId) -> Result<()> {
    let mut command = tmux(home)?;
    command.args(["kill-session", "-t", id.as_str()]);
    run_tmux(command, "remove session")
}

/// Converts a dead pane into the public exit record.
pub fn exit_record(info: &SessionInfo) -> Option<ExitRecord> {
    (info.state == SessionState::Exited).then(|| ExitRecord {
        code: info.signal.map(|signal| 128 + signal).or(info.exit_status),
        signal: info.signal.map(signal_name),
        exited_at: format_rfc3339(info.exited_at.unwrap_or_else(SystemTime::now)),
    })
}

/// Absolute path to the pinned bundled tmux.
pub fn tmux_binary() -> Result<PathBuf> {
    if let Some(path) = tmux_override() {
        let path = fs::canonicalize(path).context("cannot resolve LATCH_TMUX_BIN")?;
        return Ok(path);
    }
    let executable =
        fs::canonicalize(std::env::current_exe().context("cannot locate the latch executable")?)?;
    let candidate = executable
        .parent()
        .expect("executable has a parent")
        .join(BUNDLED_TMUX_NAME);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        bail!(
            "bundled tmux {} is missing; run `latch update` to repair the complete payload",
            candidate.display()
        )
    }
}

/// Reports the bundled tmux version.
pub fn tmux_version() -> Result<String> {
    let output = Command::new(tmux_binary()?)
        .arg("-V")
        .output()
        .context("cannot run the bundled tmux")?;
    if !output.status.success() {
        return Err(tmux_error("read tmux version", output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn ensure_tmux_available() -> Result<()> {
    let binary = tmux_binary()?;
    let metadata = fs::metadata(&binary)
        .with_context(|| format!("cannot inspect bundled tmux {}", binary.display()))?;
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("bundled tmux {} is not executable", binary.display());
    }
    Ok(())
}

fn materialize_environment(manifest: &mut LaunchManifest) {
    if !manifest.launch.inherit_env {
        return;
    }
    let explicit = std::mem::take(&mut manifest.launch.env);
    manifest.launch.env = std::env::vars()
        .filter(|(key, _)| key != "TMUX" && key != "TERM")
        .collect();
    manifest.launch.env.extend(explicit);
    manifest.launch.inherit_env = false;
}

fn ensure_config(home: &LatchHome) -> Result<()> {
    home.ensure()?;
    let path = home.tmux_config();
    if fs::read_to_string(&path).ok().as_deref() == Some(CONFIG_BODY) {
        return Ok(());
    }
    let temp = home
        .root()
        .join(format!(".tmux.conf.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(FILE_MODE)
        .open(&temp)
        .with_context(|| format!("cannot write {}", temp.display()))?;
    file.write_all(CONFIG_BODY.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &path).with_context(|| format!("cannot replace {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn tmux(home: &LatchHome) -> Result<Command> {
    let mut command = Command::new(tmux_binary()?);
    command
        .arg("-S")
        .arg(home.server_socket())
        .arg("-f")
        .arg(home.tmux_config())
        .env_remove("TMUX");
    if !effective_ctype().as_deref().is_some_and(ctype_is_utf8) {
        // LC_ALL would outrank the injected LC_CTYPE, so it has to go.
        command.env_remove("LC_ALL").env("LC_CTYPE", UTF8_CTYPE);
    }
    Ok(command)
}

/// Effective `LC_CTYPE` value under POSIX precedence (`LC_ALL` over
/// `LC_CTYPE` over `LANG`); empty values count as unset.
fn effective_ctype() -> Option<String> {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
}

/// Matches tmux's own advertisement check: any spelling containing
/// `UTF-8`/`utf8` counts.
fn ctype_is_utf8(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8")
}

fn tmux_override() -> Option<std::ffi::OsString> {
    cfg!(debug_assertions)
        .then(|| std::env::var_os(TMUX_OVERRIDE_ENV))
        .flatten()
}

fn run_tmux(mut command: Command, action: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot {action} with the bundled tmux"))?;
    if !output.status.success() {
        return Err(tmux_error(action, output));
    }
    Ok(())
}

fn tmux_error(action: &str, output: Output) -> anyhow::Error {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    anyhow!("cannot {action}: {}", diagnostic.trim())
}

fn make_fifo(path: &Path) -> Result<()> {
    let path_bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    let path = std::ffi::CString::new(path_bytes).context("launch pipe path contains NUL")?;
    // SAFETY: the C string is valid for the duration of the call.
    if unsafe { libc::mkfifo(path.as_ptr(), FILE_MODE as libc::mode_t) } == -1 {
        return Err(std::io::Error::last_os_error()).context("cannot create launch pipe");
    }
    Ok(())
}

fn open_fifo_writer(path: &Path) -> Result<fs::File> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => {
                let descriptor = file.as_raw_fd();
                // SAFETY: the descriptor belongs to `file`; fcntl only reads
                // and updates its status flags.
                let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
                if flags == -1
                    || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags & !libc::O_NONBLOCK) }
                        == -1
                {
                    return Err(std::io::Error::last_os_error())
                        .context("cannot make the launch pipe blocking");
                }
                return Ok(file);
            }
            Err(error)
                if error.raw_os_error() == Some(libc::ENXIO)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot open launch pipe {}", path.display()));
            }
        }
    }
}

fn ensure_interactive_login_shell(argv: &mut Vec<String>) {
    let Some(program) = argv.first() else {
        return;
    };
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !matches!(name, "zsh" | "bash" | "sh" | "ksh" | "dash") {
        return;
    }
    match argv.get(1).map(String::as_str) {
        None => argv.insert(1, "-il".to_owned()),
        Some("-c") => argv[1] = "-ilc".to_owned(),
        Some(flags)
            if flags.starts_with('-')
                && !flags.starts_with("--")
                && flags.chars().skip(1).all(|c| c.is_ascii_alphabetic()) =>
        {
            let mut letters: String = flags.chars().skip(1).collect();
            if !letters.contains('i') {
                letters.insert(0, 'i');
            }
            if letters.contains('c') && !letters.contains('l') {
                letters.insert(0, 'l');
            }
            argv[1] = format!("-{letters}");
        }
        _ => {}
    }
}

fn split_session_row(line: &str) -> Vec<&str> {
    line.split(SESSION_ROW_SEPARATOR).collect()
}

fn display_session_row(line: &str) -> String {
    line.replace(SESSION_ROW_SEPARATOR, ",")
}

/// Parses one session row, distinguishing a row tmux could not fully expand
/// from a row Latch cannot read at all.
///
/// tmux drops trailing empty format fields, so the columns that resolve
/// against a session's current window and active pane simply disappear while
/// that window is being created or torn down. Those rows are transient: the
/// session is real, its live facts are not knowable yet, and the next query
/// returns a complete row. They are reported as `None` so one session in
/// motion cannot fail a whole listing. Rows that are malformed for any other
/// reason — separators sanitized away by a non-UTF-8 client locale, junk in a
/// numeric column — remain hard errors with the row in the message.
fn parse_row(line: &str) -> Result<Option<SessionInfo>> {
    let fields = split_session_row(line);
    // Live panes omit trailing empty dead-time/signal fields on some tmux
    // queries; require the eight live columns and treat the rest as optional.
    if fields.len() < 8 || fields.len() > 10 {
        if fields.len() < 8 && SessionId::parse(fields[0]).is_ok() {
            return Ok(None);
        }
        bail!(
            "tmux returned an unexpected session row: {}",
            display_session_row(line)
        );
    }
    let field = |index: usize| fields.get(index).copied().unwrap_or("");
    // A complete-looking row whose live columns are blank is the same
    // half-expanded session, caught one field later.
    if [3, 4, 5, 6, 7]
        .into_iter()
        .any(|index| field(index).is_empty())
    {
        return Ok(None);
    }
    let dead = field(1) == "1";
    let exited_at = (dead && !field(8).is_empty())
        .then(|| field(8).parse::<u64>())
        .transpose()?
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds));
    let activity = exited_at.unwrap_or(UNIX_EPOCH + Duration::from_secs(field(5).parse()?));
    let signal = (!field(9).is_empty())
        .then(|| parse_signal(field(9)))
        .transpose()?;
    Ok(Some(SessionInfo {
        id: field(0).to_owned(),
        state: if dead {
            SessionState::Exited
        } else {
            SessionState::Running
        },
        exit_status: (dead && !field(2).is_empty())
            .then(|| field(2).parse())
            .transpose()?,
        size: TerminalSize::new(field(3).parse()?, field(4).parse()?),
        activity,
        attached: field(6).parse()?,
        pane_pid: field(7).parse()?,
        exited_at,
        signal,
    }))
}

fn parse_signal(raw: &str) -> Result<i32> {
    match raw.to_ascii_lowercase().as_str() {
        "hup" | "sighup" => Ok(libc::SIGHUP),
        "int" | "sigint" => Ok(libc::SIGINT),
        "quit" | "sigquit" => Ok(libc::SIGQUIT),
        "kill" | "sigkill" => Ok(libc::SIGKILL),
        "term" | "sigterm" => Ok(libc::SIGTERM),
        _ => raw
            .parse()
            .with_context(|| format!("tmux returned an unknown pane signal `{raw}`")),
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGTERM => "SIGTERM",
        _ => return format!("SIG{signal}"),
    }
    .to_owned()
}

fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

/// Formats a system time in the UTC subset Latch emits.
pub fn format_rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // SAFETY: `gmtime_r` writes only into the supplied value.
    unsafe {
        let mut utc = std::mem::zeroed::<libc::tm>();
        if libc::gmtime_r(&secs, &mut utc).is_null() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_stderr_markers_classify_empty_server_outcomes() {
        assert_eq!(
            classify_tmux_stderr("error connecting to /tmp/server (No such file or directory)"),
            None
        );
        assert_eq!(
            classify_tmux_stderr("no server running on /tmp/latch/server"),
            Some(TmuxMiss::Server)
        );
        assert_eq!(
            classify_tmux_stderr("can't find session: ses_abc"),
            Some(TmuxMiss::Session)
        );
        assert_eq!(classify_tmux_stderr("no sessions"), Some(TmuxMiss::Empty));
    }

    #[test]
    fn tmux_signal_names_map_to_platform_numbers() {
        assert_eq!(parse_signal("kill").unwrap(), libc::SIGKILL);
        assert_eq!(parse_signal("SIGTERM").unwrap(), libc::SIGTERM);
        assert_eq!(parse_signal("2").unwrap(), 2);
        assert!(parse_signal("unknown").is_err());
    }

    #[test]
    fn dash_c_shells_become_interactive_login_shells() {
        let mut zsh_lc = vec![
            "/bin/zsh".to_owned(),
            "-lc".to_owned(),
            "agp cursor".to_owned(),
        ];
        ensure_interactive_login_shell(&mut zsh_lc);
        assert_eq!(zsh_lc[1], "-ilc");

        let mut sh_c = vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 7".to_owned()];
        ensure_interactive_login_shell(&mut sh_c);
        assert_eq!(sh_c[1], "-ilc");

        let mut login = vec!["/bin/zsh".to_owned(), "-l".to_owned()];
        ensure_interactive_login_shell(&mut login);
        assert_eq!(login[1], "-il");

        let mut cursor = vec!["cursor".to_owned(), "agent".to_owned()];
        ensure_interactive_login_shell(&mut cursor);
        assert_eq!(cursor, ["cursor", "agent"]);
    }

    fn session_row(fields: &[&str]) -> String {
        fields.join(&SESSION_ROW_SEPARATOR.to_string())
    }

    #[test]
    fn live_and_dead_tmux_rows_parse_with_empty_trailing_fields() {
        let live = parse_row(&session_row(&[
            "ses_19ffc24df42abdc0",
            "0",
            "",
            "144",
            "25",
            "1786641713",
            "1",
            "43998",
            "",
            "",
        ]))
        .expect("live row")
        .expect("complete row");
        assert_eq!(live.id, "ses_19ffc24df42abdc0");
        assert_eq!(live.state, SessionState::Running);
        assert_eq!(live.size, TerminalSize::new(144, 25));
        assert_eq!(live.attached, 1);
        assert_eq!(live.pane_pid, 43998);
        assert!(live.exit_status.is_none());

        let live_short = parse_row(&session_row(&[
            "ses_19ffc24df42abdc0",
            "0",
            "",
            "144",
            "25",
            "1786641713",
            "1",
            "43998",
        ]))
        .expect("live row without trailing dead fields")
        .expect("complete row");
        assert_eq!(live_short.state, SessionState::Running);

        let dead = parse_row(&session_row(&[
            "ses_19ffc224e3183df0",
            "1",
            "127",
            "138",
            "25",
            "1786641544",
            "1",
            "33762",
            "1786641534",
            "",
        ]))
        .expect("dead row")
        .expect("complete row");
        assert_eq!(dead.state, SessionState::Exited);
        assert_eq!(dead.exit_status, Some(127));
        assert_eq!(dead.pane_pid, 33762);
    }

    #[test]
    fn sanitized_session_rows_fail_with_a_readable_error() {
        // tmux with a non-UTF-8 client locale flattens U+001F to '_'; the
        // parse error must show the row instead of invisible separators.
        let error = parse_row("ses_abc_0__110_25_1786641713_1_9__").expect_err("sanitized row");
        assert!(error.to_string().contains("unexpected session row"));
    }

    #[test]
    fn half_expanded_session_rows_are_transient_rather_than_errors() {
        // tmux drops trailing empty fields, so a session whose current window
        // is not resolvable yet arrives as a bare id.
        assert!(parse_row("ses_1a009ce1f135e990")
            .expect("bare id is transient")
            .is_none());
        assert!(parse_row(&session_row(&["ses_1a009ce1f135e990", "0", ""]))
            .expect("truncated row is transient")
            .is_none());
        // The same session caught one field later: every column is present but
        // the pane-backed ones are blank.
        assert!(parse_row(&session_row(&[
            "ses_1a009ce1f135e990",
            "0",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
        ]))
        .expect("blank live columns are transient")
        .is_none());
    }

    #[test]
    fn transient_rows_are_skipped_without_hiding_real_sessions() {
        let rows = [
            "ses_1a009ce1f135e990".to_owned(),
            session_row(&[
                "ses_19ffc24df42abdc0",
                "0",
                "",
                "144",
                "25",
                "1786641713",
                "1",
                "43998",
            ]),
        ];
        let parsed: Vec<SessionInfo> = rows
            .iter()
            .filter_map(|line| parse_row(line).transpose())
            .collect::<Result<_>>()
            .expect("a session in motion cannot fail the listing");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "ses_19ffc24df42abdc0");
    }

    #[test]
    fn ctype_detection_matches_tmux_utf8_advertisement() {
        assert!(ctype_is_utf8("en_US.UTF-8"));
        assert!(ctype_is_utf8("C.UTF-8"));
        assert!(ctype_is_utf8("UTF-8"));
        assert!(ctype_is_utf8("en_US.utf8"));
        assert!(ctype_is_utf8("de_DE.UTF-8@euro"));
        assert!(!ctype_is_utf8("C"));
        assert!(!ctype_is_utf8("POSIX"));
        assert!(!ctype_is_utf8("en_US.ISO8859-1"));
    }
}
