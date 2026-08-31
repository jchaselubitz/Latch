//! Latch's headless session kernel.
#![allow(missing_docs)]

use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::session::manifest::{self, LaunchManifest, TerminalSize};
use crate::session::meta::{ExitRecord, SessionMeta};
use crate::session::paths::{LatchHome, SessionId, SessionPaths, FILE_MODE, SESSION_ID_ENV};
use crate::session::{timing, viewer};

mod latchd_kernel;
pub use latchd_kernel::{latchd_binary, latchd_version, BUNDLED_LATCHD_NAME};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    Latchd,
}
impl Kernel {
    pub const fn as_str(self) -> &'static str {
        "latchd"
    }
}
pub const fn kernel() -> Kernel {
    Kernel::Latchd
}

pub fn session_kernel(home: &LatchHome, id: &SessionId) -> Result<Kernel> {
    let session_dir = home.session(id);
    if !session_dir.dir().exists() {
        return Ok(Kernel::Latchd);
    }
    let record = latchd::paths::KernelRecord::read(session_dir.dir())
        .with_context(|| format!("cannot read the kernel identity for {id}"))?;
    match record.as_ref().map(|record| record.kernel.as_str()) {
        Some(latchd::paths::KERNEL_NAME) => Ok(Kernel::Latchd),
        Some(other) => bail!("session {id} names unsupported retired kernel `{other}`"),
        None => bail!("session {id} predates the latchd-only release; use an older Latch release to close or export it"),
    }
}

pub const PROTOCOL_VERSION: u32 = 2;
pub const DEFAULT_TERMINAL: &str = "xterm-256color";
pub const BUNDLED_REMOTE_NAME: &str = "latch-remote";
const FIRST_VIEWER_GRACE: Duration = Duration::from_secs(3);
const FIRST_VIEWER_MAX_WAIT: Duration = Duration::from_secs(30);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STOP_GRACE_POLLS: usize = 100;
const STOP_FORCE_POLLS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Running,
    Exited,
    Lost,
}
impl SessionState {
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Lost => "lost",
        }
    }
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub state: SessionState,
    pub size: TerminalSize,
    pub activity: SystemTime,
    pub attached: usize,
    pub pane_pid: i32,
    pub exit_status: Option<i32>,
    pub exited_at: Option<SystemTime>,
    pub signal: Option<i32>,
}
pub struct CreateRequest {
    pub home: LatchHome,
    pub manifest: LaunchManifest,
}
pub struct CreateResult {
    pub id: SessionId,
    pub paths: SessionPaths,
    pub meta: SessionMeta,
}
pub struct ResizeRequest<'a> {
    pub home: &'a LatchHome,
    pub id: &'a SessionId,
    pub size: TerminalSize,
    pub pin: bool,
}
pub struct StopRequest<'a> {
    pub home: &'a LatchHome,
    pub id: &'a SessionId,
    pub force: bool,
}
pub struct PasteMessageRequest<'a> {
    pub home: &'a LatchHome,
    pub id: &'a SessionId,
    pub message: &'a [u8],
}
pub struct SendKeysRequest<'a> {
    pub home: &'a LatchHome,
    pub id: &'a SessionId,
    pub keys: &'a [String],
}

pub fn create(request: CreateRequest) -> Result<CreateResult> {
    latchd_kernel::create(request)
}

