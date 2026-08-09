//! `meta.json` and `exit.json`.
//!
//! Both are written once, by temp file plus rename, so a reader never sees a
//! partial file. `latch list` with thirty sessions open reads thirty of these
//! while workers are starting; a reader that can observe a half-written JSON
//! object is a bug that only shows up under exactly that load.
//!
//! `meta.json` holds bounded display metadata and nothing else. Secrets, full
//! environment blocks, and raw argv never appear in it — the command is recorded
//! as a redacted label. This is a hard rule rather than a convention: the file is
//! `0600`, but "we only wrote non-secret things into it" is the property that
//! actually holds when the file is later synced, backed up, or read by another
//! tool.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::worker::manifest::{DisplayMetadata, LaunchSpec, SourceInfo, TerminalSize};
use crate::worker::paths::SessionPaths;
use crate::worker::process::ChildExit;

/// `meta.json` schema version this build writes.
pub const META_FORMAT_VERSION: u32 = 1;

/// Longest a sanitized display string may be, in characters.
///
/// A title arrives from another application and ends up in a terminal title
/// escape sequence and in `latch list`; unbounded, it is a denial of service
/// against the user's own terminal.
pub const MAX_DISPLAY_CHARS: usize = 200;

/// What a reader of a session directory is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Schema version.
    pub format_version: u32,
    /// Session identifier; matches the directory name.
    pub id: String,
    /// Short name, auto-derived unless one was given.
    pub name: String,
    /// Human title, or `None` when never set.
    pub title: Option<String>,
    /// Working directory the child was started in.
    pub cwd: PathBuf,
    /// Redacted command label — never full argv.
    pub command_label: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// The size at spawn, before any controller attached.
    pub initial_size: TerminalSize,
    /// Where the session came from.
    pub source: SourceInfo,
}

/// `exit.json`: the last thing a worker writes.
///
/// Its presence, with the socket gone, is what makes a session `exited` rather
/// than `lost`. It is written before the socket is removed, so a client that
/// notices the socket disappear can always find it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    /// Exit status, or `None` when the child was signalled.
    pub code: Option<i32>,
    /// Signal name such as `SIGTERM`, or `None` on a normal exit.
    pub signal: Option<String>,
    /// RFC 3339 timestamp.
    pub exited_at: String,
}

impl ExitRecord {
    /// The record for a child that ended as `exit` describes, at `at`.
    pub fn new(exit: &ChildExit, at: impl Into<String>) -> Self {
        Self {
            code: exit.code,
            signal: exit.signal.map(|s| s.name().to_owned()),
            exited_at: at.into(),
        }
    }
}

/// Anything that can go wrong with the two metadata files.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// The file could not be written, renamed, or read.
    #[error("{path}: {source}")]
    Io {
        /// Path being operated on.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The file exists and `meta.json` is written exactly once.
    ///
    /// A second write means two workers believe they own one session directory,
    /// which is a condition to report rather than to resolve by overwriting.
    #[error("{path} already exists; meta.json is written once")]
    AlreadyWritten {
        /// The existing file.
        path: PathBuf,
    },
    /// The file is present but is not the document it should be.
    #[error("{path} is malformed: {detail}")]
    Malformed {
        /// Path being read.
        path: PathBuf,
        /// Parser detail.
        detail: String,
    },
    /// The file declares a schema version this build does not read.
    #[error("{path} has format version {found}; this build reads {supported}")]
    UnsupportedVersion {
        /// Path being read.
        path: PathBuf,
        /// Version found.
        found: u32,
        /// Version supported.
        supported: u32,
    },
}

/// Builds the metadata for a session from its launch material.
///
/// This is where the separation is enforced: the [`LaunchSpec`] carrying argv and
/// environment goes in, and only bounded display metadata comes out. Every field
/// of the result is sanitized, and the command becomes a label.
pub fn derive(
    id: &str,
    launch: &LaunchSpec,
    display: &DisplayMetadata,
    created_at: &str,
) -> SessionMeta {
    let command_label = display
        .command_label
        .as_deref()
        .map(sanitize_display)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| redact_command(&launch.argv));
    let name = display
        .name
        .as_deref()
        .map(sanitize_display)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| derive_name(&launch.cwd, &command_label));
    SessionMeta {
        format_version: META_FORMAT_VERSION,
        id: sanitize_display(id),
        name,
        title: display
            .title
            .as_deref()
            .map(sanitize_display)
            .filter(|title| !title.is_empty()),
        cwd: launch.cwd.clone(),
        command_label,
        created_at: created_at.to_owned(),
        initial_size: launch.size,
        source: SourceInfo {
            kind: sanitize_display(&display.source.kind),
            external_run_id: display
                .source
                .external_run_id
                .as_deref()
                .map(sanitize_display)
                .filter(|value| !value.is_empty()),
        },
    }
}

/// Writes `meta.json` exactly once, by temp file plus rename.
///
/// Fails with [`MetaError::AlreadyWritten`] rather than overwriting. The rename
/// is what makes the write atomic for readers; the exclusive create of the final
/// name is what makes it once.
pub fn write_once(paths: &SessionPaths, meta: &SessionMeta) -> Result<(), MetaError> {
    if paths.meta().exists() {
        return Err(MetaError::AlreadyWritten { path: paths.meta() });
    }
    write_atomic(&paths.meta(), meta, true)
}

