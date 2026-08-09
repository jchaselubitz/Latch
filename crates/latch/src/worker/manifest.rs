//! The launch manifest: everything the worker needs, delivered over stdin.
//!
//! Launch material never travels in argv. `argv` is readable by any process on
//! the machine, and the material here includes the child's environment — which
//! for an agent session is where the API tokens are. It arrives on the worker's
//! stdin, lives in worker memory, and the only part of it that reaches disk is
//! the bounded display metadata in `meta.json`.
//!
//! The same manifest is what `latch create --manifest-file -` accepts, and what
//! M3's Overlord execution provider will send. Building it now rather than at M3
//! is what keeps secrets out of argv from the first day instead of after a
//! migration.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::worker::journal::DEFAULT_JOURNAL_CAP_BYTES;
use crate::worker::queue::{DEFAULT_QUEUE_CAPACITY_BYTES, DEFAULT_QUEUE_CAPACITY_CHUNKS};

/// Manifest schema version this build writes and accepts.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// Largest manifest this build will read from stdin, in bytes.
///
/// A manifest is bounded display metadata plus an environment block. Anything
/// larger is a mistake or an attack, and reading it unbounded would let a caller
/// exhaust worker memory before the child even starts.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Default interval a graceful stop is given before it is forced, in
/// milliseconds.
pub const DEFAULT_STOP_GRACE_MS: u64 = 5_000;

/// A terminal size that can be serialized.
///
/// Both leaf crates declare their own `Size` and neither may depend on the
/// other, so neither is available to carry serde derives across this boundary.
/// This type is the manifest's own, and converts to and from both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl TerminalSize {
    /// A size in columns and rows.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl From<TerminalSize> for latch_protocol::control::Size {
    fn from(value: TerminalSize) -> Self {
        Self {
            cols: value.cols,
            rows: value.rows,
        }
    }
}

impl From<latch_protocol::control::Size> for TerminalSize {
    fn from(value: latch_protocol::control::Size) -> Self {
        Self::new(value.cols, value.rows)
    }
}

impl From<TerminalSize> for latch_term::Size {
    fn from(value: TerminalSize) -> Self {
        Self::new(value.cols, value.rows)
    }
}

/// What to run, where, and with what.
///
/// None of this reaches `meta.json` except as the redacted command label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// The command and its arguments. Never written to disk.
    pub argv: Vec<String>,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Environment entries to set for the child. Never written to disk.
    ///
    /// A map rather than a list so a caller cannot smuggle in a duplicate key
    /// whose winner depends on iteration order.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whether the child also inherits the worker's environment.
    ///
    /// Defaults to true: a shell session that loses `PATH` is not a shell
    /// session. A caller that wants an isolated environment sets this false.
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    /// The session's initial size, before any controller attaches.
    pub size: TerminalSize,
    /// `TERM` for the child.
    #[serde(default = "default_term")]
    pub term: String,
}

/// Bounded metadata that may be shown to a human.
///
/// Everything here is sanitized to printable characters at ingest — here, where
/// it arrives — and not where it is rendered. A title flows into terminal
/// titles, `latch list`, and later web and mobile clients; sanitizing at render
/// makes every future call site a new chance to forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DisplayMetadata {
    /// Short name. Derived from cwd and command when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Human title, e.g. a mission title.
    #[serde(default)]
    pub title: Option<String>,
    /// Redacted command label. Derived from argv's program name when absent.
    #[serde(default)]
    pub command_label: Option<String>,
    /// Where the session came from.
    #[serde(default)]
    pub source: SourceInfo,
}

/// Provenance of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Broad category — `cli`, `desktop`, `api`.
    pub kind: String,
    /// An opaque identifier belonging to whoever asked for the session.
    ///
    /// Opaque on purpose: it is an identifier for correlation, never an
    /// authorization, and Latch does not interpret it. This is also the only
    /// field through which an integration such as Overlord appears in a session
    /// directory — no Overlord type crosses into `crates/`.
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

/// Worker bounds a caller may set.
///
/// Every one of these has a default that is right for a desk session; they are
/// adjustable because the failure modes they bound — a session that writes
/// gigabytes, a client on a bad link — are the ones that need to be reproducible
/// on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerTuning {
    /// Journal cap in bytes.
    #[serde(default = "default_journal_cap")]
    pub journal_cap_bytes: u64,
    /// Per-client queue cap in bytes.
    #[serde(default = "default_queue_bytes")]
    pub queue_capacity_bytes: usize,
    /// Per-client queue cap in chunks.
    #[serde(default = "default_queue_chunks")]
    pub queue_capacity_chunks: usize,
    /// Interval between the graceful signal and the forced one.
    #[serde(default = "default_stop_grace_ms")]
    pub stop_grace_ms: u64,
    /// Screen-model scrollback bound, in lines.
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    /// Scrollback lines sent with the snapshot when a client attaches.
    ///
    /// Distinct from `scrollback_lines`, which is what the session *retains*.
    /// This is what a reattaching client is *sent*, and on a phone it is paid
    /// on every app-backgrounding rather than once. See
    /// `docs/DECISION_SCROLLBACK.md`.
    #[serde(default = "default_attach_history_lines")]
    pub attach_history_lines: usize,
    /// Ceiling on the bytes of history sent with a snapshot on attach.
    #[serde(default = "default_attach_history_bytes")]
    pub attach_history_bytes: usize,
    /// The uid permitted to connect to the control socket.
    ///
    /// `None` means the worker's own effective uid, which is the only value a
    /// real caller ever wants. It is expressible so the rejection path is
    /// reachable in a test without a second user account.
    #[serde(default)]
    pub owner_uid: Option<u32>,
}

