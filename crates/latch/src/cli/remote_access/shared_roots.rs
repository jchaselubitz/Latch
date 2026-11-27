//! Host-owner administration of durable, per-pairing read scopes.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

use super::devices::{ensure_private_directory, read_json_or_default, write_json};
use super::{control_grant_revision, random_hex};
use crate::cli::serve::{directory, shared_files};
use crate::session::paths::{LatchHome, FILE_MODE};

const MAX_GRANTS: usize = 128;

/// Local record of a root shared to one pairing revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedRootGrant {
    /// Opaque identifier the paired device can request.
    pub root_id: String,
    /// Local ID of the selected paired device.
    pub device_id: String,
    /// Canonical directory chosen by the host owner.
    pub path: PathBuf,
    pub(crate) grant_revision: u64,
    pub(crate) identity: shared_files::Identity,
}

fn store_path(home: &LatchHome) -> PathBuf {
    home.remote_access_dir().join("shared-roots.json")
}

fn lock_store(home: &LatchHome) -> anyhow::Result<File> {
    let dir = home.remote_access_dir();
    ensure_private_directory(&dir)?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("shared-roots.lock"))?;
    // SAFETY: the owned descriptor stays open for the caller's transaction.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error()).context("lock shared roots");
    }
    Ok(lock)
}

/// Share a specific directory with one currently authorized Control pairing.
pub fn share_root(
    home: &LatchHome,
    device_id: &str,
    path: &Path,
) -> anyhow::Result<SharedRootGrant> {
    let _lock = lock_store(home)?;
    let revision = control_grant_revision(home, device_id)
        .ok_or_else(|| anyhow!("device requires active Control access"))?;
    let path = directory::canonical_directory(path)
        .map_err(|_| anyhow!("shared root must be an accessible absolute directory"))?;
    let state_path = home.root().canonicalize()?;
    if path.starts_with(&state_path) || state_path.starts_with(&path) {
        bail!("a shared root cannot include or be inside Latch state");
    }
    let identity = shared_files::root_identity(&path).map_err(|_| {
        anyhow!("shared root is unavailable or blocked by the sensitive-path policy")
    })?;
    let mut grants = list_shared_roots(home)?;
    if let Some(existing) = grants.iter().find(|grant| {
        grant.device_id == device_id
            && grant.grant_revision == revision
            && grant.path == path
            && grant.identity == identity
    }) {
        return Ok(existing.clone());
    }
    if grants.len() >= MAX_GRANTS {
        bail!("shared root limit reached; revoke an unused share first");
    }
    let grant = SharedRootGrant {
        root_id: random_hex(32)?,
        device_id: device_id.to_owned(),
        path,
        grant_revision: revision,
        identity,
    };
    grants.push(grant.clone());
    write_json(&store_path(home), &grants)?;
    Ok(grant)
}

/// Local owner view, including old grants that can no longer be used remotely.
pub fn list_shared_roots(home: &LatchHome) -> anyhow::Result<Vec<SharedRootGrant>> {
    read_json_or_default(&store_path(home))
}

/// Remove a share. Re-sharing the same directory issues a different ID.
pub fn revoke_shared_root(home: &LatchHome, root_id: &str) -> anyhow::Result<()> {
    let _lock = lock_store(home)?;
    let mut grants = list_shared_roots(home)?;
    let count = grants.len();
    grants.retain(|grant| grant.root_id != root_id);
    if grants.len() == count {
        bail!("shared root not found");
    }
    write_json(&store_path(home), &grants)
}
