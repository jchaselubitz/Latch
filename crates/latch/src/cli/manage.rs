//! Session management commands: list, inspect, stop, rename, resize, prune,
//! doctor, config, capabilities.
//!
//! State is derived at read time from the socket and `exit.json` — never from a
//! stored status field. `stop` sends a message to the live worker; it never
//! reads a PID from storage.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use latch_protocol::control::{self, AttachMode, ClientInfo, ControlMessage, SessionState, Size};
use latch_protocol::frame::FrameType;
use latch_protocol::PROTOCOL_VERSION;

use crate::cli::attach;
use crate::cli::json::{
    AttachmentSummary, CapabilitiesReport, CapabilityFlags, ConfigReport, DoctorFinding,
    DoctorReport, InspectReport, ListReport, PruneReport, RenameReport, ResizeReport,
    RetainedSession, SessionSummary, StopReport,
};
use crate::worker::manifest::TerminalSize;
use crate::worker::meta::{self, SessionMeta};
use crate::worker::paths::{LatchHome, SessionId, SessionPaths, DIR_MODE, FILE_MODE};
use crate::worker::socket::{self, FrameStream};
use crate::worker::{self};

/// Options for `latch list`.
#[derive(Debug, Clone)]
pub struct ListOptions {
    /// Where sessions live.
    pub home: LatchHome,
}

/// Lists sessions, most recently active first, with idle time.
///
/// Must stay usable with thirty sessions open — with every window a session,
/// `list` is the primary navigation surface. If it is slow, fix how it probes,
/// not what it remembers: there is no cache and no stored status field.
pub fn list(options: ListOptions) -> anyhow::Result<ListReport> {
    let mut sessions = Vec::new();
    for id in session_ids_or_empty(&options.home)? {
        let paths = options.home.session(&id);
        let Some(summary) = summarize_session(&id, &paths)? else {
            continue;
        };
        sessions.push(summary);
    }
    sessions.sort_by(|left, right| match (left.idle_ms, right.idle_ms) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => right.id.cmp(&left.id),
    });
    Ok(ListReport { sessions })
}

/// Options for `latch inspect`.
#[derive(Debug, Clone)]
pub struct InspectOptions {
    /// Where sessions live.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
}

/// Everything known about one session.
pub fn inspect(options: InspectOptions) -> anyhow::Result<InspectReport> {
    let id = resolve_existing(&options.home, &options.session)?;
    let paths = options.home.session(&id);
    let meta = meta::read(&paths)?;
    let state = worker::derive_state(&paths)?;
    let exit = meta::read_exit(&paths)?;

    let mut size = None;
    let mut attachments = None;
    if matches!(
        state,
        SessionState::Running | SessionState::Creating | SessionState::Stopping
    ) {
        if let Ok(live) = probe_running(&paths, &meta) {
            size = Some(live.size);
            attachments = Some(live.attachments);
        }
    }

    Ok(InspectReport {
        id: meta.id,
        name: meta.name,
        title: meta.title,
        state: state.as_wire().to_owned(),
        cwd: meta.cwd,
        command_label: meta.command_label,
        created_at: meta.created_at,
        initial_size: meta.initial_size,
        size,
        exit,
        attachments,
    })
}

/// A stop request.
#[derive(Debug, Clone)]
pub struct StopRequest {
    /// Where sessions live.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// Skip the graceful signal and force immediately.
    pub force: bool,
}

/// Asks the live worker to stop its own child's process group.
///
/// Never reads a PID from storage. A session whose socket does not answer
/// cannot be stopped, because there is nothing left to stop.
pub fn stop(request: StopRequest) -> anyhow::Result<StopReport> {
    let _ = request.force;
    let id = resolve_existing(&request.home, &request.session)?;
    let paths = request.home.session(&id);
    if !socket::is_live_within(&paths.socket(), worker::LIVENESS_PATIENCE) {
        bail!("session {id} is not running; there is nothing to stop");
    }

    let stream = UnixStream::connect(paths.socket())
        .with_context(|| format!("cannot connect to session {id}"))?;
    let mut stream = FrameStream::new(stream);
    stream.send(
        FrameType::Control,
        &control::encode(&ControlMessage::SessionUpdate {
            state: Some(SessionState::Stopping),
            attachments: None,
            title: None,
        }),
    )?;
    stream.flush()?;
    // Hold the connection open until the worker has accepted and read the
    // stop request. Closing immediately races the accept loop and can drop the
    // message before it is observed.
    let state = wait_for_stop_state(&paths, Duration::from_secs(5));
    drop(stream);
    Ok(StopReport {
        id: id.to_string(),
        state: state.as_wire().to_owned(),
    })
}