pub fn launch_from_fifo(path: &Path) -> Result<()> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("cannot read launch pipe {}", path.display()))?;
    let mut manifest = manifest::read(&mut file)?;
    drop(file);
    let _ = fs::remove_file(path);
    wait_for_first_viewer(&manifest);
    ensure_interactive_login_shell(&mut manifest.launch.argv);
    let mut command = Command::new(&manifest.launch.argv[0]);
    command
        .args(&manifest.launch.argv[1..])
        .current_dir(&manifest.launch.cwd);
    if !manifest.launch.inherit_env {
        command.env_clear();
    }
    command.env_remove("TMUX").env("TERM", DEFAULT_TERMINAL);
    if let Some(id) = std::env::var_os(SESSION_ID_ENV) {
        command.env(SESSION_ID_ENV, id);
    }
    for (key, value) in &manifest.launch.env {
        if key != "TMUX" && key != "TERM" {
            command.env(key, value);
        }
    }
    Err(command.exec()).context("cannot execute session command")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRelease {
    Normal,
    Stolen,
    SlowClient,
    SessionExited,
}
impl SurfaceRelease {
    const EXIT_STOLEN: i32 = 75;
    const EXIT_SLOW_CLIENT: i32 = 76;
    const EXIT_SESSION_EXITED: i32 = 77;
    pub fn from_exit_code(code: Option<i32>) -> Option<Self> {
        match code? {
            0 => Some(Self::Normal),
            Self::EXIT_STOLEN => Some(Self::Stolen),
            Self::EXIT_SLOW_CLIENT => Some(Self::SlowClient),
            Self::EXIT_SESSION_EXITED => Some(Self::SessionExited),
            _ => None,
        }
    }
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Stolen => Self::EXIT_STOLEN,
            Self::SlowClient => Self::EXIT_SLOW_CLIENT,
            Self::SessionExited => Self::EXIT_SESSION_EXITED,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Stolen => "stolen",
            Self::SlowClient => "slow_client",
            Self::SessionExited => "session_exited",
        }
    }
}

pub fn attach_exclusive(home: &LatchHome, id: &SessionId) -> Result<SurfaceRelease> {
    session_kernel(home, id)?;
    latchd_kernel::attach_exclusive(home, id)
}

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
    let outcome = latchd_kernel::await_first_viewer(&home, &id, &paths);
    timing::record(
        &paths,
        "launch.first_viewer_wait",
        watch.total(),
        Some(outcome),
    );
    viewer::clear(&paths);
}

