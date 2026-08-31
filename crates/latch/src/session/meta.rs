//! Small metadata sidecar for display and launch fields the kernel does not own.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::{DisplayMetadata, LaunchSpec, SourceInfo, TerminalSize};
use super::paths::{SessionPaths, FILE_MODE};

/// Metadata schema version.
pub const META_FORMAT_VERSION: u32 = 1;
/// Maximum persisted display length.
pub const MAX_DISPLAY_CHARS: usize = 200;

/// Persisted display metadata for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Schema version.
    pub format_version: u32,
    /// Session identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional human title.
    pub title: Option<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Redacted program label.
    pub command_label: String,
    /// Hosted harness Latch recognized from launch argv, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Initial geometry.
    pub initial_size: TerminalSize,
    /// Integration provenance.
    pub source: SourceInfo,
}

/// Exit information derived from latchd's retained child status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    /// Shell-compatible exit status, including `128 + signal`.
    pub code: Option<i32>,
    /// Signal reported by the session kernel.
    pub signal: Option<String>,
    /// Best available RFC 3339 exit observation time.
    pub exited_at: String,
}

/// Named arguments used to derive safe metadata.
pub struct MetaRequest<'a> {
    /// Session id.
    pub id: &'a str,
    /// Launch details.
    pub launch: &'a LaunchSpec,
    /// Display details.
    pub display: &'a DisplayMetadata,
    /// Creation timestamp.
    pub created_at: &'a str,
}

struct AtomicWrite<'a, T> {
    path: &'a Path,
    value: &'a T,
    exclusive: bool,
}

/// Metadata read/write failures.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// Filesystem failure.
    #[error("{path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Metadata already exists.
    #[error("{path} already exists; meta.json is written once")]
    AlreadyWritten {
        /// Existing metadata path.
        path: PathBuf,
    },
    /// Metadata was malformed.
    #[error("{path} is malformed: {detail}")]
    Malformed {
        /// Metadata path.
        path: PathBuf,
        /// Parser detail.
        detail: String,
    },
    /// Metadata schema is unsupported.
    #[error("{path} has format version {found}; this build reads {supported}")]
    UnsupportedVersion {
        /// Metadata path.
        path: PathBuf,
        /// Found version.
        found: u32,
        /// Supported version.
        supported: u32,
    },
}

/// Derives bounded, non-secret metadata from launch material.
pub fn derive(request: MetaRequest<'_>) -> SessionMeta {
    let command_label = request
        .display
        .command_label
        .as_deref()
        .map(sanitize_display)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| redact_command(&request.launch.argv));
    let name = request
        .display
        .name
        .as_deref()
        .map(sanitize_display)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| derive_name(&request.launch.cwd, &command_label));
    SessionMeta {
        format_version: META_FORMAT_VERSION,
        id: sanitize_display(request.id),
        name,
        title: request
            .display
            .title
            .as_deref()
            .map(sanitize_display)
            .filter(|title| !title.is_empty()),
        cwd: request.launch.cwd.clone(),
        command_label,
        harness: harness_kind(&request.launch.argv).map(str::to_owned),
        created_at: request.created_at.to_owned(),
        initial_size: request.launch.size,
        source: SourceInfo {
            kind: sanitize_display(&request.display.source.kind),
            external_run_id: request
                .display
                .source
                .external_run_id
                .as_deref()
                .map(sanitize_display)
                .filter(|value| !value.is_empty()),
        },
    }
}

/// Writes metadata exactly once.
pub fn write_once(paths: &SessionPaths, meta: &SessionMeta) -> Result<(), MetaError> {
    if paths.meta().exists() {
        return Err(MetaError::AlreadyWritten { path: paths.meta() });
    }
    write_atomic(AtomicWrite {
        path: &paths.meta(),
        value: meta,
        exclusive: true,
    })
}

/// Atomically updates display metadata.
pub fn update(paths: &SessionPaths, meta: &SessionMeta) -> Result<(), MetaError> {
    if !paths.meta().is_file() {
        return Err(MetaError::Io {
            path: paths.meta(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "meta.json is missing"),
        });
    }
    write_atomic(AtomicWrite {
        path: &paths.meta(),
        value: meta,
        exclusive: false,
    })
}