/// A rename request.
#[derive(Debug, Clone)]
pub struct RenameRequest {
    /// Where sessions live.
    pub home: LatchHome,
    /// Session id or current name.
    pub session: String,
    /// The new name; sanitized at ingest before storage.
    pub name: String,
}

/// Changes a session's display name.
pub fn rename(request: RenameRequest) -> anyhow::Result<RenameReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    let paths = request.home.session(&id);
    let mut meta = meta::read(&paths)?;
    let name = meta::sanitize_display(&request.name);
    if name.is_empty() {
        bail!("session name must contain at least one printable character");
    }
    meta.name = name.clone();
    meta::update(&paths, &meta)?;
    Ok(RenameReport {
        id: id.to_string(),
        name,
    })
}

/// A resize request.
#[derive(Debug, Clone)]
pub struct ResizeRequest {
    /// Where sessions live.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Hold this size against controller-driven changes.
    pub pin: bool,
}

/// Sets a session's size explicitly.
pub fn resize(request: ResizeRequest) -> anyhow::Result<ResizeReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    let paths = request.home.session(&id);
    if !socket::is_live_within(&paths.socket(), worker::LIVENESS_PATIENCE) {
        bail!("session {id} is not running; cannot resize");
    }

    let stream = UnixStream::connect(paths.socket())
        .with_context(|| format!("cannot connect to session {id}"))?;
    let mut stream = FrameStream::new(stream);
    let attach = ControlMessage::Attach {
        protocol: PROTOCOL_VERSION,
        mode: AttachMode::Control,
        steal: true,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: "resize".to_owned(),
        },
        size: Size {
            cols: request.cols,
            rows: request.rows,
        },
    };
    stream.send(FrameType::Control, &control::encode(&attach))?;
    stream.flush()?;
    let Some(reply) = stream.recv()? else {
        bail!("session {id} closed before accepting the resize");
    };
    if reply.frame_type != FrameType::Control {
        bail!("session {id} sent output before accepting the resize");
    }
    match control::decode(&reply.payload)? {
        ControlMessage::Attached { .. } => {}
        ControlMessage::Error { code, message } => bail!("{code}: {message}"),
        message => bail!(
            "session {id} sent `{}` instead of an attachment response",
            message.discriminator()
        ),
    }

    stream.send(
        FrameType::Control,
        &control::encode(&ControlMessage::Resize {
            cols: request.cols,
            rows: request.rows,
        }),
    )?;
    stream.flush()?;

    Ok(ResizeReport {
        id: id.to_string(),
        cols: request.cols,
        rows: request.rows,
        pinned: request.pin,
    })
}

/// How long an exited session stays readable before `prune` will reclaim it.
///
/// A day, because the question this answers is "I was away from my desk and
/// something finished — can I still see how it went?", and the honest span of
/// being away is overnight. Shorter and the answer is no for the ordinary case;
/// longer and `latch prune` stops being something a person can run without
/// thinking, which makes it something they stop running. The reasoning and what
/// would justify changing it are in `docs/DECISION_SCROLLBACK.md`.
///
/// The window applies to `exited` sessions only. A `lost` session — no socket,
/// no exit record — has nothing to show and is reclaimed on sight.
pub const DEFAULT_EXITED_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Options for `latch prune`.
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Where sessions live.
    pub home: LatchHome,
    /// Show what would be reclaimed without reclaiming it.
    pub dry_run: bool,
    /// How long an exited session is kept before it may be reclaimed.
    pub retain_exited: Duration,
}

