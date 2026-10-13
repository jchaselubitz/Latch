//! The Mac's list of repositories paired devices opened recently.
//!
//! It lives on the Mac so every phone paired with it sees the same list and no
//! phone keeps a record of the Mac's folders. Only canonical roots that
//! resolved as working trees at the time are ever written.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::session::paths::LatchHome;

/// How many repositories the Mac remembers.
pub(crate) const RECENT_LIMIT: usize = 20;

const FORMAT_VERSION: u8 = 1;

/// Serializes read-modify-write within this gateway. One gateway owns a
/// Latch home at a time, so this is the only writer.
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecentRepository {
    pub root: String,
    pub opened_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentFile {
    format_version: u8,
    repositories: Vec<RecentRepository>,
}

fn file(home: &LatchHome) -> PathBuf {
    home.root().join("git").join("recent-repositories.json")
}

/// Most recently opened first. A missing or unreadable file is an empty list:
/// the list is a convenience, never a reason for a Git route to fail.
pub(crate) fn load(home: &LatchHome) -> Vec<RecentRepository> {
    let Ok(bytes) = fs::read(file(home)) else {
        return Vec::new();
    };
    match serde_json::from_slice::<RecentFile>(&bytes) {
        Ok(parsed) if parsed.format_version == FORMAT_VERSION => parsed.repositories,
        _ => Vec::new(),
    }
}

/// Moves `root` to the front of the list, stamped `now`.
pub(crate) fn record(home: &LatchHome, root: &str, now: SystemTime) -> anyhow::Result<()> {
    let _guard = WRITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut repositories = load(home);
    repositories.retain(|entry| entry.root != root);
    repositories.insert(
        0,
        RecentRepository {
            root: root.to_owned(),
            opened_at: crate::engine::format_rfc3339(now),
        },
    );
    repositories.truncate(RECENT_LIMIT);
    let mut bytes = serde_json::to_vec_pretty(&RecentFile {
        format_version: FORMAT_VERSION,
        repositories,
    })?;
    bytes.push(b'\n');
    write_private_atomic(&file(home), &bytes)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("recent list has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let temporary = parent.join(format!(
        ".recent-repositories.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}
