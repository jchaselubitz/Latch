//! Where a session's socket lives, and the record that says so.
//!
//! Unix socket paths are limited to ~104 bytes on macOS, which a session
//! directory under a temp-dir `LATCH_HOME` exceeds. The socket therefore lives
//! in a short per-user directory — the same convention as tmux's
//! `/tmp/tmux-<uid>/` — and the session directory carries a small record
//! pointing at it.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Name of the record in a session directory.
pub const KERNEL_RECORD: &str = "kernel.json";
/// Name of the exit record in a session directory.
pub const EXIT_RECORD: &str = "exit.json";
/// Kernel name written into the record.
pub const KERNEL_NAME: &str = "latchd";

/// The session directory's pointer to its daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelRecord {
    /// Always [`KERNEL_NAME`].
    pub kernel: String,
    /// Socket path.
    pub socket: PathBuf,
    /// Daemon pid.
    pub pid: i32,
}

impl KernelRecord {
    /// Writes the record into `session_dir`.
    pub fn write(&self, session_dir: &Path) -> io::Result<()> {
        let path = session_dir.join(KERNEL_RECORD);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temp = session_dir.join(format!(".{KERNEL_RECORD}.tmp-{}", std::process::id()));
        fs::write(&temp, body)?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        fs::rename(temp, path)
    }

    /// Reads the record from `session_dir`, or `None` when there is none.
    pub fn read(session_dir: &Path) -> io::Result<Option<Self>> {
        match fs::read(session_dir.join(KERNEL_RECORD)) {
            Ok(body) => serde_json::from_slice(&body)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The per-user socket directory, created 0700 if needed.
pub fn socket_dir() -> io::Result<PathBuf> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let base = std::env::var_os("LATCHD_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/latchd-{uid}")));
    match fs::create_dir(&base) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::metadata(&base)?;
    if metadata.uid() != uid || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not owned by this user", base.display()),
        ));
    }
    Ok(base)
}

/// The socket path for session `id` under the Latch home rooted at `home`.
///
/// The home is folded into the name so two homes (a real one and a test
/// harness's) can hold the same id without meeting.
pub fn socket_path(home: &Path, id: &str) -> io::Result<PathBuf> {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in home.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(socket_dir()?.join(format!("{:08x}-{id}.sock", hash as u32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_are_short_and_home_specific() {
        let a = socket_path(Path::new("/Users/someone/.latch"), "ses_1").unwrap();
        let b = socket_path(Path::new("/tmp/other"), "ses_1").unwrap();
        assert_ne!(a, b);
        assert!(a.as_os_str().len() < 100, "{}", a.display());
    }

    #[test]
    fn kernel_record_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let record = KernelRecord {
            kernel: KERNEL_NAME.into(),
            socket: PathBuf::from("/tmp/x.sock"),
            pid: 42,
        };
        record.write(dir.path()).unwrap();
        assert_eq!(KernelRecord::read(dir.path()).unwrap(), Some(record));
        assert_eq!(
            KernelRecord::read(&dir.path().join("missing")).unwrap(),
            None
        );
    }
}
