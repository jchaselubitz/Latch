//! The `~/.latch` layout, and the modes it is held to.
//!
//! ```text
//! ~/.latch/
//!   config.toml                  # non-secret preferences
//!   sessions/
//!     ses_01J.../
//!       meta.json                # written once at spawn (temp file + rename)
//!       control.sock             # worker socket; connectable == alive
//!       journal                  # bounded output journal
//!       exit.json                # written by the worker at exit
//! ```
//!
//! Every directory here is `0700` and every file `0600`, asserted by tests
//! rather than left to whatever umask the launching terminal happened to have.
//! The journal holds terminal output, so "owner-only" is a privacy requirement,
//! not tidiness.
//!
//! There is no registry file in this layout and there is no place to add one:
//! session state is derived from the socket and `exit.json`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Mode for `~/.latch` and every session directory.
pub const DIR_MODE: u32 = 0o700;

/// Mode for the control socket.
pub const SOCKET_MODE: u32 = 0o600;

/// Mode for `meta.json`, `exit.json`, and the journal.
pub const FILE_MODE: u32 = 0o600;

/// Environment variable that relocates the Latch root.
///
/// Exists for tests and for a user with an unusual home; a session directory is
/// self-describing, so relocating the root costs nothing.
pub const HOME_ENV: &str = "LATCH_HOME";

/// Environment variable the worker exports into every session child.
///
/// Its presence means "you are already inside a Latch session." The CLI reads it
/// before creating anything and either attaches to the named session or declines
/// — never a session within a session.
pub const SESSION_ID_ENV: &str = "LATCH_SESSION_ID";

/// Basename of the control socket inside a session directory.
pub const SOCKET_NAME: &str = "control.sock";

/// Basename of the metadata file.
pub const META_NAME: &str = "meta.json";

/// Basename of the exit record.
pub const EXIT_NAME: &str = "exit.json";

/// Basename of the output journal.
pub const JOURNAL_NAME: &str = "journal";

/// Basename of the final screen an exited session left behind.
///
/// A snapshot, written once at exit, and the same bytes a live worker would
/// have sent a client that attached a moment earlier. It exists so that an
/// exited session can still be *looked at* — the alternative is reconstructing
/// the screen by replaying the journal, which is precisely the byte-tail replay
/// the screen model exists to avoid.
pub const SCREEN_NAME: &str = "screen";

/// Basename of the sessions directory inside the Latch root.
pub const SESSIONS_DIR: &str = "sessions";

/// Prefix every session identifier carries.
pub const SESSION_ID_PREFIX: &str = "ses_";

/// Anything that can go wrong locating or preparing a path.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The Latch root could not be located.
    #[error("cannot locate the Latch root: {detail}")]
    NoRoot {
        /// What was missing — `$HOME`, or a `LATCH_HOME` that is not usable.
        detail: String,
    },
    /// A directory or file could not be created, read, or re-moded.
    #[error("{path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A path exists but is not the kind of thing it must be.
    #[error("{path} is not a {expected}")]
    WrongKind {
        /// The offending path.
        path: PathBuf,
        /// What was expected there — `directory`, `socket`, `file`.
        expected: &'static str,
    },
    /// A string is not a well-formed session identifier.
    #[error("`{raw}` is not a session id")]
    InvalidSessionId {
        /// The rejected string.
        raw: String,
    },
}

/// A session identifier.
///
/// Opaque and validated at construction, because it is also a directory name: a
/// caller-supplied identifier that reached `PathBuf::join` unchecked would be a
/// path-traversal bug in a product whose whole job is to hand out session names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Generates a fresh, time-ordered identifier.
    ///
    /// Time-ordered so `latch list` has a stable secondary sort and so a
    /// directory listing reads chronologically without opening anything.
    pub fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("{SESSION_ID_PREFIX}{timestamp:x}{sequence:x}"))
    }

    /// Validates a string as a session identifier.
    ///
    /// Accepts `ses_` followed by at least one character drawn from ASCII
    /// alphanumerics; rejects everything else, including anything containing a
    /// path separator or a `.` component.
    pub fn parse(raw: &str) -> Result<Self, PathsError> {
        let valid = raw.len() > SESSION_ID_PREFIX.len()
            && raw.starts_with(SESSION_ID_PREFIX)
            && raw[SESSION_ID_PREFIX.len()..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric());
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PathsError::InvalidSessionId {
                raw: raw.to_owned(),
            })
        }
    }

    /// The identifier as it appears on disk and on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The Latch root — `~/.latch` unless [`HOME_ENV`] says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatchHome {
    root: PathBuf,
}

impl LatchHome {
    /// Uses `root` as the Latch root without touching the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Locates the root from the environment: [`HOME_ENV`], else `~/.latch`.
    pub fn from_env() -> Result<Self, PathsError> {
        if let Some(root) = std::env::var_os(HOME_ENV) {
            return Ok(Self::new(root));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| PathsError::NoRoot {
            detail: "$HOME is not set".to_owned(),
        })?;
        Ok(Self::new(PathBuf::from(home).join(".latch")))
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding session directories.
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join(SESSIONS_DIR)
    }

    /// The non-secret preferences file.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Paths for one session, whether or not it exists yet.
    pub fn session(&self, id: &SessionId) -> SessionPaths {
        SessionPaths::new(self.sessions_dir().join(id.as_str()))
    }

    /// Creates the root and the sessions directory at [`DIR_MODE`], and tightens
    /// them if they already exist with looser modes.
    ///
    /// Tightening rather than only creating is the point: a root created before
    /// this rule existed, or by a user with a permissive umask, is exactly the
    /// case where terminal output leaks.
    pub fn ensure(&self) -> Result<(), PathsError> {
        ensure_directory(&self.root)?;
        ensure_directory(&self.sessions_dir())
    }

    /// Every session directory currently present, in identifier order.
    ///
    /// This is a directory listing, not a registry read. Entries that are not
    /// well-formed session directories are skipped rather than failing the call.
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

/// The files one session owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPaths {
    dir: PathBuf,
}

impl SessionPaths {
    /// Uses `dir` as a session directory without touching the filesystem.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `meta.json`.
    pub fn meta(&self) -> PathBuf {
        self.dir.join(META_NAME)
    }

    /// `control.sock`.
    pub fn socket(&self) -> PathBuf {
        self.dir.join(SOCKET_NAME)
    }

    /// `journal`.
    pub fn journal(&self) -> PathBuf {
        self.dir.join(JOURNAL_NAME)
    }

    /// `screen`: the final snapshot an exited session left behind.
    pub fn screen(&self) -> PathBuf {
        self.dir.join(SCREEN_NAME)
    }

    /// `exit.json`.
    pub fn exit(&self) -> PathBuf {
        self.dir.join(EXIT_NAME)
    }

    /// Creates the session directory at [`DIR_MODE`].
    pub fn ensure(&self) -> Result<(), PathsError> {
        ensure_directory(&self.dir)
    }

    /// Removes the session directory and everything in it.
    ///
    /// Used by `latch prune`. Refuses a directory that has no `meta.json`, so a
    /// mistyped path cannot become a recursive delete.
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