pub fn attached_clients(home: &LatchHome, id: &SessionId) -> Option<usize> {
    latchd_kernel::inspect(home, id)
        .ok()
        .flatten()
        .map(|info| info.attached)
}
pub fn surface_attached(home: &LatchHome, id: &SessionId) -> bool {
    latchd_kernel::surface_attached(home, id)
}
pub fn has_session(home: &LatchHome, id: &SessionId) -> bool {
    latchd_kernel::has_session(home, id)
}
pub fn list(home: &LatchHome) -> Result<Vec<SessionInfo>> {
    latchd_kernel::list(home)
}
pub fn inspect(home: &LatchHome, id: &SessionId) -> Result<Option<SessionInfo>> {
    session_kernel(home, id)?;
    latchd_kernel::inspect(home, id)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CapturePaneOptions {
    pub styled: bool,
    pub scrollback_lines: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneMetrics {
    pub cols: u16,
    pub rows: u16,
    pub alternate_screen: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationScreen {
    pub lines: Vec<String>,
    pub history: Vec<String>,
    pub alternate_screen: bool,
    pub title: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationWake {
    Timeout,
    Resynchronized,
    OutputQuiet { ms: u64 },
    ChildExited,
    AlternateScreen { active: bool },
    TitleChanged { title: Option<String> },
    SurfaceChanged,
}

pub struct ConversationControl {
    id: SessionId,
    socket: PathBuf,
    control: Option<latchd::client::Client>,
    events: Option<latchd::client::Subscription>,
}
impl std::fmt::Debug for ConversationControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationControl")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl ConversationControl {
    pub fn open(home: &LatchHome, id: &SessionId) -> Result<Self> {
        session_kernel(home, id)?;
        let record = latchd::paths::KernelRecord::read(home.session(id).dir())?
            .ok_or_else(|| anyhow!("session {id} has no latchd kernel record"))?;
        Ok(Self {
            id: id.clone(),
            socket: record.socket,
            control: None,
            events: None,
        })
    }
    pub const fn is_event_driven(&self) -> bool {
        true
    }
    pub fn wait_for_activity(
        &mut self,
        _fallback_poll: Duration,
        event_timeout: Duration,
    ) -> Result<ConversationWake> {
        if self.events.is_none() {
            self.events = Some(
                latchd::client::Client::connect_with_timeout(&self.socket, event_timeout)?
                    .subscribe()?,
            );
            return Ok(ConversationWake::Resynchronized);
        }
        let event = match self
            .events
            .as_mut()
            .expect("subscription installed")
            .recv_timeout(event_timeout)
        {
            Ok(Some(event)) => event,
            Ok(None) => return Ok(ConversationWake::Timeout),
            Err(_) => {
                self.events = None;
                return Ok(ConversationWake::Resynchronized);
            }
        };
        Ok(match event {
            latchd::protocol::Event::OutputQuiet { ms } => ConversationWake::OutputQuiet { ms },
            latchd::protocol::Event::ChildExited { .. } => ConversationWake::ChildExited,
            latchd::protocol::Event::AltScreen { active } => {
                ConversationWake::AlternateScreen { active }
            }
            latchd::protocol::Event::TitleChanged { title } => {
                ConversationWake::TitleChanged { title }
            }
            latchd::protocol::Event::SurfaceAttached { .. }
            | latchd::protocol::Event::SurfaceDetached { .. } => ConversationWake::SurfaceChanged,
        })
    }
    pub fn snapshot(&mut self, timeout: Duration) -> Result<ConversationScreen> {
        let reply = self.query(
            &latchd::protocol::Request::Snapshot {
                format: latchd::protocol::SnapshotFormat::Json,
                scrollback_lines: 0,
            },
            timeout,
        )?;
        let screen = reply.screen.ok_or_else(|| {
            anyhow!(
                "kernel returned no structured screen for session {}",
                self.id
            )
        })?;
        let lines = screen["lines"]
            .as_array()
            .ok_or_else(|| anyhow!("kernel returned an invalid structured screen"))?
            .iter()
            .filter_map(|line| line.as_str().map(str::to_owned))
            .collect();
        let alternate_screen = screen["alternate_screen"].as_bool().unwrap_or(false);
        let title = screen["title"].as_str().map(str::to_owned);
        let history = if alternate_screen {
            Vec::new()
        } else {
            self.query(&latchd::protocol::Request::History { max: 200 }, timeout)?
                .lines
                .unwrap_or_default()
        };
        Ok(ConversationScreen {
            lines,
            history,
            alternate_screen,
            title,
        })
    }
    pub fn submit(&mut self, text: &str, timeout: Duration) -> Result<()> {
        self.action(
            &latchd::protocol::Request::Submit {
                text: text.to_owned(),
            },
            timeout,
        )
    }
    pub fn paste(&mut self, text: &str, timeout: Duration) -> Result<()> {
        self.action(
            &latchd::protocol::Request::Paste {
                text: text.to_owned(),
            },
            timeout,
        )
    }
    pub fn key(&mut self, keys: &[String], timeout: Duration) -> Result<()> {
        self.action(
            &latchd::protocol::Request::Key {
                keys: keys.to_vec(),
            },
            timeout,
        )
    }
    fn query(
        &mut self,
        request: &latchd::protocol::Request,
        timeout: Duration,
    ) -> Result<latchd::protocol::Reply> {
        let mut last = None;
        for _ in 0..2 {
            if self.control.is_none() {
                self.control = Some(latchd::client::Client::connect_with_timeout(
                    &self.socket,
                    timeout,
                )?);
            }
            let client = self.control.as_mut().expect("control installed");
            client.set_timeout(timeout)?;
            match client.call(request) {
                Ok(reply) => return Ok(reply),
                Err(error) => {
                    last = Some(error);
                    self.control = None;
                }
            }
        }
        Err(last.expect("query attempted"))
            .with_context(|| format!("persistent control query failed for session {}", self.id))
    }
    fn action(&mut self, request: &latchd::protocol::Request, timeout: Duration) -> Result<()> {
        if self.control.is_none() {
            self.control = Some(latchd::client::Client::connect_with_timeout(
                &self.socket,
                timeout,
            )?);
        }
        let client = self.control.as_mut().expect("control installed");
        client.set_timeout(timeout)?;
        let result = client.call(request);
        if result.is_err() {
            self.control = None;
        }
        result
            .map(|_| ())
            .with_context(|| format!("persistent control action failed for session {}", self.id))
    }
}

pub fn pane_metrics_with_timeout(
    home: &LatchHome,
    id: &SessionId,
    _timeout: Duration,
) -> Result<PaneMetrics> {
    session_kernel(home, id)?;
    latchd_kernel::pane_metrics(home, id)
}
pub fn capture_pane(home: &LatchHome, id: &SessionId) -> Result<String> {
    capture_pane_with_timeout(
        home,
        id,
        Duration::from_secs(30),
        CapturePaneOptions::default(),
    )
}
pub fn capture_pane_with_timeout(
    home: &LatchHome,
    id: &SessionId,
    timeout: Duration,
    options: CapturePaneOptions,
) -> Result<String> {
    session_kernel(home, id)?;
    latchd_kernel::capture_pane(home, id, timeout, options)
}
pub fn paste_message(request: PasteMessageRequest<'_>) -> Result<()> {
    paste_message_with_timeout(request, Duration::from_secs(30))
}
pub fn paste_message_with_timeout(
    request: PasteMessageRequest<'_>,
    timeout: Duration,
) -> Result<()> {
    session_kernel(request.home, request.id)?;
    latchd_kernel::paste_message(request, timeout)
}
pub fn send_keys(request: SendKeysRequest<'_>) -> Result<()> {
    send_keys_with_timeout(request, Duration::from_secs(30))
}
pub fn send_keys_with_timeout(request: SendKeysRequest<'_>, timeout: Duration) -> Result<()> {
    session_kernel(request.home, request.id)?;
    latchd_kernel::send_keys(request, timeout)
}
pub fn resize(request: ResizeRequest<'_>) -> Result<()> {
    session_kernel(request.home, request.id)?;
    latchd_kernel::resize(request)
}

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
    signal_child(&info, signal).with_context(|| format!("cannot signal session {}", request.id))?;
    let polls = if request.force {
        STOP_FORCE_POLLS
    } else {
        STOP_GRACE_POLLS
    };
    if wait_until_exited(request.home, request.id, polls)? {
        return Ok(SessionState::Exited);
    }
    if !request.force {
        signal_child(&info, libc::SIGKILL)
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
pub(crate) fn attach_is_retryable(home: &LatchHome, id: &SessionId) -> bool {
    inspect(home, id)
        .ok()
        .flatten()
        .is_some_and(|info| info.state == SessionState::Running)
}
fn signal_child(info: &SessionInfo, signal: i32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(-info.pane_pid, signal) };
    if result == -1 {
        let direct = unsafe { libc::kill(info.pane_pid, signal) };
        if direct == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
pub fn kill_session(home: &LatchHome, id: &SessionId) -> Result<()> {
    session_kernel(home, id)?;
    latchd_kernel::kill_session(home, id)
}
pub fn exit_record(info: &SessionInfo) -> Option<ExitRecord> {
    (info.state == SessionState::Exited).then(|| ExitRecord {
        code: info.signal.map(|signal| 128 + signal).or(info.exit_status),
        signal: info.signal.map(signal_name),
        exited_at: format_rfc3339(info.exited_at.unwrap_or_else(SystemTime::now)),
    })
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
fn make_fifo(path: &Path) -> Result<()> {
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    let path = std::ffi::CString::new(bytes).context("launch pipe path contains NUL")?;
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
                let fd = file.as_raw_fd();
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags == -1
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } == -1
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
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot open launch pipe {}", path.display()))
            }
        }
    }
}
fn ensure_interactive_login_shell(argv: &mut Vec<String>) {
    let Some(program) = argv.first() else { return };
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
            // Command shells also need login startup files.
            if letters.contains('c') && !letters.contains('l') {
                letters.insert(0, 'l');
            }
            argv[1] = format!("-{letters}");
        }
        _ => {}
    }
}
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP".to_owned(),
        libc::SIGINT => "SIGINT".to_owned(),
        libc::SIGQUIT => "SIGQUIT".to_owned(),
        libc::SIGKILL => "SIGKILL".to_owned(),
        libc::SIGTERM => "SIGTERM".to_owned(),
        _ => format!("SIG{signal}"),
    }
}
fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}
pub fn format_rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
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