impl PruneOptions {
    /// The ordinary options: reclaim, keeping recently exited sessions.
    pub fn new(home: LatchHome) -> Self {
        Self {
            home,
            dry_run: false,
            retain_exited: DEFAULT_EXITED_RETENTION,
        }
    }
}

/// Reclaims directories for exited and lost sessions.
///
/// An exited session is not reclaimed until it has aged out of the retention
/// window: it is still attachable read-only until then, and reclaiming it is
/// what makes that stop being true. `retain_exited` of zero reclaims every
/// exited session, which is what `--all` asks for.
pub fn prune(options: PruneOptions) -> anyhow::Result<PruneReport> {
    let mut reclaimed = Vec::new();
    let mut retained = Vec::new();
    for id in session_ids_or_empty(&options.home)? {
        let paths = options.home.session(&id);
        let state = worker::derive_state(&paths)?;
        if !matches!(state, SessionState::Exited | SessionState::Lost) {
            continue;
        }
        if state == SessionState::Exited && !options.retain_exited.is_zero() {
            let age = exited_age(&paths)?;
            if age < options.retain_exited {
                retained.push(RetainedSession {
                    id: id.to_string(),
                    // Reported rather than merely honored: a `prune` that
                    // silently skipped sessions would look like a `prune` that
                    // does not work.
                    reclaimable_in_ms: (options.retain_exited - age).as_millis() as u64,
                });
                continue;
            }
        }
        reclaimed.push(id.to_string());
        if !options.dry_run {
            paths.remove()?;
        }
    }
    reclaimed.sort();
    retained.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(PruneReport {
        reclaimed,
        retained,
        dry_run: options.dry_run,
    })
}

/// How long ago a session exited.
///
/// The exit record's own timestamp is authoritative; its mtime is the fallback
/// for a record written by a build that did not have one, and for a clock that
/// has moved backwards since. A session whose exit time is in the future is
/// treated as having just exited rather than as infinitely old — the safe
/// direction, since the cost of keeping a directory a day too long is a
/// directory and the cost of reclaiming one too early is the screen a person
/// came back to read.
fn exited_age(paths: &SessionPaths) -> anyhow::Result<Duration> {
    let exited_at = match meta::read_exit(paths)? {
        Some(exit) => parse_rfc3339(&exit.exited_at).unwrap_or_else(|| file_mtime(&paths.exit())),
        None => file_mtime(&paths.exit()),
    };
    Ok(SystemTime::now()
        .duration_since(exited_at)
        .unwrap_or_default())
}

/// Options for `latch doctor`.
#[derive(Debug, Clone)]
pub struct DoctorOptions {
    /// Where sessions live.
    pub home: LatchHome,
}

/// Reports what is actually wrong with an installation.
pub fn doctor(options: DoctorOptions) -> anyhow::Result<DoctorReport> {
    let mut findings = Vec::new();
    let root = options.home.root();
    if !root.exists() {
        findings.push(DoctorFinding {
            code: "home_missing".to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "Latch home {} does not exist yet; it is created on first session",
                root.display()
            ),
        });
        return Ok(DoctorReport { findings });
    }
    push_mode_finding(&mut findings, root, DIR_MODE, "home_permissions");
    let sessions = options.home.sessions_dir();
    if sessions.exists() {
        push_mode_finding(&mut findings, &sessions, DIR_MODE, "sessions_permissions");
    }

    let mut lost = 0usize;
    for id in session_ids_or_empty(&options.home)? {
        let paths = options.home.session(&id);
        match worker::derive_state(&paths) {
            Ok(SessionState::Lost) => lost += 1,
            Ok(_) => {}
            Err(error) => findings.push(DoctorFinding {
                code: "session_unreadable".to_owned(),
                severity: "error".to_owned(),
                message: format!("session {id}: {error}"),
            }),
        }
        if paths.meta().is_file() {
            push_mode_finding(&mut findings, &paths.meta(), FILE_MODE, "meta_permissions");
        }
    }
    if lost > 0 {
        findings.push(DoctorFinding {
            code: "lost_sessions".to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "{lost} session(s) have neither a live socket nor exit.json; run `latch prune`"
            ),
        });
    }

    Ok(DoctorReport { findings })
}

