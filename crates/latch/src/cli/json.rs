//! Stable `--json` response shapes.
//!
//! Overlord parses these. Asserting the schema — field names, types, and the
//! state vocabulary — is the contract; asserting "some JSON came out" is not.
//! Field names are camelCase where Overlord's examples are (`protocolVersion`,
//! `productVersion`) and snake_case where `meta.json` already is
//! (`command_label`, `created_at`), so a caller that already reads session
//! directories does not learn a second vocabulary for the same facts.
//!
//! These types exist so the suite can round-trip and so a future TypeScript
//! client has a named shape to mirror. Serialization is the implementation's
//! job; the tests assert the on-the-wire document.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::manifest::TerminalSize;
use crate::session::meta::ExitRecord;
use crate::session::timing::LaunchPhase;

/// One row of `latch list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session identifier.
    pub id: String,
    /// Short display name.
    pub name: String,
    /// Human title, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Derived state: creating | running | stopping | exited | lost.
    pub state: String,
    /// Working directory at spawn.
    pub cwd: PathBuf,
    /// Redacted command label.
    pub command_label: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last-activity timestamp, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Milliseconds since last activity, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    /// The conversation connector this session uses, or null when it has none
    /// and never will.
    ///
    /// Deliberately serialized even when `None`: absent means *this gateway
    /// predates the field*, null means *this session is a plain terminal*.
    /// A client that collapsed the two would read an older Latch as a fleet of
    /// connector-less sessions, so no `skip_serializing_if` here.
    #[serde(default)]
    pub connector: Option<String>,
}

/// `latch list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListReport {
    /// Sessions, most recently active first.
    pub sessions: Vec<SessionSummary>,
}

/// `latch inspect --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectReport {
    /// Session identifier.
    pub id: String,
    /// Short display name.
    pub name: String,
    /// Human title, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Derived state.
    pub state: String,
    /// Kernel that owns this session, independent of the caller's selector.
    pub kernel: String,
    /// Working directory at spawn.
    pub cwd: PathBuf,
    /// Redacted command label.
    pub command_label: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// Size at spawn.
    pub initial_size: TerminalSize,
    /// Current size, when the kernel reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
    /// Exit record, when the session has exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitRecord>,
    /// Whether one human terminal surface currently owns the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "surfaceAttached")]
    pub surface_attached: Option<bool>,
    /// Per-phase launch timings, oldest first, recorded by `create`, `open`,
    /// and the session's own launcher. Empty for sessions created before the
    /// sidecar existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_timings: Vec<LaunchPhase>,
    /// The conversation connector this session uses, or null when it has none.
    ///
    /// Same field, same rules, as `SessionSummary::connector`, so inspecting a
    /// session cannot contradict the list it came from.
    #[serde(default)]
    pub connector: Option<String>,
}

/// `latch stop --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    /// Session that was asked to stop.
    pub id: String,
    /// State after the request was delivered (typically `exited`, or `running`
    /// when the process survived SIGKILL).
    pub state: String,
    /// False when the pane was still live after the escalation window.
    ///
    /// Scripted callers should also treat a non-zero exit as failure; this
    /// field makes the same fact visible in the JSON document.
    pub stopped: bool,
}

/// `latch stop --all --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopAllReport {
    /// Every live session the command attempted to stop.
    pub sessions: Vec<StopReport>,
}

/// `latch remove SESSION --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    /// Session whose retained files were removed.
    pub id: String,
    /// True after the session directory has been removed.
    pub removed: bool,
}

/// `latch open SESSION --with VIEWER --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenReport {
    /// Session opened in the requested viewer.
    pub id: String,
    /// Viewer selected by the caller.
    pub viewer: String,
    /// Whether the operating system accepted the viewer-open request.
    pub opened: bool,
    /// True when the viewer had not appeared yet by the time this command
    /// returned. The request was accepted and the session is running; the
    /// window is simply still being presented. Defaulted on read so reports
    /// written before the field existed still decode.
    #[serde(default)]
    pub pending: bool,
    /// Shape the viewer was asked for: `new-window` or `new-tab`.
    ///
    /// Defaulted on read so reports written before the setting existed still
    /// decode.
    #[serde(default)]
    pub behavior: String,
}

