//! Private on-disk layout used by Latch.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Directory mode for private Latch state.
pub const DIR_MODE: u32 = 0o700;
/// File mode for private metadata and configuration.
pub const FILE_MODE: u32 = 0o600;
/// Environment variable that relocates Latch state.
pub const HOME_ENV: &str = "LATCH_HOME";
/// Environment marker exported into a hosted session.
pub const SESSION_ID_ENV: &str = "LATCH_SESSION_ID";
/// Prefix for opaque session identifiers.
pub const SESSION_ID_PREFIX: &str = "ses_";

/// Path-related failures.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The state root could not be located.
    #[error("cannot locate the Latch root: {detail}")]
    NoRoot {
        /// Missing input.
        detail: String,
    },
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A path was not the expected kind.
    #[error("{path} is not a {expected}")]
    WrongKind {
        /// Offending path.
        path: PathBuf,
        /// Expected kind.
        expected: &'static str,
    },
    /// A session id was malformed.
    #[error("`{raw}` is not a session id")]
    InvalidSessionId {
        /// Rejected value.
        raw: String,
    },
}

/// Validated opaque session identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Generates a fresh time-ordered identifier.
    pub fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let process = std::process::id();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!(
            "{SESSION_ID_PREFIX}{timestamp:x}{process:x}{sequence:x}"
        ))
    }

    /// Parses and validates a session id.
    pub fn parse(raw: &str) -> Result<Self, PathsError> {
        let suffix = raw.strip_prefix(SESSION_ID_PREFIX);
        if suffix.is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PathsError::InvalidSessionId {
                raw: raw.to_owned(),
            })
        }
    }

    /// Returns the wire representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Latch's private state root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatchHome {
    root: PathBuf,
}

impl LatchHome {
    /// Uses an explicit state root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves `LATCH_HOME`, otherwise `~/.latch`.
    pub fn from_env() -> Result<Self, PathsError> {
        if let Some(root) = std::env::var_os(HOME_ENV) {
            return Ok(Self::new(root));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| PathsError::NoRoot {
            detail: "$HOME is not set".to_owned(),
        })?;
        Ok(Self::new(PathBuf::from(home).join(".latch")))
    }

    /// Root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Session metadata directory.
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Generated tmux configuration.
    pub fn tmux_config(&self) -> PathBuf {
        self.root.join("tmux.conf")
    }

    /// Private tmux server socket.
    pub fn server_socket(&self) -> PathBuf {
        self.root.join("server")
    }

    /// User preferences.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Bearer token used by `latch serve`.
    pub fn serve_token(&self) -> PathBuf {
        self.root.join("serve.token")
    }

    /// Owner-only state for the paired LAN remote-access prototype.
    pub fn remote_access_dir(&self) -> PathBuf {
        self.root.join("remote-access")
    }

    /// Paths belonging to one session.
    pub fn session(&self, id: &SessionId) -> SessionPaths {
        SessionPaths::new(self.sessions_dir().join(id.as_str()))
    }

    /// Creates and tightens private directories.
    pub fn ensure(&self) -> Result<(), PathsError> {
        ensure_directory(&self.root)?;
        ensure_directory(&self.sessions_dir())
    }

    /// Returns all metadata session ids in lexical order.
    pub fn session_ids(&self) -> Result<Vec<SessionId>, PathsError> {
        let entries = fs::read_dir(self.sessions_dir()).map_err(|source| PathsError::Io {
            path: self.sessions_dir(),
            source,
        })?;
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| PathsError::Io {
                path: self.sessions_dir(),
                source,
            })?;
            if !entry
                .file_type()
                .map_err(|source| PathsError::Io {
                    path: entry.path(),
                    source,
                })?
                .is_dir()
            {
                continue;
            }
            if let Some(raw) = entry.file_name().to_str() {
                if let Ok(id) = SessionId::parse(raw) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// Files belonging to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPaths {
    dir: PathBuf,
}

impl SessionPaths {
    /// Uses an explicit session directory.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Metadata sidecar.
    pub fn meta(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    /// Ephemeral FIFO used to transfer launch material.
    pub fn launch_fifo(&self) -> PathBuf {
        self.dir.join("launch.pipe")
    }

    /// Per-phase launch timings appended by create, open, and the launcher.
    pub fn launch_timings(&self) -> PathBuf {
        self.dir.join("launch-timings.jsonl")
    }

    /// Marker written while a viewer open is in flight.
    pub fn viewer_open(&self) -> PathBuf {
        self.dir.join("viewer-open.json")
    }

    /// Viewer-open diagnostics, kept on disk because the viewer may fail after
    /// `latch open` has already returned.
    pub fn viewer_log(&self) -> PathBuf {
        self.dir.join("viewer-open.log")
    }

    /// Raw Claude hook records captured for this session.
    pub fn harness_hooks(&self) -> PathBuf {
        self.dir.join("harness-hooks.jsonl")
    }

    /// Persistent normalized event ledger used for stable cursors.
    pub fn harness_events(&self) -> PathBuf {
        self.dir.join("harness-events.ndjson")
    }

    /// Advisory writer lock for the normalized event ledger.
    pub fn harness_events_lock(&self) -> PathBuf {
        self.dir.join("harness-events.lock")
    }

    /// Applied request-bound answers, used to reject repeated resolutions.
    pub fn harness_resolutions(&self) -> PathBuf {
        self.dir.join("harness-resolutions.ndjson")
    }

    /// Advisory lock held by `latch send` while applying capability-gated input.
    /// Read-only capability queries do not take this lock.
    pub fn harness_interaction_lock(&self) -> PathBuf {
        self.dir.join("harness-interaction.lock")
    }

    /// Creates the private session directory.
    pub fn ensure(&self) -> Result<(), PathsError> {
        ensure_directory(&self.dir)
    }

    /// Removes retained metadata after verifying its identity.
    pub fn remove(&self) -> Result<(), PathsError> {
        if !self.meta().is_file() {
            return Err(PathsError::WrongKind {
                path: self.meta(),
                expected: "session directory with meta.json",
            });
        }
        fs::remove_dir_all(&self.dir).map_err(|source| PathsError::Io {
            path: self.dir.clone(),
            source,
        })
    }
}

fn ensure_directory(path: &Path) -> Result<(), PathsError> {
    fs::create_dir_all(path).map_err(|source| PathsError::Io {
        path: path.to_owned(),
        source,
    })?;
    let metadata = fs::metadata(path).map_err(|source| PathsError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(PathsError::WrongKind {
            path: path.to_owned(),
            expected: "directory",
        });
    }
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE)).map_err(|source| {
        PathsError::Io {
            path: path.to_owned(),
            source,
        }
    })
}