/// Reads and validates metadata.
pub fn read(paths: &SessionPaths) -> Result<SessionMeta, MetaError> {
    let path = paths.meta();
    let bytes = fs::read(&path).map_err(|source| MetaError::Io {
        path: path.clone(),
        source,
    })?;
    let meta: SessionMeta =
        serde_json::from_slice(&bytes).map_err(|error| MetaError::Malformed {
            path: path.clone(),
            detail: error.to_string(),
        })?;
    if meta.format_version != META_FORMAT_VERSION {
        return Err(MetaError::UnsupportedVersion {
            path,
            found: meta.format_version,
            supported: META_FORMAT_VERSION,
        });
    }
    Ok(meta)
}

/// Sanitizes untrusted display text.
pub fn sanitize_display(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(MAX_DISPLAY_CHARS)
        .collect()
}

/// Hosted harness Latch recognizes from launch argv.
pub fn harness_kind(argv: &[String]) -> Option<&'static str> {
    match redact_command(argv).as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// Reduces argv to the executable basename.
pub fn redact_command(argv: &[String]) -> String {
    argv.first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|program| program.to_str())
        .map(sanitize_display)
        .unwrap_or_default()
}

/// Derives a recognizable default name.
pub fn derive_name(cwd: &Path, command_label: &str) -> String {
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

fn write_atomic<T: Serialize>(request: AtomicWrite<'_, T>) -> Result<(), MetaError> {
    let bytes = serde_json::to_vec(request.value).map_err(|error| MetaError::Malformed {
        path: request.path.to_owned(),
        detail: error.to_string(),
    })?;
    let parent = request.path.parent().expect("session files have a parent");
    let temp = parent.join(format!(".meta.json.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&temp)
        .map_err(|source| MetaError::Io {
            path: temp.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| MetaError::Io {
            path: temp.clone(),
            source,
        })?;
    drop(file);
    let result = if request.exclusive {
        fs::hard_link(&temp, request.path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                MetaError::AlreadyWritten {
                    path: request.path.to_owned(),
                }
            } else {
                MetaError::Io {
                    path: request.path.to_owned(),
                    source,
                }
            }
        })
    } else {
        fs::rename(&temp, request.path).map_err(|source| MetaError::Io {
            path: request.path.to_owned(),
            source,
        })
    };
    let _ = fs::remove_file(&temp);
    result?;
    fs::set_permissions(request.path, fs::Permissions::from_mode(FILE_MODE)).map_err(|source| {
        MetaError::Io {
            path: request.path.to_owned(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manifest::LaunchSpec;

    fn launch(argv: Vec<String>) -> LaunchSpec {
        LaunchSpec {
            argv,
            cwd: PathBuf::from("/tmp/app"),
            env: Default::default(),
            inherit_env: true,
            size: TerminalSize::new(80, 24),
            term: "xterm-256color".to_owned(),
        }
    }

    fn derive_argv(argv: Vec<String>) -> SessionMeta {
        let launch = launch(argv);
        let display = DisplayMetadata::default();
        derive(MetaRequest {
            id: "ses_test",
            launch: &launch,
            display: &display,
            created_at: "2026-08-13T00:00:00Z",
        })
    }

    #[test]
    fn claude_argv_persists_a_harness_marker() {
        let meta = derive_argv(vec!["/usr/local/bin/claude".to_owned(), "-p".to_owned()]);
        assert_eq!(meta.harness.as_deref(), Some("claude"));
        assert_eq!(meta.command_label, "claude");
    }

    #[test]
    fn codex_argv_persists_a_harness_marker() {
        let meta = derive_argv(vec!["/usr/local/bin/codex".to_owned(), "exec".to_owned()]);
        assert_eq!(meta.harness.as_deref(), Some("codex"));
        assert_eq!(meta.command_label, "codex");
    }

    #[test]
    fn shell_argv_does_not_mark_a_harness() {
        let meta = derive_argv(vec!["/bin/zsh".to_owned(), "-il".to_owned()]);
        assert_eq!(meta.harness, None);
        assert_eq!(meta.command_label, "zsh");
    }
}