/// `latch rename --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameReport {
    /// Session that was renamed.
    pub id: String,
    /// The new name, already sanitized at ingest.
    pub name: String,
}

/// `latch resize --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeReport {
    /// Session that was resized.
    pub id: String,
    /// Columns now in effect.
    pub cols: u16,
    /// Rows now in effect.
    pub rows: u16,
    /// Whether the size is pinned against controller changes.
    pub pinned: bool,
}

/// `latch prune --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// Session directories that were (or would be) reclaimed.
    pub reclaimed: Vec<String>,
    /// Exited sessions still inside the retention window, and when each leaves
    /// it.
    ///
    /// Reported because a skipped session is otherwise indistinguishable from a
    /// broken `prune`, and because the whole point of retaining one is that
    /// somebody might still want to look at it.
    #[serde(default)]
    pub retained: Vec<RetainedSession>,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

/// An exited session `prune` deliberately left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedSession {
    /// The session's identifier.
    pub id: String,
    /// How much longer it is kept, in milliseconds.
    pub reclaimable_in_ms: u64,
}

/// `latch update --json`.
///
/// One shape for all three outcomes so a caller can branch on `status` alone:
/// `current` when the install is the published version, `available` when a
/// check found a newer one, `installed` when this run replaced the binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    /// `current` | `available` | `installed`.
    pub status: String,
    /// Version that was running when the check was made.
    pub current_version: String,
    /// Newest published version, when the release feed was readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// Release page for the newest published version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_url: Option<String>,
    /// Binary that was replaced, present only for `installed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<PathBuf>,
}

/// One finding from `latch doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorFinding {
    /// Stable machine-readable code.
    pub code: String,
    /// Severity: `error` | `warning` | `info`.
    pub severity: String,
    /// Human-readable detail.
    pub message: String,
}

/// `latch doctor --json`.
///
/// Reports what is actually wrong, not a checklist of things that are fine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Session kernel (`latchd`).
    pub kernel: String,
    /// Version and protocol reported by the bundled headless kernel.
    #[serde(
        rename = "latchdVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub latchd_version: Option<String>,
    /// Findings. Empty means the installation looks sound.
    pub findings: Vec<DoctorFinding>,
}

/// `latch config --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReport {
    /// Preference key, when a single key was read or written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Value for that key, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Full config document when listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// `latch capabilities --json`.
///
/// Engine discovery for launch providers and the `/v2` gateway capabilities
/// document. It reports no conversation or interaction capability: those are
/// pushed as conversation state over the v2 WebSocket instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesReport {
    /// Wire protocol version this build speaks.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    /// Product version string.
    #[serde(rename = "productVersion")]
    pub product_version: String,
    /// Feature flags.
    pub capabilities: CapabilityFlags,
}

/// Feature flags inside [`CapabilitiesReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityFlags {
    /// Session creation is available.
    pub create: bool,
    /// Opening a preferred viewer is available.
    #[serde(rename = "openViewer")]
    pub open_viewer: bool,
    /// Local attach is available.
    #[serde(rename = "localAttach")]
    pub local_attach: bool,
    /// Cloud attach is available (M4).
    #[serde(rename = "cloudAttach")]
    pub cloud_attach: bool,
    /// `latch update` can replace this install in place.
    ///
    /// False for a build with no published release channel, and false for a
    /// copy something else owns — the desktop app reads this to decide whether
    /// offering to update the CLI would work.
    #[serde(rename = "selfUpdate")]
    pub self_update: bool,
    /// Extension identifiers this build supports.
    pub extensions: Vec<String>,
}

/// `latch create --manifest-file - --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateReport {
    /// Wire protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    /// The new session.
    pub session: CreatedSession,
}

/// The session object inside [`CreateReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedSession {
    /// Session identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Derived state at creation time.
    pub state: String,
    /// RFC 3339 creation timestamp.
    #[serde(rename = "createdAt")]
    pub created_at: String,
}