/// A config read or write.
#[derive(Debug, Clone)]
pub struct ConfigRequest {
    /// Where config lives.
    pub home: LatchHome,
    /// Preference key; `None` lists everything.
    pub key: Option<String>,
    /// New value; `None` reads.
    pub value: Option<String>,
}

/// Reads or writes non-secret preferences in `config.toml`.
pub fn config(request: ConfigRequest) -> anyhow::Result<ConfigReport> {
    request.home.ensure()?;
    let path = request.home.config_file();
    let mut values = read_config(&path)?;

    match (&request.key, &request.value) {
        (None, None) => Ok(ConfigReport {
            key: None,
            value: None,
            config: Some(serde_json::to_value(&values)?),
        }),
        (Some(key), None) => {
            let value = values.get(key).cloned();
            Ok(ConfigReport {
                key: Some(key.clone()),
                value,
                config: None,
            })
        }
        (Some(key), Some(value)) => {
            values.insert(key.clone(), value.clone());
            write_config(&path, &values)?;
            Ok(ConfigReport {
                key: Some(key.clone()),
                value: Some(value.clone()),
                config: None,
            })
        }
        (None, Some(_)) => bail!("a config key is required when writing a value"),
    }
}

/// Reports protocol versions and features this build supports.
pub fn capabilities() -> CapabilitiesReport {
    CapabilitiesReport {
        protocol_version: PROTOCOL_VERSION,
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: CapabilityFlags {
            create: true,
            open_viewer: true,
            local_attach: true,
            cloud_attach: false,
            extensions: Vec::new(),
        },
    }
}

struct LiveProbe {
    size: TerminalSize,
    attachments: Vec<AttachmentSummary>,
}

