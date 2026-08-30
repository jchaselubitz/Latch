//! Session management backed by direct tmux queries.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context};

use crate::cli::attach;
use crate::cli::json::{
    CapabilitiesReport, CapabilityFlags, ConfigReport, DoctorFinding, DoctorReport, InspectReport,
    ListReport, PruneReport, RemoveReport, RenameReport, ResizeReport, RetainedSession,
    SessionSummary, StopAllReport, StopReport,
};
use crate::conversation::connector_kind;
use crate::engine::{self, SessionState, PROTOCOL_VERSION};
use crate::session::manifest::TerminalSize;
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId, DIR_MODE, FILE_MODE};
use crate::session::timing;

/// Default dead-pane retention.
pub const DEFAULT_EXITED_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Options for listing sessions.
#[derive(Debug, Clone)]
pub struct ListOptions {
    /// Latch state root.
    pub home: LatchHome,
}

/// Lists metadata enriched with live tmux state.
pub fn list(options: ListOptions) -> anyhow::Result<ListReport> {
    let live = engine::list(&options.home)?
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<HashMap<_, _>>();
    let now = SystemTime::now();
    let mut sessions = Vec::new();
    for id in session_ids_or_empty(&options.home)? {
        let paths = options.home.session(&id);
        let metadata = match meta::read(&paths) {
            Ok(metadata) => metadata,
            Err(meta::MetaError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let info = live.get(id.as_str());
        let state = info.map_or(SessionState::Lost, |value| value.state);
        let activity = info.map_or_else(
            || {
                fs::metadata(paths.meta())
                    .and_then(|value| value.modified())
                    .unwrap_or(now)
            },
            |value| value.activity,
        );
        // The same helper the Conversation Hub builds from, so a routing
        // client and the Hub can never disagree about one session.
        let connector = connector_kind(metadata.harness.as_deref()).map(str::to_owned);
        sessions.push(SessionSummary {
            id: metadata.id,
            name: metadata.name,
            title: metadata.title,
            state: state.as_wire().to_owned(),
            cwd: metadata.cwd,
            command_label: metadata.command_label,
            created_at: metadata.created_at,
            last_activity_at: Some(engine::format_rfc3339(activity)),
            idle_ms: Some(now.duration_since(activity).unwrap_or_default().as_millis() as u64),
            connector,
        });
    }
    sessions.sort_by_key(|session| session.idle_ms);
    Ok(ListReport { sessions })
}

/// Options for inspecting one session.
#[derive(Debug, Clone)]
pub struct InspectOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
}

/// Returns all public facts about one session.
pub fn inspect(options: InspectOptions) -> anyhow::Result<InspectReport> {
    let id = resolve_existing(&options.home, &options.session)?;
    let metadata = meta::read(&options.home.session(&id))?;
    let info = engine::inspect(&options.home, &id)?;
    let state = info
        .as_ref()
        .map_or(SessionState::Lost, |value| value.state);
    let connector = connector_kind(metadata.harness.as_deref()).map(str::to_owned);
    Ok(InspectReport {
        id: metadata.id,
        name: metadata.name,
        title: metadata.title,
        state: state.as_wire().to_owned(),
        kernel: engine::session_kernel(&options.home, &id)?
            .as_str()
            .to_owned(),
        cwd: metadata.cwd,
        command_label: metadata.command_label,
        created_at: metadata.created_at,
        initial_size: metadata.initial_size,
        size: info.as_ref().map(|value| value.size),
        exit: info.as_ref().and_then(engine::exit_record),
        // The kernel invariant is one raw human surface. Do not expose the
        // tmux client count: administrative commands are clients too.
        surface_attached: info
            .as_ref()
            .map(|_| engine::surface_attached(&options.home, &id)),
        launch_timings: timing::read(&options.home.session(&id)),
        connector,
    })
}

/// Stop request.
#[derive(Debug, Clone)]
pub struct StopRequest {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// Send SIGKILL immediately.
    pub force: bool,
}

/// Stops the hosted process while preserving its dead pane.
pub fn stop(request: StopRequest) -> anyhow::Result<StopReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    let state = engine::stop(engine::StopRequest {
        home: &request.home,
        id: &id,
        force: request.force,
    })?;
    Ok(StopReport {
        id: id.to_string(),
        state: state.as_wire().to_owned(),
        stopped: state != SessionState::Running,
    })
}

