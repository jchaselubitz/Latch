//! Durable, device-scoped receipts for Git mutations.
//!
//! A receipt is claimed (written as pending, flushed to disk) before Git runs
//! and rewritten when the outcome is known. Each device's receipts live in
//! their own folder under the Latch home, named by the opaque id the paired
//! proxy proved, so one device can neither read another's receipt nor learn
//! that a request id was used by someone else.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::ops::Mutation;
use crate::cli::serve::contract::GitMutationReceipt;
use crate::session::paths::{LatchHome, FILE_MODE};

const FORMAT_VERSION: u8 = 1;

/// What the Mac knows about one request id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Stored {
    pub format_version: u8,
    /// Opaque device the paired proxy proved, or `None` for a local client.
    pub device: Option<String>,
    /// Digest of the request's arguments. Reusing the id with any other
    /// arguments is refused.
    pub digest: String,
    pub mutation: Mutation,
    /// Recorded under the worktree lock just before Git runs. `None` means
    /// Git was never started for this request.
    pub before: Option<Before>,
    pub receipt: GitMutationReceipt,
}

/// The repository as the Mac found it immediately before running Git.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Before {
    pub head: Option<String>,
    pub branch: Option<String>,
    /// Hex path bytes the operation was about to act on.
    pub targets: Vec<String>,
    /// The commit an update moved toward, or the upstream's commit before a
    /// fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// FETCH_HEAD's time before a fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_head: Option<String>,
    /// The merge or rebase in progress when Continue or Abort started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<crate::cli::serve::contract::GitOperation>,
    /// The push destination's commit as last fetched, when the push started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

impl Stored {
    pub fn new(
        device: Option<String>,
        digest: String,
        mutation: Mutation,
        receipt: GitMutationReceipt,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            device,
            digest,
            mutation,
            before: None,
            receipt,
        }
    }
}

pub(crate) enum Claim {
    /// This request now owns the id; Git may run.
    Claimed,
    /// The id was already used by this device.
    Exists(Box<Stored>),
}

fn device_dir(home: &LatchHome, device: Option<&str>) -> PathBuf {
    let scope = match device {
        // The gateway accepts only `[A-Za-z0-9_-]{1,96}` as a device id.
        Some(device) => format!("device-{device}"),
        None => "local".to_owned(),
    };
    home.root().join("git").join("receipts").join(scope)
}

fn file(home: &LatchHome, device: Option<&str>, request_id: &str) -> PathBuf {
    device_dir(home, device).join(format!("{}.json", request_id.to_ascii_lowercase()))
}

fn pid_file(home: &LatchHome, device: Option<&str>, request_id: &str) -> PathBuf {
    device_dir(home, device).join(format!("{}.pid", request_id.to_ascii_lowercase()))
}

/// Records the process (and process group) Git runs as for a request, so a
/// gateway that restarts can tell whether that Git might still be running.
pub(crate) fn record_pid(home: &LatchHome, device: Option<&str>, request_id: &str, pid: u32) {
    let path = pid_file(home, device, request_id);
    if let Ok(mut out) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        let _ = out.write_all(pid.to_string().as_bytes());
        let _ = out.sync_all();
    }
}

pub(crate) fn clear_pid(home: &LatchHome, device: Option<&str>, request_id: &str) {
    // Preserve affirmative evidence that the whole process group stopped.
    // A missing file can mean a crash between spawn and recording the PID.
    let path = pid_file(home, device, request_id);
    if let Ok(mut out) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        let _ = out.write_all(b"stopped");
        let _ = out.sync_all();
    }
}

/// Whether Git, or anything it started, might still be running for this
/// request. `true` only when a recorded process group still exists; a
/// reused process id can only make this answer "might", never "gone".
pub(crate) fn git_may_be_running(home: &LatchHome, device: Option<&str>, request_id: &str) -> bool {
    let Ok(text) = fs::read_to_string(pid_file(home, device, request_id)) else {
        return true;
    };
    if text == "stopped" {
        return false;
    }
    let Ok(pid) = text.trim().parse::<i32>() else {
        return true;
    };
    if pid <= 1 {
        return true;
    }
    // SAFETY: signal 0 only checks that the group exists and may be
    // signalled; nothing is delivered.
    let alive = unsafe { libc::killpg(pid, 0) };
    alive == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// When the receipt file was last written: for a settled receipt, when it
/// was settled.
pub(crate) fn written_at(
    home: &LatchHome,
    device: Option<&str>,
    request_id: &str,
) -> Option<SystemTime> {
    fs::metadata(file(home, device, request_id))
        .ok()?
        .modified()
        .ok()
}

/// This device's receipt for `request_id`, if it was ever accepted. An
/// unreadable file is an error, never "not found": the id was used.
pub(crate) fn load(
    home: &LatchHome,
    device: Option<&str>,
    request_id: &str,
) -> anyhow::Result<Option<Stored>> {
    let path = file(home, device, request_id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let stored: Stored = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        stored.format_version == FORMAT_VERSION,
        "unknown receipt format"
    );
    // The folder already scopes the device; this is a second check that a
    // copied or misplaced file is never served to the wrong one.
    anyhow::ensure!(
        stored.device.as_deref() == device,
        "receipt belongs to another device"
    );
    Ok(Some(stored))
}

/// Writes `stored` as this request id's receipt only if none exists yet.
/// The file is complete and flushed before it appears under its name, so a
/// crash leaves either no receipt or a whole pending one.
pub(crate) fn claim(home: &LatchHome, stored: &Stored) -> anyhow::Result<Claim> {
    let device = stored.device.as_deref();
    let dir = ensure_dir(&device_dir(home, device))?;
    let path = file(home, device, &stored.receipt.request_id);
    let temporary = write_temporary(&dir, stored)?;
    let linked = fs::hard_link(&temporary, &path);
    let _ = fs::remove_file(&temporary);
    match linked {
        Ok(()) => {
            sync_dir(&dir);
            Ok(Claim::Claimed)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match load(home, device, &stored.receipt.request_id)? {
                Some(existing) => Ok(Claim::Exists(Box::new(existing))),
                None => anyhow::bail!("receipt vanished while claiming"),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Replaces an already-claimed receipt.
pub(crate) fn save(home: &LatchHome, stored: &Stored) -> anyhow::Result<()> {
    let device = stored.device.as_deref();
    let dir = ensure_dir(&device_dir(home, device))?;
    let path = file(home, device, &stored.receipt.request_id);
    let temporary = write_temporary(&dir, stored)?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_dir(&dir);
    Ok(())
}

fn ensure_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(crate::session::paths::DIR_MODE)
        .create(dir)?;
    Ok(dir.to_owned())
}

fn write_temporary(dir: &Path, stored: &Stored) -> anyhow::Result<PathBuf> {
    let temporary = dir.join(format!(
        ".{}.{}.{}.tmp",
        stored.receipt.request_id.to_ascii_lowercase(),
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    let mut bytes = serde_json::to_vec_pretty(stored)?;
    bytes.push(b'\n');
    let written = out.write_all(&bytes).and_then(|()| out.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(temporary)
}

/// Flushes the folder entry so a new or renamed receipt survives a crash.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

// Do not delete accepted request IDs. A delayed client can replay any old
// request after losing its response; forgetting it would permit a second
// effect. A future compactor must retain durable deduplication tombstones.