impl Default for WorkerTuning {
    fn default() -> Self {
        Self {
            journal_cap_bytes: DEFAULT_JOURNAL_CAP_BYTES,
            queue_capacity_bytes: DEFAULT_QUEUE_CAPACITY_BYTES,
            queue_capacity_chunks: DEFAULT_QUEUE_CAPACITY_CHUNKS,
            stop_grace_ms: DEFAULT_STOP_GRACE_MS,
            scrollback_lines: latch_term::DEFAULT_SCROLLBACK_LINES,
            attach_history_lines: latch_term::DEFAULT_ATTACH_HISTORY_LINES,
            attach_history_bytes: latch_term::DEFAULT_ATTACH_HISTORY_BYTES,
            owner_uid: None,
        }
    }
}

/// Everything a worker is told at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchManifest {
    /// Schema version. A manifest from a future version is rejected, not
    /// guessed at — the same rule the protocol applies to its own version.
    pub format_version: u32,
    /// What to run.
    pub launch: LaunchSpec,
    /// What to show a human.
    #[serde(default)]
    pub display: DisplayMetadata,
    /// What to bound.
    #[serde(default)]
    pub tuning: WorkerTuning,
}

impl LaunchManifest {
    /// A manifest for `argv` in `cwd` at `size`, with defaults everywhere else.
    pub fn new(argv: Vec<String>, cwd: impl Into<PathBuf>, size: TerminalSize) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            launch: LaunchSpec {
                argv,
                cwd: cwd.into(),
                env: BTreeMap::new(),
                inherit_env: true,
                size,
                term: default_term(),
            },
            display: DisplayMetadata::default(),
            tuning: WorkerTuning::default(),
        }
    }
}

/// Anything that can go wrong reading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The manifest could not be read from its source.
    #[error("cannot read launch manifest: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes are not a manifest.
    #[error("launch manifest is malformed: {detail}")]
    Malformed {
        /// Parser detail.
        detail: String,
    },
    /// The manifest declares a schema version this build does not read.
    #[error("launch manifest format version {found}; this build reads {supported}")]
    UnsupportedVersion {
        /// Version the manifest declared.
        found: u32,
        /// Version this build reads.
        supported: u32,
    },
    /// The manifest is structurally fine but cannot be acted on.
    #[error("launch manifest field `{field}` is unusable: {detail}")]
    InvalidField {
        /// Field at fault.
        field: &'static str,
        /// Why it cannot be used.
        detail: String,
    },
    /// The manifest exceeds [`MAX_MANIFEST_BYTES`].
    #[error("launch manifest exceeds {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded.
        limit: usize,
    },
}

/// Reads a manifest, refusing to buffer more than [`MAX_MANIFEST_BYTES`].
///
/// Validation happens here and not at use: an empty `argv`, a relative `cwd`, or
/// a zero dimension is rejected before a PTY is allocated, so a bad manifest
/// never produces a half-created session directory.
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
    let manifest: LaunchManifest =
        serde_json::from_slice(&bytes).map_err(|error| ManifestError::Malformed {
            detail: error.to_string(),
        })?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Writes a manifest in the form [`read`] accepts.
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
    if manifest.launch.argv.is_empty() || manifest.launch.argv[0].is_empty() {
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
    "xterm-256color".to_owned()
}

fn default_journal_cap() -> u64 {
    DEFAULT_JOURNAL_CAP_BYTES
}

fn default_queue_bytes() -> usize {
    DEFAULT_QUEUE_CAPACITY_BYTES
}

fn default_queue_chunks() -> usize {
    DEFAULT_QUEUE_CAPACITY_CHUNKS
}

fn default_stop_grace_ms() -> u64 {
    DEFAULT_STOP_GRACE_MS
}

fn default_scrollback_lines() -> usize {
    latch_term::DEFAULT_SCROLLBACK_LINES
}

fn default_attach_history_lines() -> usize {
    latch_term::DEFAULT_ATTACH_HISTORY_LINES
}

fn default_attach_history_bytes() -> usize {
    latch_term::DEFAULT_ATTACH_HISTORY_BYTES
}
