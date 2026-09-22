//! Session creation on the private latchd kernel.
//!
//! Bare `latch`, `latch shell`, and `latch run -- <cmd>` all land here. So does
//! `latch create --manifest-file -`, which is the path M3's Overlord provider
//! will use and which exists at M1 so launch secrets never travel in argv.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::cli::nesting::{self, NestingDecision, SESSION_ID_ENV};
use crate::cli::serve::SessionAgent;
use crate::engine::{self, CreateRequest};
use crate::session::manifest::{
    AgentKind, DisplayMetadata, LaunchManifest, LaunchRequest, LoginShell, TerminalSize,
};
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId, SessionPaths, FILE_MODE};
use anyhow::{bail, Context};

/// Provenance recorded for a session the phone asked for.
pub const MOBILE_SOURCE_KIND: &str = "mobile";

/// Geometry an unattended remote-created shell starts in.
///
/// The same 80x24 a desktop shell falls back to when no terminal reports a
/// size, so a pane created from the phone begins in a known geometry rather
/// than one derived from whatever the gateway process happens to be attached
/// to.
pub const REMOTE_INITIAL_SIZE: TerminalSize = TerminalSize::new(80, 24);

/// How a session is being asked for.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Where sessions live for this invocation.
    pub home: LatchHome,
    /// What to run. Arrives over stdin for `--manifest-file`; built in-process
    /// for `shell` / `run`.
    pub manifest: LaunchManifest,
    /// Whether the invoking terminal should attach with control after spawn.
    ///
    /// `latch` / `shell` / `run` attach. `latch create --json` does not: the
    /// caller (Overlord) wants the session id and will open a viewer separately.
    pub attach: bool,
}

/// What creation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    /// The new session's identifier.
    pub id: SessionId,
    /// Where its files are.
    pub paths: SessionPaths,
    /// The name that was stored — either supplied or auto-derived.
    pub name: String,
}

/// Shared launch details for shell and explicit command sessions.
#[derive(Debug, Clone)]
pub struct ManifestOptions {
    /// The child's initial working directory.
    pub cwd: PathBuf,
    /// The child's initial terminal size before a client attaches.
    pub size: TerminalSize,
    /// Human-display fields supplied at the CLI boundary.
    pub display: DisplayMetadata,
}

/// Creates a session: id, directory at `0700`, `meta.json` via temp+rename,
/// detached latchd session, optional attach.
///
/// Display fields on the manifest are sanitized at this boundary — the single
/// place externally supplied names and titles arrive — before anything is
/// written or shown.
pub fn create_session(options: CreateOptions) -> anyhow::Result<CreateOutcome> {
    match nesting::nesting_decision(enclosing_session_id().as_deref()) {
        NestingDecision::Allow => {}
        NestingDecision::AttachToEnclosing { session_id } => {
            bail!(
                "refusing to create a nested Latch session (already inside {session_id}); \
                 run `latch attach` or exit the enclosing session first"
            );
        }
    }

    create_session_detached(options)
}

/// Creates a session for a caller that has no enclosing terminal.
///
/// The nesting refusal in [`create_session`] protects a person who typed
/// `latch` inside a Latch pane. The gateway is not in a pane, so the same
/// refusal there would only make remote creation fail whenever `latch serve`
/// happened to be started from inside a session.
pub fn create_session_detached(options: CreateOptions) -> anyhow::Result<CreateOutcome> {
    options.home.ensure()?;
    let mut manifest = options.manifest;
    sanitize_display_metadata(&mut manifest.display);
    let result = engine::create(CreateRequest {
        home: options.home,
        manifest,
    })?;
    Ok(CreateOutcome {
        id: result.id,
        paths: result.paths,
        name: result.meta.name,
    })
}

/// One phone request to start a standard shell or a hosted agent.
///
/// There is deliberately no argv, environment, shell, display metadata, or
/// terminal type here: the only caller-controlled inputs are the correlation
/// id, where the session starts, and which agent kind — if any — the Mac
/// launches there. The Mac resolves the agent's executable itself.
#[derive(Debug, Clone)]
pub struct RemoteSessionRequest {
    /// Where sessions live for this gateway.
    pub home: LatchHome,
    /// Opaque per-attempt correlation id, retained across the phone's retries.
    pub request_id: String,
    /// Absolute directory the session starts in. Canonicalized by the caller
    /// and re-canonicalized here immediately before creation.
    pub cwd: PathBuf,
    /// Hosted agent to launch directly, or `None` for a standard login shell.
    pub agent: Option<SessionAgent>,
    /// Opaque device that submitted the request, when it came through the
    /// paired proxy. A request id is owned by the device that first used it.
    pub device: Option<String>,
}