/// Rewrites `meta.json` after a rename or other display-metadata change.
///
/// Uses temp file plus rename so a concurrent `list` never sees a partial
/// document. Unlike [`write_once`], this replaces an existing file.
pub fn update(paths: &SessionPaths, meta: &SessionMeta) -> Result<(), MetaError> {
    if !paths.meta().is_file() {
        return Err(MetaError::Io {
            path: paths.meta(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "meta.json is missing"),
        });
    }
    write_atomic(&paths.meta(), meta, false)
}

/// Reads `meta.json`.
pub fn read(paths: &SessionPaths) -> Result<SessionMeta, MetaError> {
    let meta: SessionMeta = read_json(&paths.meta())?;
    if meta.format_version != META_FORMAT_VERSION {
        return Err(MetaError::UnsupportedVersion {
            path: paths.meta(),
            found: meta.format_version,
            supported: META_FORMAT_VERSION,
        });
    }
    Ok(meta)
}

/// Writes `exit.json` by temp file plus rename.
pub fn write_exit(paths: &SessionPaths, record: &ExitRecord) -> Result<(), MetaError> {
    write_atomic(&paths.exit(), record, false)
}

/// Reads `exit.json`, or `None` when the session has not exited.
pub fn read_exit(paths: &SessionPaths) -> Result<Option<ExitRecord>, MetaError> {
    if !paths.exit().exists() {
        return Ok(None);
    }
    read_json(&paths.exit()).map(Some)
}

/// Writes the final screen by temp file plus rename.
///
/// Not JSON: these are the same snapshot bytes a live worker sends a client
/// that attaches, and wrapping them in a document would mean two encodings of
/// one thing that could disagree. Written atomically for the same reason
/// `exit.json` is — a reader that arrives mid-write must see the old file or
/// the new one, never half of a screen.
pub fn write_screen(paths: &SessionPaths, snapshot: &[u8]) -> Result<(), MetaError> {
    write_bytes_atomic(&paths.screen(), snapshot, false)
}

/// Reads the final screen, or `None` when the session did not leave one.
///
/// `None` is ordinary rather than exceptional: a session that predates this
/// file, or whose worker was killed outright, has an exit record and no screen.
/// Callers show what they have.
pub fn read_screen(paths: &SessionPaths) -> Result<Option<Vec<u8>>, MetaError> {
    let path = paths.screen();
    if !path.exists() {
        return Ok(None);
    }
    fs::read(&path)
        .map(Some)
        .map_err(|source| MetaError::Io { path, source })
}

/// Reduces an externally supplied string to something safe to display.
///
/// Control characters are removed rather than escaped — a title is rendered into
/// a terminal title sequence, where an embedded escape is an injection — and the
/// result is truncated to [`MAX_DISPLAY_CHARS`]. Applied at ingest, at the
/// boundary where the string arrives.
pub fn sanitize_display(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(MAX_DISPLAY_CHARS)
        .collect()
}

/// Reduces argv to a label safe to record.
///
/// The program's file name and nothing else. Arguments are where the secrets
/// are — a token passed with `--token`, a URL with a query string — so no
/// argument is preserved, not even a truncated one.
pub fn redact_command(argv: &[String]) -> String {
    argv.first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|program| program.to_str())
        .map(sanitize_display)
        .unwrap_or_default()
}

/// Derives a session name from a working directory and a command label.
///
/// With every terminal window a session, `latch list` is the primary navigation
/// surface, so an auto-derived name has to be recognizable without being
/// explained.
pub fn derive_name(cwd: &std::path::Path, command_label: &str) -> String {
    let directory = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("session");
    let directory = sanitize_display(directory);
    if command_label.is_empty() {
        directory
    } else {
        sanitize_display(&format!("{directory}-{command_label}"))
    }
}

fn write_atomic<T: Serialize>(path: &Path, value: &T, exclusive: bool) -> Result<(), MetaError> {
    let bytes = serde_json::to_vec(value).map_err(|error| MetaError::Malformed {
        path: path.to_owned(),
        detail: error.to_string(),
    })?;
    write_bytes_atomic(path, &bytes, exclusive)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8], exclusive: bool) -> Result<(), MetaError> {
    let parent = path.parent().expect("session files have a parent");
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(crate::worker::paths::FILE_MODE)
        .open(&temp)
        .map_err(|source| MetaError::Io {
            path: temp.clone(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| MetaError::Io {
            path: temp.clone(),
            source,
        })?;
    drop(file);
    let result = if exclusive {
        fs::hard_link(&temp, path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                MetaError::AlreadyWritten {
                    path: path.to_owned(),
                }
            } else {
                MetaError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })
    } else {
        fs::rename(&temp, path).map_err(|source| MetaError::Io {
            path: path.to_owned(),
            source,
        })
    };
    let _ = fs::remove_file(&temp);
    result?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(crate::worker::paths::FILE_MODE),
    )
    .map_err(|source| MetaError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, MetaError> {
    let bytes = fs::read(path).map_err(|source| MetaError::Io {
        path: path.to_owned(),
        source,
    })?;
    let value: T = serde_json::from_slice(&bytes).map_err(|error| MetaError::Malformed {
        path: path.to_owned(),
        detail: error.to_string(),
    })?;
    Ok(value)
}
