//! Session creation on the private latchd kernel.
//!
//! Bare `latch`, `latch shell`, and `latch run -- <cmd>` all land here. So does
//! `latch create --manifest-file -`, which is the path M3's Overlord provider
//! will use and which exists at M1 so launch secrets never travel in argv.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use crate::cli::nesting::{self, NestingDecision, SESSION_ID_ENV};
use crate::engine::{self, CreateRequest};
use crate::session::manifest::{DisplayMetadata, LaunchManifest, LaunchRequest, TerminalSize};
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

/// One phone request to start a standard shell.
///
/// There is deliberately no argv, environment, shell, display metadata, or
/// terminal type here: the only caller-controlled inputs are the correlation
/// id and where the shell starts.
#[derive(Debug, Clone)]
pub struct RemoteShellRequest {
    /// Where sessions live for this gateway.
    pub home: LatchHome,
    /// Opaque per-attempt correlation id, retained across the phone's retries.
    pub request_id: String,
    /// Absolute directory the shell starts in. Canonicalized by the caller and
    /// re-canonicalized here immediately before creation.
    pub cwd: PathBuf,
}

/// Why an idempotent remote creation could not produce a session.
#[derive(Debug)]
pub enum RemoteShellError {
    /// The request id already named a session started somewhere else.
    RequestIdConflict,
    /// Creation itself failed.
    Failed(anyhow::Error),
}

/// What an idempotent remote creation resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteShellOutcome {
    /// Session identifier.
    pub id: String,
    /// Stored display name.
    pub name: String,
    /// RFC 3339 creation time read back from durable metadata.
    pub created_at: String,
    /// True when an earlier request with this id had already created it.
    pub reused: bool,
}

/// Creates exactly one standard login shell for `request`, or returns the one
/// an earlier attempt with the same request id already created.
///
/// Idempotency lives here, at the create boundary, because only this machine
/// can know whether a process actually started: a phone that loses the
/// response cannot tell a failed launch from a lost reply.
pub fn create_remote_shell(
    request: RemoteShellRequest,
) -> Result<RemoteShellOutcome, RemoteShellError> {
    create_remote_shell_with(request, create_session_detached)
}

fn create_remote_shell_with(
    request: RemoteShellRequest,
    launch: impl FnOnce(CreateOptions) -> anyhow::Result<CreateOutcome>,
) -> Result<RemoteShellOutcome, RemoteShellError> {
    request.home.ensure().map_err(failed)?;
    // Held from the lookup through durable metadata creation, so two
    // concurrent requests carrying one request id — or a retry after a lost
    // response — cannot both pass the scan and each start a shell.
    let _lock = CreationLock::acquire(&request.home).map_err(RemoteShellError::Failed)?;

    // Canonicalized again under the lock: the directory the phone browsed may
    // have been replaced between validation and creation.
    let cwd = request
        .cwd
        .canonicalize()
        .map_err(|error| RemoteShellError::Failed(error.into()))?;
    if !cwd.is_dir() {
        return Err(RemoteShellError::Failed(anyhow::anyhow!(
            "session working directory is not a directory"
        )));
    }

    if let Some(existing) =
        find_remote_session(&request.home, &request.request_id).map_err(failed)?
    {
        if existing.cwd != cwd {
            return Err(RemoteShellError::RequestIdConflict);
        }
        return Ok(RemoteShellOutcome {
            id: existing.id,
            name: existing.name,
            created_at: existing.created_at,
            reused: true,
        });
    }

    let manifest = shell_manifest(ManifestOptions {
        cwd,
        size: REMOTE_INITIAL_SIZE,
        display: DisplayMetadata {
            source: crate::session::manifest::SourceInfo {
                kind: MOBILE_SOURCE_KIND.to_owned(),
                external_run_id: Some(request.request_id),
            },
            ..DisplayMetadata::default()
        },
    });
    let outcome = launch(CreateOptions {
        home: request.home.clone(),
        manifest,
        // A remote creation starts a shell and nothing else: no attach client
        // is spawned, and no existing session's surface is touched.
        attach: false,
    })
    .map_err(RemoteShellError::Failed)?;
    let meta = meta::read(&outcome.paths).map_err(failed)?;
    Ok(RemoteShellOutcome {
        id: outcome.id.to_string(),
        name: outcome.name,
        created_at: meta.created_at,
        reused: false,
    })
}

fn failed(error: impl Into<anyhow::Error>) -> RemoteShellError {
    RemoteShellError::Failed(error.into())
}

struct RemoteSession {
    id: String,
    name: String,
    created_at: String,
    cwd: PathBuf,
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

    fn request(home: &LatchHome, cwd: &std::path::Path) -> RemoteShellRequest {
        RemoteShellRequest {
            home: home.clone(),
            request_id: REQUEST_ID.to_owned(),
            cwd: cwd.to_owned(),
        }
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
        assert!(matches!(conflict, Err(RemoteShellError::RequestIdConflict)));
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
        assert!(matches!(failure, Err(RemoteShellError::Failed(_))));
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
        assert!(matches!(result, Err(RemoteShellError::Failed(_))));
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
}