/// Why an idempotent remote creation could not produce a session.
#[derive(Debug)]
pub enum RemoteSessionError {
    /// The request id already named a session started somewhere else, or
    /// with a different agent.
    RequestIdConflict,
    /// The request id was first used by a different device.
    DeviceConflict,
    /// The requested agent is not installed where this Mac can find it.
    AgentUnavailable(SessionAgent),
    /// Creation itself failed.
    Failed(anyhow::Error),
}

/// Durable acceptance for one request id, written before the launch is
/// dispatched. A retry after a crash reads it back: the device and directory
/// must match, and the actual session (metadata under the creation lock, then
/// the daemon) remains the authority on whether anything started.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreationReceipt {
    /// The phone's idempotency key.
    pub request_id: String,
    /// Opaque device that owns the key, when proxied.
    pub device: Option<String>,
    /// Canonical directory the request named.
    pub cwd: PathBuf,
    /// Harness marker of the agent the request named; `None` for a shell.
    /// Absent on receipts written before agents could be requested, which
    /// were all shells.
    #[serde(default)]
    pub agent: Option<String>,
    /// Where the launch got to.
    pub status: CreationReceiptStatus,
    /// The session it produced, once it did.
    pub session_id: Option<String>,
    /// RFC 3339 acceptance time.
    pub accepted_at: String,
}

/// Where a durably accepted creation got to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CreationReceiptStatus {
    /// Accepted durably; the launch may or may not have run.
    Accepted,
    /// The launch produced a session.
    Created,
    /// The launch failed before creating anything; the id may be retried.
    Failed,
}

/// Receipts retained before the oldest are evicted. Older than this a phone
/// has long since given up automatic retry of the id.
const MAX_CREATION_RECEIPTS: usize = 512;

fn receipts_dir(home: &LatchHome) -> PathBuf {
    home.root().join("remote-shell-receipts")
}

fn receipt_path(home: &LatchHome, request_id: &str) -> PathBuf {
    receipts_dir(home).join(format!("{request_id}.json"))
}

/// Reads the receipt for a request id, if one was ever accepted.
pub fn read_receipt(home: &LatchHome, request_id: &str) -> anyhow::Result<Option<CreationReceipt>> {
    let path = receipt_path(home, request_id);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    Ok(serde_json::from_slice(&bytes).ok())
}

fn write_receipt(home: &LatchHome, receipt: &CreationReceipt) -> anyhow::Result<()> {
    let dir = receipts_dir(home);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(
        &dir,
        std::fs::Permissions::from_mode(crate::session::paths::DIR_MODE),
    )?;
    let path = receipt_path(home, &receipt.request_id);
    let temporary = dir.join(format!(".{}.tmp", receipt.request_id));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(receipt)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    prune_receipts(&dir);
    Ok(())
}

fn prune_receipts(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if files.len() <= MAX_CREATION_RECEIPTS {
        return;
    }
    files.sort();
    for (_, path) in files.iter().take(files.len() - MAX_CREATION_RECEIPTS) {
        let _ = std::fs::remove_file(path);
    }
}

/// What an idempotent remote creation resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSessionOutcome {
    /// Session identifier.
    pub id: String,
    /// Stored display name.
    pub name: String,
    /// RFC 3339 creation time read back from durable metadata.
    pub created_at: String,
    /// True when an earlier request with this id had already created it.
    pub reused: bool,
}

/// Creates exactly one session for `request` — a standard login shell, or the
/// named agent launched directly — or returns the one an earlier attempt with
/// the same request id already created.
///
/// Idempotency lives here, at the create boundary, because only this machine
/// can know whether a process actually started: a phone that loses the
/// response cannot tell a failed launch from a lost reply.
pub fn create_remote_session(
    request: RemoteSessionRequest,
) -> Result<RemoteSessionOutcome, RemoteSessionError> {
    create_remote_session_with(request, resolve_agent_program, create_session_detached)
}