fn summarize_session(
    _id: &SessionId,
    paths: &SessionPaths,
) -> anyhow::Result<Option<SessionSummary>> {
    let meta = match meta::read(paths) {
        Ok(meta) => meta,
        Err(meta::MetaError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let state = worker::derive_state(paths)?;
    let (last_activity_at, idle_ms) = activity_for(paths, &meta, state)?;
    Ok(Some(SessionSummary {
        id: meta.id,
        name: meta.name,
        title: meta.title,
        state: state.as_wire().to_owned(),
        cwd: meta.cwd,
        command_label: meta.command_label,
        created_at: meta.created_at,
        last_activity_at,
        idle_ms,
    }))
}

fn activity_for(
    paths: &SessionPaths,
    meta: &SessionMeta,
    state: SessionState,
) -> anyhow::Result<(Option<String>, Option<u64>)> {
    let activity = if let Some(exit) = meta::read_exit(paths)? {
        parse_rfc3339(&exit.exited_at).unwrap_or_else(|| file_mtime(&paths.exit()))
    } else if let Some(mtime) = optional_mtime(&paths.journal()) {
        mtime
    } else if let Some(mtime) = optional_mtime(&paths.meta()) {
        mtime
    } else {
        parse_rfc3339(&meta.created_at).unwrap_or(SystemTime::UNIX_EPOCH)
    };
    let last_activity_at = Some(format_rfc3339(activity));
    let idle_ms = Some(
        SystemTime::now()
            .duration_since(activity)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    let _ = state;
    Ok((last_activity_at, idle_ms))
}

fn probe_running(paths: &SessionPaths, meta: &SessionMeta) -> anyhow::Result<LiveProbe> {
    let stream = UnixStream::connect(paths.socket())?;
    let mut stream = FrameStream::new(stream);
    let request = ControlMessage::Attach {
        protocol: PROTOCOL_VERSION,
        mode: AttachMode::Watch,
        steal: false,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: "inspect".to_owned(),
        },
        size: Size {
            cols: meta.initial_size.cols,
            rows: meta.initial_size.rows,
        },
    };
    stream.send(FrameType::Control, &control::encode(&request))?;
    stream.flush()?;
    let Some(reply) = stream.recv()? else {
        bail!("session closed before answering inspect");
    };
    if reply.frame_type != FrameType::Control {
        bail!("session sent output before answering inspect");
    }
    match control::decode(&reply.payload)? {
        ControlMessage::Attached {
            session,
            attachments,
            ..
        } => Ok(LiveProbe {
            size: TerminalSize::new(session.size.cols, session.size.rows),
            attachments: attachments
                .into_iter()
                .map(|attachment| AttachmentSummary {
                    id: attachment.id,
                    mode: attachment.mode.as_wire().to_owned(),
                    client_kind: attachment.client.kind,
                    client_name: attachment.client.name,
                })
                .collect(),
        }),
        ControlMessage::Error { code, message } => bail!("{code}: {message}"),
        message => bail!(
            "session sent `{}` instead of an attachment response",
            message.discriminator()
        ),
    }
}

fn wait_for_stop_state(paths: &SessionPaths, timeout: Duration) -> SessionState {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(state) = worker::derive_state(paths) {
            if !matches!(state, SessionState::Running | SessionState::Creating) {
                return state;
            }
        }
        if std::time::Instant::now() >= deadline {
            return SessionState::Stopping;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn resolve_existing(home: &LatchHome, session: &str) -> anyhow::Result<SessionId> {
    let id = attach::resolve_session(home, session)?;
    let paths = home.session(&id);
    if !paths.meta().is_file() {
        bail!("no session `{session}`");
    }
    Ok(id)
}

fn session_ids_or_empty(home: &LatchHome) -> anyhow::Result<Vec<SessionId>> {
    match home.session_ids() {
        Ok(ids) => Ok(ids),
        Err(crate::worker::paths::PathsError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

fn push_mode_finding(findings: &mut Vec<DoctorFinding>, path: &Path, expected: u32, code: &str) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode != expected {
        findings.push(DoctorFinding {
            code: code.to_owned(),
            severity: "error".to_owned(),
            message: format!(
                "{} has mode {:04o}; expected {:04o}",
                path.display(),
                mode,
                expected
            ),
        });
    }
}

fn read_config(path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut values = BTreeMap::new();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "{}:{}: expected `key = value`",
                path.display(),
                line_number + 1
            );
        };
        let key = key.trim().to_owned();
        let value = unquote(value.trim());
        values.insert(key, value);
    }
    Ok(values)
}

fn write_config(path: &Path, values: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let mut body = String::new();
    for (key, value) in values {
        body.push_str(key);
        body.push_str(" = ");
        body.push('"');
        body.push_str(&value.replace('\\', "\\\\").replace('"', "\\\""));
        body.push('"');
        body.push('\n');
    }
    let parent = path.parent().context("config path has no parent")?;
    let temp = parent.join(format!(".config.toml.tmp-{}", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temp)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        trimmed.to_owned()
    }
}

fn optional_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn file_mtime(path: &Path) -> SystemTime {
    optional_mtime(path).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn format_rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // SAFETY: `gmtime_r` writes only into the supplied `tm`.
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

fn parse_rfc3339(raw: &str) -> Option<SystemTime> {
    // Accept the subset Latch itself writes: `YYYY-MM-DDTHH:MM:SSZ`.
    if raw.len() < 20 || !raw.ends_with('Z') {
        return None;
    }
    let bytes = raw.as_bytes();
    let year = atoi(&bytes[0..4])?;
    let month = atoi(&bytes[5..7])?;
    let day = atoi(&bytes[8..10])?;
    let hour = atoi(&bytes[11..13])?;
    let minute = atoi(&bytes[14..16])?;
    let second = atoi(&bytes[17..19])?;
    // SAFETY: `timegm` interprets the broken-down UTC time.
    let secs = unsafe {
        let mut tm = std::mem::zeroed::<libc::tm>();
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_sec = second;
        libc::timegm(&mut tm)
    };
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

fn atoi(bytes: &[u8]) -> Option<i32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}
