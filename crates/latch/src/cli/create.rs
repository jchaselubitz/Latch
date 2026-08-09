//! Session creation: allocate an id, write `meta.json`, spawn the worker, attach.
//!
//! Bare `latch`, `latch shell`, and `latch run -- <cmd>` all land here. So does
//! `latch create --manifest-file -`, which is the path M3's Overlord provider
//! will use and which exists at M1 so launch secrets never travel in argv.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use crate::cli::nesting::{self, NestingDecision, SESSION_ID_ENV};
use crate::worker::manifest::{DisplayMetadata, LaunchManifest, TerminalSize};
use crate::worker::meta;
use crate::worker::paths::{LatchHome, SessionId, SessionPaths};
use crate::worker::spawn::{self, SpawnRequest};
use anyhow::{bail, Context};

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
    /// The size supplied to the worker before a controller attaches.
    pub size: TerminalSize,
    /// Human-display fields supplied at the CLI boundary.
    pub display: DisplayMetadata,
}

/// Creates a session: id, directory at `0700`, `meta.json` via temp+rename,
/// detached worker, optional attach.
///
/// Display fields on the manifest are sanitized at this boundary — the single
/// place externally supplied names and titles arrive — before anything is
/// written or shown.
pub fn create_session(options: CreateOptions) -> anyhow::Result<CreateOutcome> {
    match nesting::nesting_decision(enclosing_session_id().as_deref(), true) {
        NestingDecision::Allow => {}
        NestingDecision::AttachToEnclosing { session_id } => {
            bail!(
                "refusing to create a nested Latch session (already inside {session_id}); \
                 run `latch attach` or exit the enclosing session first"
            );
        }
        NestingDecision::Decline { reason } => bail!("{reason}"),
    }

    options.home.ensure()?;

    let mut manifest = options.manifest;
    sanitize_display_metadata(&mut manifest.display);

    let (id, paths) = loop {
        let id = SessionId::generate();
        let paths = options.home.session(&id);
        if !paths.dir().exists() {
            break (id, paths);
        }
    };

    let worker_binary = worker_binary()?;
    spawn::spawn_detached(SpawnRequest::new(
        paths.clone(),
        id.clone(),
        worker_binary,
        manifest,
    ))?;

    let name = meta::read(&paths)?.name;
    Ok(CreateOutcome { id, paths, name })
}

/// Builds the launch material for a shell session.
pub fn shell_manifest(options: ManifestOptions) -> LaunchManifest {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let mut manifest = LaunchManifest::new(vec![shell], options.cwd, options.size);
    manifest.display = options.display;
    manifest
}

/// Builds the launch material for `latch run -- <argv>`.
pub fn run_manifest(argv: Vec<String>, options: ManifestOptions) -> LaunchManifest {
    let mut manifest = LaunchManifest::new(argv, options.cwd, options.size);
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
        return crate::worker::manifest::read(io::stdin()).map_err(Into::into);
    }
    let file = File::open(path).with_context(|| format!("cannot open launch manifest {path}"))?;
    crate::worker::manifest::read(file).map_err(Into::into)
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

fn worker_binary() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_latch") {
        return Ok(path.into());
    }

    let current = std::env::current_exe().context("cannot locate the current executable")?;
    if current.file_name().is_some_and(|name| name == "latch") {
        return Ok(current);
    }

    let test_binary = current
        .parent()
        .and_then(|directory| directory.parent())
        .map(|directory| directory.join("latch"))
        .filter(|path| path.is_file());
    test_binary.context("cannot locate the latch worker binary")
}

/// Reads [`SESSION_ID_ENV`] from this process, if set to a non-empty value.
pub fn enclosing_session_id() -> Option<String> {
    std::env::var(SESSION_ID_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
