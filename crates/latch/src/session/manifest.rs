//! Launch material accepted by `latch create`.
//!
//! The document is bounded and validated before a session is created. Secrets
//! are held in memory and passed to the child through a private FIFO.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Manifest schema version this build writes and accepts.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// Largest accepted launch manifest.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// A terminal size in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl TerminalSize {
    /// Creates a terminal size.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// What to run, where, and with what environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// Program and arguments. Never persisted by Latch.
    pub argv: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Environment entries to set.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whether the child inherits the launcher environment.
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    /// Initial terminal size.
    pub size: TerminalSize,
    /// Requested terminal type. Latch normalizes this to tmux's pinned value.
    #[serde(default = "default_term")]
    pub term: String,
}

/// Bounded metadata safe to persist and display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DisplayMetadata {
    /// Short display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Human title.
    #[serde(default)]
    pub title: Option<String>,
    /// Redacted command label.
    #[serde(default)]
    pub command_label: Option<String>,
    /// Session provenance.
    #[serde(default)]
    pub source: SourceInfo,
}

/// Session provenance supplied by an integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Broad source kind.
    pub kind: String,
    /// Opaque external correlation id.
    #[serde(default)]
    pub external_run_id: Option<String>,
}

impl Default for SourceInfo {
    fn default() -> Self {
        Self {
            kind: "cli".to_owned(),
            external_run_id: None,
        }
    }
}

/// Everything needed to launch a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchManifest {
    /// Schema version.
    pub format_version: u32,
    /// Child launch details.
    pub launch: LaunchSpec,
    /// Display-only metadata.
    #[serde(default)]
    pub display: DisplayMetadata,
}

/// Named arguments for constructing a manifest.
pub struct LaunchRequest {
    /// Program and arguments.
    pub argv: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Initial terminal size.
    pub size: TerminalSize,
}

impl LaunchManifest {
    /// Creates a manifest with safe defaults.
    pub fn new(request: LaunchRequest) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            launch: LaunchSpec {
                argv: request.argv,
                cwd: request.cwd,
                env: BTreeMap::new(),
                inherit_env: true,
                size: request.size,
                term: default_term(),
            },
            display: DisplayMetadata::default(),
        }
    }
}

/// Errors produced while reading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Input could not be read.
    #[error("cannot read launch manifest: {0}")]
    Io(#[from] std::io::Error),
    /// JSON was malformed.
    #[error("launch manifest is malformed: {detail}")]
    Malformed {
        /// Parser detail.
        detail: String,
    },
    /// Schema version is unsupported.
    #[error("launch manifest format version {found}; this build reads {supported}")]
    UnsupportedVersion {
        /// Version supplied.
        found: u32,
        /// Version accepted.
        supported: u32,
    },
    /// A field cannot be acted on.
    #[error("launch manifest field `{field}` is unusable: {detail}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Validation detail.
        detail: String,
    },
    /// Input exceeded the hard limit.
    #[error("launch manifest exceeds {limit} bytes")]
    TooLarge {
        /// Maximum accepted bytes.
        limit: usize,
    },
}

/// Reads and validates a bounded manifest.
pub fn read(reader: impl Read) -> Result<LaunchManifest, ManifestError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge {
            limit: MAX_MANIFEST_BYTES,
        });
    }
    let manifest = serde_json::from_slice(&bytes).map_err(|error| ManifestError::Malformed {
        detail: error.to_string(),
    })?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Writes a validated manifest.
pub fn write(writer: impl Write, manifest: &LaunchManifest) -> Result<(), ManifestError> {
    validate(manifest)?;
    serde_json::to_writer(writer, manifest).map_err(|error| ManifestError::Malformed {
        detail: error.to_string(),
    })
}

fn validate(manifest: &LaunchManifest) -> Result<(), ManifestError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(ManifestError::UnsupportedVersion {
            found: manifest.format_version,
            supported: MANIFEST_FORMAT_VERSION,
        });
    }
    if manifest.launch.argv.first().is_none_or(String::is_empty) {
        return Err(ManifestError::InvalidField {
            field: "launch.argv",
            detail: "must contain a program".to_owned(),
        });
    }
    if !manifest.launch.cwd.is_absolute() {
        return Err(ManifestError::InvalidField {
            field: "launch.cwd",
            detail: "must be absolute".to_owned(),
        });
    }
    if manifest.launch.size.cols == 0 || manifest.launch.size.rows == 0 {
        return Err(ManifestError::InvalidField {
            field: "launch.size",
            detail: "dimensions must be non-zero".to_owned(),
        });
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

fn default_term() -> String {
    crate::engine::DEFAULT_TERMINAL.to_owned()
}
