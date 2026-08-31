//! Where a session's socket lives, and the record that says so.
//!
//! Unix socket paths are limited to ~104 bytes on macOS, which a session
//! directory under a temp-dir `LATCH_HOME` exceeds. The socket therefore lives
//! in a short per-user directory, and the session directory carries a small record
//! pointing at it.
//!
//! Everything here touches paths other users can reach (`/tmp`) or files a
//! session's own child could read, so the rules are: never follow a symlink
//! another user could have planted, never widen a permission after the fact,
//! and never build a path from an identifier that has not been validated.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Name of the record in a session directory.
pub const KERNEL_RECORD: &str = "kernel.json";
/// Name of the exit record in a session directory.
pub const EXIT_RECORD: &str = "exit.json";
/// Kernel name written into the record.
pub const KERNEL_NAME: &str = "latchd";
/// Longest session id accepted into a socket name.
pub const MAX_SESSION_ID_LEN: usize = 64;

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
        write_json(session_dir, KERNEL_RECORD, self)
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

/// Writes `value` as pretty JSON to `session_dir/name`, atomically and
/// owner-readable only.
///
/// The file is created with mode 0600 rather than chmod'ed afterwards, so
/// there is no instant at which it is readable by anyone else, and it is
/// renamed into place so a reader never sees a partial record.
pub fn write_json<T: Serialize>(session_dir: &Path, name: &str, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let path = session_dir.join(name);
    let temp = session_dir.join(format!(".{name}.tmp-{}", std::process::id()));
    // A stale temp file from a crashed predecessor would keep its old mode
    // through `create(true)`; start from nothing so the mode is ours.
    let _ = fs::remove_file(&temp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&body)?;
    drop(file);
    fs::rename(&temp, path)
}

/// Checks that `id` is safe to fold into a file name: non-empty, at most
/// [`MAX_SESSION_ID_LEN`] bytes, and only `[A-Za-z0-9_-]`.
///
/// The id comes from `latch`, which generates it, but the daemon also takes
/// it on its command line, and a path built from an unchecked id would let
/// `../` land the socket anywhere the user can write.
pub fn validate_session_id(id: &str) -> io::Result<()> {
    let well_formed = !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if well_formed {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("session id {id:?} must be 1-{MAX_SESSION_ID_LEN} characters of [A-Za-z0-9_-]"),
        ))
    }
}

/// The per-user socket directory, created 0700 if needed.
///
/// The directory sits in a world-writable `/tmp`, so it is treated as hostile
/// until proven otherwise: it must be a real directory (never a symlink)
/// owned by this user with no group or other permission bits. A directory
/// that fails the check is rejected, never repaired — a chmod through a path
/// another user planted would follow their symlink.
pub fn socket_dir() -> io::Result<PathBuf> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let base = std::env::var_os("LATCHD_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/latchd-{uid}")));
    socket_dir_at(&base, uid)
}

/// [`socket_dir`] for an explicit base and owner; the checkable core.
pub fn socket_dir_at(base: &Path, uid: u32) -> io::Result<PathBuf> {
    let c_path = CString::new(base.as_os_str().as_encoded_bytes())?;
    // mkdir applies the mode at creation (the umask can only remove bits), so
    // the directory is never more open than 0700 for even an instant.
    // SAFETY: mkdir reads a NUL-terminated path we own.
    if unsafe { libc::mkdir(c_path.as_ptr(), 0o700) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EEXIST) {
            return Err(error);
        }
    }
    // Open without following symlinks and inspect the descriptor itself, so
    // the thing checked is the thing used.
    // SAFETY: open reads a NUL-terminated path we own.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a plain directory: {error}", base.display()),
        ));
    }
    // SAFETY: the descriptor was just returned by open and is owned here.
    let dir = unsafe { File::from_raw_fd(fd) };
    let metadata = dir.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a directory", base.display()),
        ));
    }
    if metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not owned by this user", base.display()),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is accessible to other users (mode {:04o}); remove it or make it 0700",
                base.display(),
                metadata.mode() & 0o7777
            ),
        ));
    }
    Ok(base.to_path_buf())
}

/// The socket path for session `id` under the Latch home rooted at `home`.
///
/// The home is folded into the name so two homes (a real one and a test
/// harness's) can hold the same id without meeting.
pub fn socket_path(home: &Path, id: &str) -> io::Result<PathBuf> {
    validate_session_id(id)?;
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
    use std::os::unix::fs::PermissionsExt;

    fn uid() -> u32 {
        // SAFETY: getuid has no preconditions.
        unsafe { libc::getuid() }
    }

    #[test]
    fn socket_paths_are_short_and_home_specific() {
        let a = socket_path(Path::new("/Users/someone/.latch"), "ses_1").unwrap();
        let b = socket_path(Path::new("/tmp/other"), "ses_1").unwrap();
        assert_ne!(a, b);
        assert!(a.as_os_str().len() < 100, "{}", a.display());
    }

    #[test]
    fn socket_paths_reject_ids_that_could_escape_the_directory() {
        for bad in ["", "../x", "a/b", "ses 1", "ses\0", &"x".repeat(65)] {
            let error = socket_path(Path::new("/h"), bad).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        validate_session_id("ses_19ab-cd").unwrap();
    }

    #[test]
    fn socket_dir_is_created_private() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sockets");
        let dir = socket_dir_at(&base, uid()).unwrap();
        assert_eq!(dir, base);
        let mode = fs::metadata(&base).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // A second call on the existing directory is accepted.
        socket_dir_at(&base, uid()).unwrap();
    }

    #[test]
    fn socket_dir_rejects_a_symlink_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("victim");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.path().join("sockets");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = socket_dir_at(&link, uid()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        // The target was neither chmod'ed nor adopted.
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn socket_dir_rejects_group_or_world_access() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sockets");
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o750)).unwrap();
        let error = socket_dir_at(&base, uid()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        // It is not silently repaired either.
        assert_eq!(fs::metadata(&base).unwrap().mode() & 0o777, 0o750);
    }

    #[test]
    fn socket_dir_rejects_another_owner() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sockets");
        let error = socket_dir_at(&base, uid().wrapping_add(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }

    #[test]
    fn socket_dir_rejects_a_plain_file() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sockets");
        fs::write(&base, b"").unwrap();
        let error = socket_dir_at(&base, uid()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }

    #[test]
    fn kernel_record_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let record = KernelRecord {
            kernel: KERNEL_NAME.into(),
            socket: PathBuf::from("/tmp/x.sock"),
            pid: 42,
        };
        record.write(dir.path()).unwrap();
        assert_eq!(KernelRecord::read(dir.path()).unwrap(), Some(record));
        let mode = fs::metadata(dir.path().join(KERNEL_RECORD)).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            KernelRecord::read(&dir.path().join("missing")).unwrap(),
            None
        );
    }

    #[test]
    fn write_json_replaces_a_stale_temp_file_and_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir
            .path()
            .join(format!(".{EXIT_RECORD}.tmp-{}", std::process::id()));
        fs::write(&stale, b"old").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();
        write_json(dir.path(), EXIT_RECORD, &serde_json::json!({"ok": true})).unwrap();
        let mode = fs::metadata(dir.path().join(EXIT_RECORD)).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!stale.exists());
    }
}