fn create_remote_session_with(
    request: RemoteSessionRequest,
    resolve: impl FnOnce(SessionAgent) -> Option<PathBuf>,
    launch: impl FnOnce(CreateOptions) -> anyhow::Result<CreateOutcome>,
) -> Result<RemoteSessionOutcome, RemoteSessionError> {
    request.home.ensure().map_err(failed)?;
    // Held from the lookup through durable metadata creation, so two
    // concurrent requests carrying one request id — or a retry after a lost
    // response — cannot both pass the scan and each start a session.
    let _lock = CreationLock::acquire(&request.home).map_err(RemoteSessionError::Failed)?;

    // Canonicalized again under the lock: the directory the phone browsed may
    // have been replaced between validation and creation.
    let cwd = request
        .cwd
        .canonicalize()
        .map_err(|error| RemoteSessionError::Failed(error.into()))?;
    if !cwd.is_dir() {
        return Err(RemoteSessionError::Failed(anyhow::anyhow!(
            "session working directory is not a directory"
        )));
    }
    let harness = request.agent.map(|agent| agent.harness().to_owned());

    // The receipt is the device- and payload-scoped owner of this id. Only
    // the device that first used the id may reuse it, and only for the same
    // directory and agent; anything else is a conflict, never a second launch.
    let receipt = read_receipt(&request.home, &request.request_id).map_err(failed)?;
    if let Some(receipt) = &receipt {
        if receipt.device != request.device {
            return Err(RemoteSessionError::DeviceConflict);
        }
        if receipt.cwd != cwd || receipt.agent != harness {
            return Err(RemoteSessionError::RequestIdConflict);
        }
    }

    if let Some(existing) =
        find_remote_session(&request.home, &request.request_id).map_err(failed)?
    {
        if existing.cwd != cwd || existing.harness != harness {
            return Err(RemoteSessionError::RequestIdConflict);
        }
        return Ok(RemoteSessionOutcome {
            id: existing.id,
            name: existing.name,
            created_at: existing.created_at,
            reused: true,
        });
    }

    // An agent that cannot be found is answered before anything is accepted:
    // there is nothing to retry until it is installed, and no receipt should
    // suggest a launch was ever attempted.
    let program = match request.agent {
        Some(agent) => Some(resolve(agent).ok_or(RemoteSessionError::AgentUnavailable(agent))?),
        None => None,
    };

    // Durable acceptance precedes dispatch. Metadata is written before the
    // daemon is spawned, so "accepted but no session" after a crash means
    // nothing ran and the retry is safe; "accepted and a session exists" is
    // answered by the lookup above.
    let mut receipt = CreationReceipt {
        request_id: request.request_id.clone(),
        device: request.device.clone(),
        cwd: cwd.clone(),
        agent: harness,
        status: CreationReceiptStatus::Accepted,
        session_id: None,
        accepted_at: crate::engine::format_rfc3339(std::time::SystemTime::now()),
    };
    write_receipt(&request.home, &receipt).map_err(failed)?;

    let options = ManifestOptions {
        cwd,
        size: REMOTE_INITIAL_SIZE,
        display: DisplayMetadata {
            source: crate::session::manifest::SourceInfo {
                kind: MOBILE_SOURCE_KIND.to_owned(),
                external_run_id: Some(request.request_id),
            },
            ..DisplayMetadata::default()
        },
    };
    // Declare the real agent argv before the engine prepares its observer and
    // wraps it in a login shell. This is the same path Desktop uses, preserving
    // agent identity while loading the owner's terminal environment.
    let manifest = match program {
        Some(program) => {
            let mut manifest = run_manifest(vec![program.display().to_string()], options);
            manifest.launch.agent = request.agent.map(|agent| match agent {
                SessionAgent::Claude => AgentKind::Claude,
                SessionAgent::Codex => AgentKind::Codex,
            });
            manifest.launch.login_shell = Some(LoginShell {
                path: login_shell_path(),
                prelude: None,
            });
            manifest
        }
        None => shell_manifest(options),
    };
    let launched = launch(CreateOptions {
        home: request.home.clone(),
        manifest,
        // A remote creation starts the process and nothing else: no attach
        // client is spawned, and no existing session's surface is touched.
        attach: false,
    });
    let outcome = match launched {
        Ok(outcome) => outcome,
        Err(error) => {
            receipt.status = CreationReceiptStatus::Failed;
            let _ = write_receipt(&request.home, &receipt);
            return Err(RemoteSessionError::Failed(error));
        }
    };
    receipt.status = CreationReceiptStatus::Created;
    receipt.session_id = Some(outcome.id.to_string());
    let _ = write_receipt(&request.home, &receipt);
    let meta = meta::read(&outcome.paths).map_err(failed)?;
    Ok(RemoteSessionOutcome {
        id: outcome.id.to_string(),
        name: outcome.name,
        created_at: meta.created_at,
        reused: false,
    })
}

fn failed(error: impl Into<anyhow::Error>) -> RemoteSessionError {
    RemoteSessionError::Failed(error.into())
}

