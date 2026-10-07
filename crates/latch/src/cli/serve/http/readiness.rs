//! Gateway identity and private readiness document persistence.

use crate::cli::serve::contract::GatewayReadiness;
use crate::session::paths::{DIR_MODE, FILE_MODE};
use anyhow::Context;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path as FsPath;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn gateway_instance_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("gw-{:x}-{:x}", std::process::id(), nanos)
}

pub(super) fn write_readiness(path: &FsPath, readiness: &GatewayReadiness) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if parent.exists() {
            let mode = fs::metadata(parent)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                anyhow::bail!(
                    "refusing readiness directory {}: it must be owner-only",
                    parent.display()
                );
            }
        } else {
            fs::create_dir_all(parent).with_context(|| {
                format!("cannot create readiness directory {}", parent.display())
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE)).with_context(
                || format!("cannot tighten readiness directory {}", parent.display()),
            )?;
        }
    }
    let payload = serde_json::to_vec(readiness).context("cannot serialize gateway readiness")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot write readiness file {}", path.display()))?;
    file.write_all(&payload)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}