/// Stops every currently running session while retaining their dead panes.
pub fn stop_all(home: LatchHome, force: bool) -> anyhow::Result<StopAllReport> {
    let sessions = list(ListOptions { home: home.clone() })?
        .sessions
        .into_iter()
        .filter(|session| session.state == SessionState::Running.as_wire())
        .map(|session| {
            stop(StopRequest {
                home: home.clone(),
                session: session.id,
                force,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(StopAllReport { sessions })
}

/// Removal request.
#[derive(Debug, Clone)]
pub struct RemoveRequest {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// Stop a live process first.
    pub force: bool,
}

/// Removes the tmux session and metadata sidecar.
pub fn remove(request: RemoveRequest) -> anyhow::Result<RemoveReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    if let Some(info) = engine::inspect(&request.home, &id)? {
        if info.state == SessionState::Running && !request.force {
            bail!("session {id} is running; stop it first or pass --force");
        }
        if info.state == SessionState::Running {
            engine::stop(engine::StopRequest {
                home: &request.home,
                id: &id,
                force: true,
            })?;
        }
        engine::kill_session(&request.home, &id)?;
    }
    request.home.session(&id).remove()?;
    Ok(RemoveReport {
        id: id.to_string(),
        removed: true,
    })
}

/// Rename request.
#[derive(Debug, Clone)]
pub struct RenameRequest {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// New display name.
    pub name: String,
}

/// Renames only the metadata sidecar; tmux ids remain opaque.
pub fn rename(request: RenameRequest) -> anyhow::Result<RenameReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    let paths = request.home.session(&id);
    let mut metadata = meta::read(&paths)?;
    let name = meta::sanitize_display(&request.name);
    if name.is_empty() {
        bail!("session name must contain at least one printable character");
    }
    if name != metadata.name {
        if let Some(other) = attach::session_ids_named(&request.home, &name)?
            .into_iter()
            .find(|candidate| candidate != &id)
        {
            bail!("session name `{name}` is already used by {other}; choose a different name");
        }
    }
    metadata.name = name.clone();
    meta::update(&paths, &metadata)?;
    Ok(RenameReport {
        id: id.to_string(),
        name,
    })
}

/// Resize request.
#[derive(Debug, Clone)]
pub struct ResizeRequest {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or name.
    pub session: String,
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Switch tmux's window-size policy to manual.
    pub pin: bool,
}

/// Resizes a tmux window.
pub fn resize(request: ResizeRequest) -> anyhow::Result<ResizeReport> {
    let id = resolve_existing(&request.home, &request.session)?;
    if !engine::has_session(&request.home, &id) {
        bail!("session {id} is not running; cannot resize");
    }
    engine::resize(engine::ResizeRequest {
        home: &request.home,
        id: &id,
        size: TerminalSize::new(request.cols, request.rows),
        pin: request.pin,
    })?;
    Ok(ResizeReport {
        id: id.to_string(),
        cols: request.cols,
        rows: request.rows,
        pinned: request.pin,
    })
}

/// Prune options.
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Report without deleting.
    pub dry_run: bool,
    /// Dead-pane retention.
    pub retain_exited: Duration,
}

impl PruneOptions {
    /// Ordinary prune options.
    pub fn new(home: LatchHome) -> Self {
        Self {
            home,
            dry_run: false,
            retain_exited: DEFAULT_EXITED_RETENTION,
        }
    }
}

/// Reclaims lost metadata and dead panes older than the retention window.
pub fn prune(options: PruneOptions) -> anyhow::Result<PruneReport> {
    let live = engine::list(&options.home)?
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<HashMap<_, _>>();
    let now = SystemTime::now();
    let mut reclaimed = Vec::new();
    let mut retained = Vec::new();
    for id in session_ids_or_empty(&options.home)? {
        let info = live.get(id.as_str());
        let age = info
            .map(|value| now.duration_since(value.activity).unwrap_or_default())
            .unwrap_or(options.retain_exited);
        if info.is_some_and(|value| value.state == SessionState::Running) {
            continue;
        }
        if info.is_some() && !options.retain_exited.is_zero() && age < options.retain_exited {
            retained.push(RetainedSession {
                id: id.to_string(),
                reclaimable_in_ms: (options.retain_exited - age).as_millis() as u64,
            });
            continue;
        }
        reclaimed.push(id.to_string());
        if !options.dry_run {
            if info.is_some() {
                engine::kill_session(&options.home, &id)?;
            }
            options.home.session(&id).remove()?;
        }
    }
    reclaimed.sort();
    retained.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(PruneReport {
        reclaimed,
        retained,
        dry_run: options.dry_run,
    })
}

/// Doctor options.
#[derive(Debug, Clone)]
pub struct DoctorOptions {
    /// Latch state root.
    pub home: LatchHome,
}

/// Reports installation and state problems.
pub fn doctor(options: DoctorOptions) -> anyhow::Result<DoctorReport> {
    let mut findings = Vec::new();
    let tmux_version = match engine::tmux_version() {
        Ok(version) => {
            if version != format!("tmux {}", engine::TMUX_VERSION) {
                findings.push(DoctorFinding {
                    code: "tmux_version".to_owned(),
                    severity: "error".to_owned(),
                    message: format!(
                        "bundled tmux is {version}; expected tmux {}",
                        engine::TMUX_VERSION
                    ),
                });
            }
            Some(version)
        }
        Err(error) => {
            findings.push(DoctorFinding {
                code: "tmux_missing".to_owned(),
                severity: "error".to_owned(),
                message: error.to_string(),
            });
            None
        }
    };
    let latchd_version = match engine::latchd_version() {
        Ok(version) => {
            let expected = format!(
                "latchd {} protocol {}",
                env!("CARGO_PKG_VERSION"),
                latchd::protocol::PROTOCOL_VERSION
            );
            if version != expected {
                findings.push(DoctorFinding {
                    code: "latchd_version".to_owned(),
                    severity: "error".to_owned(),
                    message: format!("bundled latchd is {version}; expected {expected}"),
                });
            }
            Some(version)
        }
        Err(error) => {
            findings.push(DoctorFinding {
                code: "latchd_missing".to_owned(),
                severity: "error".to_owned(),
                message: error.to_string(),
            });
            None
        }
    };
    doctor_common(options, findings, tmux_version, latchd_version)
}

fn doctor_common(
    options: DoctorOptions,
    mut findings: Vec<DoctorFinding>,
    tmux_version: Option<String>,
    latchd_version: Option<String>,
) -> anyhow::Result<DoctorReport> {
    if let Ok(executable) = std::env::current_exe().and_then(fs::canonicalize) {
        if let Some(parent) = executable.parent() {
            let remote = parent.join(engine::BUNDLED_REMOTE_NAME);
            if !remote.is_file() {
                findings.push(DoctorFinding {
                    code: "remote_helper_missing".to_owned(),
                    severity: "error".to_owned(),
                    message: format!(
                        "bundled remote-access helper {} is missing; run `latch update` to repair the complete payload",
                        remote.display()
                    ),
                });
            }
        }
    }
    let root = options.home.root();
    if !root.exists() {
        findings.push(DoctorFinding {
            code: "home_missing".to_owned(),
            severity: "warning".to_owned(),
            message: format!("Latch home {} does not exist yet", root.display()),
        });
        return Ok(DoctorReport {
            kernel: engine::kernel().as_str().to_owned(),
            tmux_version,
            latchd_version,
            findings,
        });
    }
    push_mode_finding(ModeCheck {
        findings: &mut findings,
        path: root,
        expected: DIR_MODE,
        code: "home_permissions",
    });
    let sessions = options.home.sessions_dir();
    if sessions.exists() {
        push_mode_finding(ModeCheck {
            findings: &mut findings,
            path: &sessions,
            expected: DIR_MODE,
            code: "sessions_permissions",
        });
    }
    let live = engine::list(&options.home)
        .unwrap_or_default()
        .into_iter()
        .map(|session| session.id)
        .collect::<std::collections::HashSet<_>>();
    let lost = session_ids_or_empty(&options.home)?
        .into_iter()
        .filter(|id| !live.contains(id.as_str()))
        .count();
    if lost > 0 {
        findings.push(DoctorFinding {
            code: "lost_sessions".to_owned(),
            severity: "warning".to_owned(),
            message: format!("{lost} session(s) are absent from the session kernel"),
        });
    }
    Ok(DoctorReport {
        kernel: engine::kernel().as_str().to_owned(),
        tmux_version,
        latchd_version,
        findings,
    })
}

/// Config request.
#[derive(Debug, Clone)]
pub struct ConfigRequest {
    /// Latch state root.
    pub home: LatchHome,
    /// Optional key.
    pub key: Option<String>,
    /// Optional value.
    pub value: Option<String>,
}

/// Reads or writes non-secret preferences.
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
        (Some(key), None) => Ok(ConfigReport {
            key: Some(key.clone()),
            value: values.get(key).cloned(),
            config: None,
        }),
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

/// Reads one non-secret preference without creating the state root.
///
/// Callers use this on paths where a missing config simply means "no stored
/// preference", so an absent file is `Ok(None)` rather than an error.
pub fn config_value(home: &LatchHome, key: &str) -> anyhow::Result<Option<String>> {
    Ok(read_config(&home.config_file())?.remove(key))
}

/// Reports the unchanged engine discovery contract.
pub fn capabilities() -> CapabilitiesReport {
    CapabilitiesReport {
        protocol_version: PROTOCOL_VERSION,
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: CapabilityFlags {
            create: true,
            open_viewer: cfg!(target_os = "macos"),
            local_attach: true,
            cloud_attach: false,
            self_update: crate::cli::update::can_self_update(),
            extensions: Vec::new(),
        },
    }
}

/// Resolves an existing metadata session.
pub fn resolve_existing(home: &LatchHome, session: &str) -> anyhow::Result<SessionId> {
    let id = attach::resolve_session(home, session)?;
    if !home.session(&id).meta().is_file() {
        bail!(attach::SessionLookupError::NotFound {
            session: session.to_owned(),
        });
    }
    Ok(id)
}

fn session_ids_or_empty(home: &LatchHome) -> anyhow::Result<Vec<SessionId>> {
    match home.session_ids() {
        Ok(ids) => Ok(ids),
        Err(crate::session::paths::PathsError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

struct ModeCheck<'a> {
    findings: &'a mut Vec<DoctorFinding>,
    path: &'a Path,
    expected: u32,
    code: &'a str,
}

fn push_mode_finding(request: ModeCheck<'_>) {
    let Ok(metadata) = fs::symlink_metadata(request.path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode != request.expected {
        request.findings.push(DoctorFinding {
            code: request.code.to_owned(),
            severity: "error".to_owned(),
            message: format!(
                "{} has mode {:04o}; expected {:04o}",
                request.path.display(),
                mode,
                request.expected
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
        values.insert(key.trim().to_owned(), unquote(value.trim()));
    }
    Ok(values)
}

fn write_config(path: &Path, values: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let body = values
        .iter()
        .map(|(key, value)| {
            format!(
                "{key} = \"{}\"\n",
                value.replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .collect::<String>();
    let parent = path.parent().context("config path has no parent")?;
    let temp = parent.join(format!(".config.toml.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&temp)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn unquote(raw: &str) -> String {
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        raw[1..raw.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        raw.to_owned()
    }
}
