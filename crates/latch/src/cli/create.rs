//! Session creation on the private tmux kernel.
//!
//! Bare `latch`, `latch shell`, and `latch run -- <cmd>` all land here. So does
//! `latch create --manifest-file -`, which is the path M3's Overlord provider
//! will use and which exists at M1 so launch secrets never travel in argv.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use crate::cli::nesting::{self, NestingDecision, SESSION_ID_ENV};
use crate::engine::{self, CreateRequest};
use crate::session::manifest::{DisplayMetadata, LaunchManifest, LaunchRequest, TerminalSize};
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId, SessionPaths};
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
    /// The child's initial terminal size before a client attaches.
    pub size: TerminalSize,
    /// Human-display fields supplied at the CLI boundary.
    pub display: DisplayMetadata,
}

/// Creates a session: id, directory at `0700`, `meta.json` via temp+rename,
/// detached tmux session, optional attach.
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