/// How long the login shell gets to answer where an agent lives. A shell whose
/// startup files hang must not hold the creation lock indefinitely.
const AGENT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Finds the executable for `agent` the way the Mac's owner would reach it
/// from a terminal.
///
/// The gateway runs under Latch Desktop's environment, which has none of the
/// PATH a person's `.zshrc` adds — and that is where `claude` lives when it
/// was installed by npm under nvm, by its own installer under `~/.local/bin`,
/// or as the `~/.claude/local` alias. So the login shell is asked first, the
/// same way Desktop finds `latch`; the process PATH and the installer's known
/// locations are the fallbacks for a shell whose startup files fail.
pub fn resolve_agent_program(agent: SessionAgent) -> Option<PathBuf> {
    let name = agent.harness();
    let shell_answer = login_shell_lookup(name);
    find_agent_program(
        name,
        shell_answer.as_deref(),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
}

/// Chooses the executable from what the login shell said, then the process
/// PATH, then the agent's own installer locations. Pure, so the order can be
/// tested without a shell.
fn find_agent_program(
    name: &str,
    shell_answer: Option<&str>,
    path: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    // `command -v` answers with a path for a binary, and with the alias text
    // for an alias; only an absolute path to something runnable counts.
    if let Some(answer) = shell_answer {
        let candidate = Path::new(answer.trim());
        if candidate.is_absolute() && is_executable_file(candidate) {
            return Some(candidate.to_owned());
        }
    }
    if let Some(path) = path {
        for directory in std::env::split_paths(path) {
            let candidate = directory.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    let home = home?;
    [
        home.join(".claude/local").join(name),
        home.join(".local/bin").join(name),
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
    ]
    .into_iter()
    .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Asks the owner's interactive login shell where `name` is, bounded by
/// [`AGENT_LOOKUP_TIMEOUT`]. `None` for a shell that fails, hangs, or does not
/// know the name.
fn login_shell_lookup(name: &str) -> Option<String> {
    let shell = login_shell_path();
    // Interactive and login, for the same reason the remote shell itself is:
    // `.zshrc` is where PATH additions usually live, and it only loads for an
    // interactive shell. The name is an argument, never interpolated.
    let mut child = Command::new(shell)
        .args(["-ilc", "command -v -- \"$1\"", "latch", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        output
    });
    let deadline = Instant::now() + AGENT_LOOKUP_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let output = reader.join().ok()?;
    if !status?.success() {
        return None;
    }
    Some(last_plain_word(&output))
}

fn login_shell_path() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|value| Path::new(value).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

/// The answer a shell printed, with startup-file noise removed.
///
/// Startup files echo lines, and terminal integrations emit OSC sequences
/// (`ESC ] ... BEL`) on stdout even without a terminal, sometimes on the same
/// line as the answer. A path contains no control characters, so the answer
/// is the last run of text between control characters.
fn last_plain_word(output: &str) -> String {
    output
        .split(|character: char| character.is_control())
        .map(str::trim)
        .rfind(|segment| !segment.is_empty())
        .unwrap_or_default()
        .to_owned()
}

struct RemoteSession {
    id: String,
    name: String,
    created_at: String,
    cwd: PathBuf,
    harness: Option<String>,
}

/// Finds the session an earlier request with this id created, if any.
fn find_remote_session(
    home: &LatchHome,
    request_id: &str,
) -> anyhow::Result<Option<RemoteSession>> {
    for id in home.session_ids()? {
        let paths = home.session(&id);
        // A session directory without readable metadata is either mid-creation
        // under someone else's lock or damaged; neither answers this question.
        let Ok(meta) = meta::read(&paths) else {
            continue;
        };
        if meta.source.kind != MOBILE_SOURCE_KIND
            || meta.source.external_run_id.as_deref() != Some(request_id)
        {
            continue;
        }
        // The recorded cwd is canonical because creation canonicalized it.
        return Ok(Some(RemoteSession {
            id: meta.id,
            name: meta.name,
            created_at: meta.created_at,
            cwd: meta.cwd,
            harness: meta.harness,
        }));
    }
    Ok(None)
}

/// Advisory exclusive lock serializing remote session creation for one Latch
/// home.
struct CreationLock {
    file: File,
}

impl CreationLock {
    fn acquire(home: &LatchHome) -> anyhow::Result<Self> {
        let path = home.root().join("create.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&path)
            .with_context(|| format!("cannot open the creation lock {}", path.display()))?;
        // SAFETY: `file` outlives the lock; the guard unlocks it on drop.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot lock {}", path.display()));
        }
        Ok(Self { file })
    }
}

impl Drop for CreationLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is still open here.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Builds the launch material for a shell session.
pub fn shell_manifest(options: ManifestOptions) -> LaunchManifest {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    // iTerm's ordinary profile starts a login *interactive* shell, so nvm,
    // aliases, and other `.zshrc` setup are in PATH. `zsh -l` on a TTY is
    // interactive; pass `-il` so the same files load even if TTY detection
    // fails (and so Overlord's `$SHELL -lc` wrapper is promoted to `-ilc`).
    let mut manifest = LaunchManifest::new(LaunchRequest {
        argv: vec![shell, "-il".to_owned()],
        cwd: options.cwd,
        size: options.size,
    });
    manifest.display = options.display;
    manifest
}

/// Builds the launch material for `latch run -- <argv>`.
pub fn run_manifest(argv: Vec<String>, options: ManifestOptions) -> LaunchManifest {
    let mut manifest = LaunchManifest::new(LaunchRequest {
        argv,
        cwd: options.cwd,
        size: options.size,
    });
    manifest.display = options.display;
    manifest
}

/// Reads a launch manifest from a path, or from stdin when `path` is `-`.
///
/// Launch material never appears in argv after this call. The only part that
/// may be stored is a redacted command label derived from the manifest's
/// display metadata.
pub fn read_manifest_file(path: &str) -> anyhow::Result<LaunchManifest> {
    if path == "-" {
        return crate::session::manifest::read(io::stdin()).map_err(Into::into);
    }
    let file = File::open(path).with_context(|| format!("cannot open launch manifest {path}"))?;
    crate::session::manifest::read(file).map_err(Into::into)
}

fn sanitize_display_metadata(display: &mut DisplayMetadata) {
    display.name = sanitize_option(display.name.take());
    display.title = sanitize_option(display.title.take());
    display.command_label = sanitize_option(display.command_label.take());
    display.source.kind = meta::sanitize_display(&display.source.kind);
    display.source.external_run_id = sanitize_option(display.source.external_run_id.take());
}

fn sanitize_option(value: Option<String>) -> Option<String> {
    value
        .map(|value| meta::sanitize_display(&value))
        .filter(|value| !value.is_empty())
}

/// Reads [`SESSION_ID_ENV`] from this process, if set to a non-empty value.
pub fn enclosing_session_id() -> Option<String> {
    std::env::var(SESSION_ID_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::session::meta::MetaRequest;

    const REQUEST_ID: &str = "8cba5d78-79a0-4a55-9047-f77e57e463c7";

    /// Stands in for the kernel launch: it writes the same durable metadata
    /// `engine::create` writes, which is what idempotency reads back, without
    /// needing a real daemon or PTY.
    fn stub_launch(options: CreateOptions) -> anyhow::Result<CreateOutcome> {
        let id = SessionId::generate();
        let paths = options.home.session(&id);
        paths.ensure()?;
        let meta = meta::derive(MetaRequest {
            id: id.as_str(),
            launch: &options.manifest.launch,
            display: &options.manifest.display,
            created_at: "2026-09-05T00:00:00Z",
        });
        meta::write_once(&paths, &meta)?;
        Ok(CreateOutcome {
            id,
            paths,
            name: meta.name,
        })
    }

    fn home() -> (tempfile::TempDir, LatchHome, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = LatchHome::new(temp.path().join("latch"));
        let cwd = temp.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        (temp, home, cwd)
    }

    fn request(home: &LatchHome, cwd: &std::path::Path) -> RemoteSessionRequest {
        RemoteSessionRequest {
            home: home.clone(),
            request_id: REQUEST_ID.to_owned(),
            cwd: cwd.to_owned(),
            agent: None,
            device: Some("phone-a".to_owned()),
        }
    }

    fn claude_request(home: &LatchHome, cwd: &std::path::Path) -> RemoteSessionRequest {
        RemoteSessionRequest {
            agent: Some(SessionAgent::Claude),
            ..request(home, cwd)
        }
    }

    fn codex_request(home: &LatchHome, cwd: &std::path::Path) -> RemoteSessionRequest {
        RemoteSessionRequest {
            agent: Some(SessionAgent::Codex),
            ..request(home, cwd)
        }
    }

    /// No agent is ever resolved for a shell request; a resolver that panics
    /// proves it is not consulted.
    fn no_agent(agent: SessionAgent) -> Option<PathBuf> {
        panic!("a shell request must not resolve {agent:?}")
    }

    fn fake_claude(_: SessionAgent) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/fake/bin/claude"))
    }

    fn fake_codex(_: SessionAgent) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/fake/bin/codex"))
    }

    fn create_remote_shell_with(
        request: RemoteSessionRequest,
        launch: impl FnOnce(CreateOptions) -> anyhow::Result<CreateOutcome>,
    ) -> Result<RemoteSessionOutcome, RemoteSessionError> {
        create_remote_session_with(request, no_agent, launch)
    }

    #[test]
    fn a_remote_request_launches_one_standard_login_shell_without_attaching() {
        let (_temp, home, cwd) = home();
        let seen = Arc::new(Mutex::new(None));
        let recorder = seen.clone();
        let outcome = create_remote_shell_with(request(&home, &cwd), move |options| {
            *recorder.lock().unwrap() = Some((
                options.attach,
                options.manifest.launch.clone(),
                options.manifest.display.clone(),
            ));
            stub_launch(options)
        })
        .unwrap();

        assert!(!outcome.reused);
        let (attach, launch, display) = seen.lock().unwrap().clone().unwrap();
        // A creation is not an attach: the phone gets a session it can open
        // later, and nothing takes any session's exclusive surface here.
        assert!(!attach);
        assert_eq!(launch.cwd, cwd.canonicalize().unwrap());
        assert_eq!(launch.size, REMOTE_INITIAL_SIZE);
        assert_eq!(launch.argv.len(), 2);
        assert_eq!(launch.argv[1], "-il");
        assert_eq!(display.source.kind, MOBILE_SOURCE_KIND);
        assert_eq!(display.source.external_run_id.as_deref(), Some(REQUEST_ID));
        // Nothing else about the session is caller-controlled.
        assert_eq!(display.name, None);
        assert_eq!(display.title, None);
        assert_eq!(display.command_label, None);
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn a_retry_after_a_lost_response_returns_the_first_session() {
        let (_temp, home, cwd) = home();
        // The response to this one never reaches the phone.
        let first = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();

        let launches = Arc::new(AtomicUsize::new(0));
        let counter = launches.clone();
        let retry = create_remote_shell_with(request(&home, &cwd), move |options| {
            counter.fetch_add(1, Ordering::SeqCst);
            stub_launch(options)
        })
        .unwrap();

        assert_eq!(launches.load(Ordering::SeqCst), 0);
        assert!(retry.reused);
        assert_eq!(retry.id, first.id);
        assert_eq!(retry.name, first.name);
        assert_eq!(retry.created_at, first.created_at);
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn reusing_a_request_id_for_another_directory_is_a_conflict() {
        let (temp, home, cwd) = home();
        create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();

        let conflict = create_remote_shell_with(request(&home, &elsewhere), |_| {
            panic!("a conflicting request id must never launch a session")
        });
        assert!(matches!(
            conflict,
            Err(RemoteSessionError::RequestIdConflict)
        ));
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_duplicate_requests_create_exactly_one_session() {
        let (_temp, home, cwd) = home();
        home.ensure().unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let outcomes = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    let home = home.clone();
                    let cwd = cwd.clone();
                    let counter = launches.clone();
                    scope.spawn(move || {
                        create_remote_shell_with(request(&home, &cwd), move |options| {
                            counter.fetch_add(1, Ordering::SeqCst);
                            stub_launch(options)
                        })
                        .unwrap()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(home.session_ids().unwrap().len(), 1);
        let id = &outcomes[0].id;
        assert!(outcomes.iter().all(|outcome| &outcome.id == id));
        assert_eq!(outcomes.iter().filter(|outcome| !outcome.reused).count(), 1);
    }

    #[test]
    fn a_failed_launch_leaves_no_session_and_a_retry_can_still_succeed() {
        let (_temp, home, cwd) = home();
        let failure = create_remote_shell_with(request(&home, &cwd), |_| {
            anyhow::bail!("kernel is unavailable")
        });
        assert!(matches!(failure, Err(RemoteSessionError::Failed(_))));
        assert!(home.session_ids().unwrap().is_empty());

        let retry = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        assert!(!retry.reused);
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn a_directory_that_disappears_before_creation_is_a_failure_not_a_launch() {
        let (_temp, home, cwd) = home();
        std::fs::remove_dir(&cwd).unwrap();
        let result = create_remote_shell_with(request(&home, &cwd), |_| {
            panic!("an unavailable directory must never launch a session")
        });
        assert!(matches!(result, Err(RemoteSessionError::Failed(_))));
    }

    #[test]
    fn a_request_id_is_owned_by_the_device_that_first_used_it() {
        let (_temp, home, cwd) = home();
        let first = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        let receipt = read_receipt(&home, REQUEST_ID).unwrap().unwrap();
        assert_eq!(receipt.status, CreationReceiptStatus::Created);
        assert_eq!(receipt.session_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(receipt.device.as_deref(), Some("phone-a"));

        // The owner's retry is the earlier session, not a second shell.
        let again = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        assert!(again.reused);
        assert_eq!(again.id, first.id);

        // Another device presenting the same id learns only that the id is
        // foreign; nothing is created and the original session is not named.
        let mut foreign = request(&home, &cwd);
        foreign.device = Some("phone-b".to_owned());
        assert!(matches!(
            create_remote_shell_with(foreign, stub_launch),
            Err(RemoteSessionError::DeviceConflict)
        ));
        let mut local = request(&home, &cwd);
        local.device = None;
        assert!(matches!(
            create_remote_shell_with(local, stub_launch),
            Err(RemoteSessionError::DeviceConflict)
        ));
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn acceptance_is_durable_before_dispatch_and_a_failed_launch_is_retryable() {
        let (_temp, home, cwd) = home();
        let seen_receipt = Arc::new(Mutex::new(None));
        let recorder = seen_receipt.clone();
        let failed_home = home.clone();
        let result = create_remote_shell_with(request(&home, &cwd), move |_| {
            // The launch sees its own acceptance already on disk.
            *recorder.lock().unwrap() = read_receipt(&failed_home, REQUEST_ID).unwrap();
            anyhow::bail!("launch failed")
        });
        assert!(matches!(result, Err(RemoteSessionError::Failed(_))));
        let during = seen_receipt.lock().unwrap().clone().unwrap();
        assert_eq!(during.status, CreationReceiptStatus::Accepted);
        assert_eq!(during.session_id, None);
        let after = read_receipt(&home, REQUEST_ID).unwrap().unwrap();
        assert_eq!(after.status, CreationReceiptStatus::Failed);
        assert!(home.session_ids().unwrap().is_empty());

        // Nothing ran, so the same id may try again and produce one session.
        let outcome = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        assert!(!outcome.reused);
        let receipt = read_receipt(&home, REQUEST_ID).unwrap().unwrap();
        assert_eq!(receipt.status, CreationReceiptStatus::Created);
        assert_eq!(receipt.session_id.as_deref(), Some(outcome.id.as_str()));
    }

    #[test]
    fn a_session_from_another_source_never_satisfies_a_request_id() {
        let (_temp, home, cwd) = home();
        // Same correlation id, different provenance: an Overlord run id must
        // not silently answer a phone's creation request.
        stub_launch(CreateOptions {
            home: home.clone(),
            manifest: shell_manifest(ManifestOptions {
                cwd: cwd.clone(),
                size: REMOTE_INITIAL_SIZE,
                display: DisplayMetadata {
                    source: crate::session::manifest::SourceInfo {
                        kind: "overlord".to_owned(),
                        external_run_id: Some(REQUEST_ID.to_owned()),
                    },
                    ..DisplayMetadata::default()
                },
            }),
            attach: false,
        })
        .unwrap();

        let outcome = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        assert!(!outcome.reused);
        assert_eq!(home.session_ids().unwrap().len(), 2);
    }

    #[test]
    fn a_claude_request_uses_the_structured_agent_launch() {
        let (_temp, home, cwd) = home();
        let seen = Arc::new(Mutex::new(None));
        let recorder = seen.clone();
        let outcome =
            create_remote_session_with(claude_request(&home, &cwd), fake_claude, move |options| {
                *recorder.lock().unwrap() = Some((
                    options.attach,
                    options.manifest.launch.clone(),
                    options.manifest.display.clone(),
                ));
                stub_launch(options)
            })
            .unwrap();

        assert!(!outcome.reused);
        let (attach, launch, display) = seen.lock().unwrap().clone().unwrap();
        assert!(!attach);
        // Identity and observer setup see the agent argv before the engine
        // wraps it in the owner's login shell.
        assert_eq!(launch.argv, vec!["/opt/fake/bin/claude".to_owned()]);
        assert_eq!(launch.agent, Some(AgentKind::Claude));
        assert_eq!(launch.login_shell.unwrap().path, login_shell_path());
        assert_eq!(
            crate::session::meta::harness_kind(&launch.argv),
            Some("claude")
        );
        assert_eq!(launch.cwd, cwd.canonicalize().unwrap());
        assert_eq!(launch.size, REMOTE_INITIAL_SIZE);
        assert_eq!(display.source.kind, MOBILE_SOURCE_KIND);
        assert_eq!(display.source.external_run_id.as_deref(), Some(REQUEST_ID));
        assert_eq!(display.name, None);
        assert_eq!(display.command_label, None);
        let receipt = read_receipt(&home, REQUEST_ID).unwrap().unwrap();
        assert_eq!(receipt.agent.as_deref(), Some("claude"));
        let meta = meta::read(&home.session(&SessionId::parse(&outcome.id).unwrap())).unwrap();
        assert_eq!(meta.harness.as_deref(), Some("claude"));
    }

    #[test]
    fn a_codex_request_records_identity_and_conflicts_with_claude_retry() {
        let (_temp, home, cwd) = home();
        let outcome =
            create_remote_session_with(codex_request(&home, &cwd), fake_codex, |options| {
                assert_eq!(options.manifest.launch.agent, Some(AgentKind::Codex));
                assert_eq!(options.manifest.launch.argv, ["/opt/fake/bin/codex"]);
                assert!(options.manifest.launch.login_shell.is_some());
                stub_launch(options)
            })
            .unwrap();
        let meta = meta::read(&home.session(&SessionId::parse(&outcome.id).unwrap())).unwrap();
        assert_eq!(meta.harness.as_deref(), Some("codex"));
        assert_eq!(
            read_receipt(&home, REQUEST_ID)
                .unwrap()
                .unwrap()
                .agent
                .as_deref(),
            Some("codex")
        );
        let retry = create_remote_session_with(codex_request(&home, &cwd), fake_codex, |_| {
            panic!("a retry must not launch another Codex session")
        })
        .unwrap();
        assert!(retry.reused);
        let conflict = create_remote_session_with(claude_request(&home, &cwd), fake_claude, |_| {
            panic!("a different agent must conflict")
        });
        assert!(matches!(
            conflict,
            Err(RemoteSessionError::RequestIdConflict)
        ));
    }

    #[test]
    fn a_missing_agent_is_refused_before_anything_is_accepted() {
        let (_temp, home, cwd) = home();
        let result = create_remote_session_with(
            claude_request(&home, &cwd),
            |_| None,
            |_| panic!("an unavailable agent must never launch"),
        );
        assert!(matches!(
            result,
            Err(RemoteSessionError::AgentUnavailable(SessionAgent::Claude))
        ));
        // Nothing to retry until it is installed, so no receipt claims a
        // launch was ever attempted under this id.
        assert!(read_receipt(&home, REQUEST_ID).unwrap().is_none());
        assert!(home.session_ids().unwrap().is_empty());

        // Once installed, the same id starts the session normally.
        let retry =
            create_remote_session_with(claude_request(&home, &cwd), fake_claude, stub_launch)
                .unwrap();
        assert!(!retry.reused);
    }

    #[test]
    fn a_claude_retry_reuses_the_first_session_and_a_shell_under_the_same_id_conflicts() {
        let (_temp, home, cwd) = home();
        let first =
            create_remote_session_with(claude_request(&home, &cwd), fake_claude, stub_launch)
                .unwrap();

        let retry = create_remote_session_with(claude_request(&home, &cwd), fake_claude, |_| {
            panic!("a retry must not launch a second agent")
        })
        .unwrap();
        assert!(retry.reused);
        assert_eq!(retry.id, first.id);

        // The id is bound to what it launched, not only where: the same id
        // asking for a shell instead would otherwise silently hand back the
        // Claude session as if it were one.
        let conflict = create_remote_shell_with(request(&home, &cwd), |_| {
            panic!("a conflicting request id must never launch a session")
        });
        assert!(matches!(
            conflict,
            Err(RemoteSessionError::RequestIdConflict)
        ));
        assert_eq!(home.session_ids().unwrap().len(), 1);
    }

    #[test]
    fn a_shell_created_before_agents_existed_still_answers_its_shell_retry() {
        let (_temp, home, cwd) = home();
        let first = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        // Rewrite the receipt the way a pre-agent build wrote it: no `agent`.
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt_path(&home, REQUEST_ID)).unwrap())
                .unwrap();
        value.as_object_mut().unwrap().remove("agent");
        std::fs::write(receipt_path(&home, REQUEST_ID), value.to_string()).unwrap();

        let retry = create_remote_shell_with(request(&home, &cwd), stub_launch).unwrap();
        assert!(retry.reused);
        assert_eq!(retry.id, first.id);

        let conflict = create_remote_session_with(claude_request(&home, &cwd), fake_claude, |_| {
            panic!("a conflicting request id must never launch a session")
        });
        assert!(matches!(
            conflict,
            Err(RemoteSessionError::RequestIdConflict)
        ));
    }

    #[test]
    fn a_shell_answer_survives_startup_echo_and_terminal_integration_noise() {
        assert_eq!(
            last_plain_word("/usr/local/bin/claude\n"),
            "/usr/local/bin/claude"
        );
        assert_eq!(
            last_plain_word("welcome back\n/Users/p/.local/bin/claude\n"),
            "/Users/p/.local/bin/claude"
        );
        // iTerm's shell integration writes OSC 1337 sequences terminated by
        // BEL, so the path shares a line with them.
        assert_eq!(
            last_plain_word(
                "\x1b]1337;RemoteHost=p@mac\x07\x1b]1337;CurrentDir=/Users/p\x07/Users/p/.local/bin/claude\n"
            ),
            "/Users/p/.local/bin/claude"
        );
        assert_eq!(last_plain_word("\n  \n"), "");
    }

    #[test]
    fn agent_lookup_prefers_the_shell_answer_then_path_then_installer_locations() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let home = temp.path().join("home");
        let shell_claude = temp.path().join("shell/claude");
        let path_claude = temp.path().join("path/claude");
        let local_claude = home.join(".local/bin/claude");
        executable(&shell_claude);
        executable(&path_claude);
        executable(&local_claude);
        let path_var = std::env::join_paths([temp.path().join("path")]).unwrap();

        // The shell's word wins when it names a real executable.
        assert_eq!(
            find_agent_program(
                "claude",
                Some(&format!("{}\n", shell_claude.display())),
                Some(&path_var),
                Some(&home)
            ),
            Some(shell_claude.clone())
        );
        // An alias answer is text, not a path, and falls through to PATH.
        assert_eq!(
            find_agent_program(
                "claude",
                Some("claude: aliased to ~/.claude/local/claude"),
                Some(&path_var),
                Some(&home)
            ),
            Some(path_claude.clone())
        );
        // A non-executable file on PATH is not the agent.
        std::fs::set_permissions(&path_claude, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            find_agent_program("claude", None, Some(&path_var), Some(&home)),
            Some(local_claude)
        );
        // Nothing anywhere is an honest `None`, never a guess.
        assert_eq!(
            find_agent_program(
                "claude",
                None,
                Some(&path_var),
                Some(&temp.path().join("empty"))
            ),
            None
        );
    }
}
