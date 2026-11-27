//! Read-only Files operations for explicit, per-pairing shared roots.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contract::{
    SharedDirectoryEntry, SharedDirectoryPage, SharedFileKind, SharedFilePreview, SharedRoot,
    SharedRoots,
};
use super::directory::MAX_PATH_BYTES;
use crate::cli::remote_access::{
    control_grant_revision, list_shared_roots, random_hex, SharedRootGrant,
};
use crate::session::paths::LatchHome;

const MAX_COMPONENTS: usize = 64;
const MAX_NAME_BYTES: usize = 1024;
const PAGE_SIZE: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 8192;
const MAX_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_CURSORS: usize = 512;
const CURSOR_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("an active paired device with Control is required")]
    Denied,
    #[error("this shared root is unavailable")]
    RootUnavailable,
    #[error("use a valid root-relative path")]
    InvalidPath,
    #[error("this entry is unavailable for shared reads")]
    EntryUnavailable,
    #[error("the entry or directory changed; reload before reading it again")]
    Changed,
    #[error("the directory page expired or changed; reload the first page")]
    StaleCursor,
    #[error("this directory exceeds the listing limit")]
    DirectoryTooLarge,
    #[error("the gateway could not read shared files")]
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    created: Option<SystemTime>,
}

impl From<&Metadata> for Identity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().ok(),
        }
    }
}

fn same_snapshot(before: &Metadata, after: &Metadata) -> bool {
    Identity::from(before) == Identity::from(after)
        && before.nlink() == after.nlink()
        && before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && (before.ctime(), before.ctime_nsec()) == (after.ctime(), after.ctime_nsec())
}

fn allowed_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.starts_with('.')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return false;
    }
    let name = name.to_ascii_lowercase();
    !matches!(
        name.as_str(),
        "credentials" | "secrets" | "keychains" | "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
    ) && ![
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        ".ppk",
        ".kdbx",
        ".keychain",
        ".keychain-db",
        ".tfstate",
        ".tfstate.backup",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
        && ![
            "credentials.",
            "secrets.",
            "id_rsa.",
            "id_dsa.",
            "id_ecdsa.",
            "id_ed25519.",
            "ssh_host_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The kernel resolves the entire path beneath the supplied descriptor in
/// one call. No component may be a symlink, including the final component.
fn open_beneath(parent: &File, path: &str, directory: bool, same_mount: bool) -> io::Result<File> {
    let path = CString::new(path)?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: path is NUL terminated and parent owns its live descriptor.
    let fd = unsafe {
        // Darwin's sys/fcntl.h defines this flag. libc exposes the FreeBSD
        // value under the same name, but does not export Darwin's value.
        const O_RESOLVE_BENEATH: libc::c_int = 0x00001000;
        let _ = same_mount; // Darwin checks the opened objects' device below.
        libc::openat(
            parent.as_raw_fd(),
            path.as_ptr(),
            flags | O_RESOLVE_BENEATH | libc::O_NOFOLLOW_ANY,
        )
    };
    #[cfg(target_os = "linux")]
    let fd = {
        // SAFETY: the openat2 UAPI requires a zero-initialized open_how.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_NOFOLLOW) as u64;
        how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;
        if same_mount {
            how.resolve |= libc::RESOLVE_NO_XDEV;
        }
        // SAFETY: path and how remain live for the duration of the syscall.
        let result = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                parent.as_raw_fd(),
                path.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        result as libc::c_int
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let fd = {
        let _ = (parent, path, same_mount, flags);
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the open returned a fresh descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(crate) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let Ok(root) = File::open("/") else {
            return false;
        };
        let Ok(opened) = open_beneath(&root, ".", true, false) else {
            return false;
        };
        descriptor_path(&opened).is_ok() && open_beneath(&root, "/", true, false).is_err()
    })
}

fn descriptor_path(file: &File) -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut path = [0; libc::MAXPATHLEN as usize];
        // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes to the buffer.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let path = CStr::from_bytes_until_nul(&path).map_err(io::Error::other)?;
        Ok(std::ffi::OsString::from_vec(path.to_bytes().to_vec()).into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn check_location(file: &File, path: &Path) -> Result<(), Error> {
    if descriptor_path(file).as_deref().ok() != Some(path) {
        return Err(Error::Changed);
    }
    Ok(())
}

fn open_root(path: &Path) -> Result<File, Error> {
    let mut components = path.components();
    if !supported()
        || components.next() != Some(std::path::Component::RootDir)
        || path == Path::new("/")
    {
        return Err(Error::RootUnavailable);
    }
    let system_paths = ["/dev", "/proc", "/sys", "/etc", "/private/etc"];
    if system_paths.iter().any(|system| {
        let system = Path::new(system);
        path.starts_with(system) || system.starts_with(path)
    }) {
        return Err(Error::RootUnavailable);
    }
    for component in components {
        let Some(name) = component.as_os_str().to_str() else {
            return Err(Error::RootUnavailable);
        };
        let sensitive = matches!(
            name.to_ascii_lowercase().as_str(),
            ".git"
                | ".ssh"
                | ".gnupg"
                | ".aws"
                | ".kube"
                | ".latch"
                | ".config"
                | ".codex"
                | ".claude"
                | ".password-store"
                | ".azure"
                | ".docker"
        );
        if sensitive || (!name.starts_with('.') && !allowed_name(name)) {
            return Err(Error::RootUnavailable);
        }
    }
    if !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(allowed_name)
    {
        return Err(Error::RootUnavailable);
    }
    let relative = path
        .to_str()
        .and_then(|path| path.strip_prefix('/'))
        .ok_or(Error::RootUnavailable)?;
    let filesystem_root = File::open("/").map_err(|_| Error::RootUnavailable)?;
    let dir = open_beneath(&filesystem_root, relative, true, false)
        .map_err(|_| Error::RootUnavailable)?;
    check_location(&dir, path).map_err(|_| Error::RootUnavailable)?;
    Ok(dir)
}

pub(crate) fn root_identity(path: &Path) -> Result<Identity, Error> {
    let metadata = open_root(path)?
        .metadata()
        .map_err(|_| Error::RootUnavailable)?;
    Ok(Identity::from(&metadata))
}

fn relative_parts(path: &str) -> Result<Vec<&str>, Error> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(Error::InvalidPath);
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() > MAX_COMPONENTS
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(Error::InvalidPath);
    }
    if parts.iter().any(|part| !allowed_name(part)) {
        return Err(Error::EntryUnavailable);
    }
    Ok(parts)
}

fn stat_at(parent_fd: libc::c_int, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::uninit();
    // SAFETY: name and parent are live. A successful fstatat initializes stat.
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

fn stat_device(stat: &libc::stat) -> u64 {
    #[cfg(target_os = "linux")]
    return stat.st_dev;
    #[cfg(not(target_os = "linux"))]
    return stat.st_dev as u64;
}

fn secure_file(stat: &libc::stat, device: u64) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
        && stat.st_nlink == 1
        && stat_device(stat) == device
}

fn open_relative(grant: &SharedRootGrant, parts: &[&str], directory: bool) -> Result<File, Error> {
    let root = open_root(&grant.path)?;
    if Identity::from(&root.metadata().map_err(|_| Error::RootUnavailable)?) != grant.identity {
        return Err(Error::RootUnavailable);
    }
    if parts.is_empty() {
        return if directory {
            Ok(root)
        } else {
            Err(Error::InvalidPath)
        };
    }

    let relative = parts.join("/");
    let expected_file = if directory {
        None
    } else {
        let (name, parents) = parts.split_last().ok_or(Error::InvalidPath)?;
        let parent = if parents.is_empty() {
            None
        } else {
            let parent_path = parents.join("/");
            let parent = open_beneath(&root, &parent_path, true, true)
                .map_err(|_| Error::EntryUnavailable)?;
            check_location(&parent, &grant.path.join(parent_path))?;
            Some(parent)
        };
        let name = CString::new(*name).map_err(|_| Error::InvalidPath)?;
        let parent_fd = parent.as_ref().unwrap_or(&root).as_raw_fd();
        let stat = stat_at(parent_fd, &name).map_err(|_| Error::EntryUnavailable)?;
        if !secure_file(&stat, grant.identity.device) {
            return Err(Error::EntryUnavailable);
        }
        Some(stat)
    };

    let file =
        open_beneath(&root, &relative, directory, true).map_err(|_| Error::EntryUnavailable)?;
    let metadata = file.metadata().map_err(|_| Error::EntryUnavailable)?;
    if metadata.dev() != grant.identity.device
        || expected_file.is_some_and(|stat| {
            !metadata.is_file() || metadata.nlink() != 1 || metadata.ino() != stat.st_ino
        })
    {
        return Err(Error::EntryUnavailable);
    }
    check_location(&root, &grant.path)?;
    check_location(&file, &grant.path.join(relative))?;
    Ok(file)
}

/// Compare what is still named by the authorized path with the open object.
fn validate_open(
    grant: &SharedRootGrant,
    parts: &[&str],
    file: &File,
    before: &Metadata,
    directory: bool,
) -> Result<(), Error> {
    let expected = parts
        .iter()
        .fold(grant.path.clone(), |path, name| path.join(name));
    check_location(file, &expected)?;
    let named = open_relative(grant, parts, directory).map_err(|_| Error::Changed)?;
    let current = named.metadata().map_err(|_| Error::Changed)?;
    if !same_snapshot(before, &current)
        || !same_snapshot(before, &file.metadata().map_err(|_| Error::Changed)?)
    {
        return Err(Error::Changed);
    }
    Ok(())
}

fn authorized_roots(home: &LatchHome, device: &str) -> Result<Vec<SharedRootGrant>, Error> {
    let revision = control_grant_revision(home, device).ok_or(Error::Denied)?;
    let mut roots = list_shared_roots(home).map_err(|_| Error::Failed)?;
    roots.retain(|root| root.device_id == device && root.grant_revision == revision);
    Ok(roots)
}

fn grant(home: &LatchHome, device: &str, root_id: &str) -> Result<SharedRootGrant, Error> {
    authorized_roots(home, device)?
        .into_iter()
        .find(|grant| grant.root_id == root_id)
        .ok_or(Error::RootUnavailable)
}

fn validate_grant(home: &LatchHome, device: &str, expected: &SharedRootGrant) -> Result<(), Error> {
    if grant(home, device, &expected.root_id)? != *expected {
        return Err(Error::RootUnavailable);
    }
    Ok(())
}

pub(crate) fn roots(home: &LatchHome, device: &str) -> Result<SharedRoots, Error> {
    let mut visible = Vec::new();
    for root in authorized_roots(home, device)? {
        if root_identity(&root.path).ok().as_ref() != Some(&root.identity) {
            continue;
        }
        if let Some(name) = root.path.file_name().and_then(|name| name.to_str()) {
            visible.push(SharedRoot {
                root_id: root.root_id,
                name: name.to_owned(),
            });
        }
    }
    // Take one fresh authorization snapshot after filesystem work.
    let latest = authorized_roots(home, device)?;
    visible.retain(|root| latest.iter().any(|grant| grant.root_id == root.root_id));
    Ok(SharedRoots { roots: visible })
}

/// Owns the fdopendir stream. readdir returns a borrowed entry until the next read.
struct DirectoryStream(*mut libc::DIR);

impl DirectoryStream {
    fn new(file: &File) -> Result<Self, Error> {
        // Open an independent directory offset, and let fdopendir own the fd.
        let dir = open_beneath(file, ".", true, true).map_err(|_| Error::EntryUnavailable)?;
        // SAFETY: dir owns a live directory descriptor.
        let stream = unsafe { libc::fdopendir(dir.as_raw_fd()) };
        if stream.is_null() {
            return Err(Error::EntryUnavailable);
        }
        std::mem::forget(dir);
        Ok(Self(stream))
    }

    fn next(&mut self) -> Result<Option<&CStr>, Error> {
        #[cfg(target_os = "macos")]
        // SAFETY: errno is thread-local and readdir is called synchronously.
        let errno = unsafe { libc::__error() };
        #[cfg(target_os = "linux")]
        // SAFETY: errno is thread-local and readdir is called synchronously.
        let errno = unsafe { libc::__errno_location() };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let errno = return Err(Error::Failed);
        // SAFETY: this stream is exclusively borrowed and the returned name
        // cannot outlive this borrow or another call to readdir.
        unsafe {
            *errno = 0;
            let entry = libc::readdir(self.0);
            if entry.is_null() {
                return if *errno == 0 {
                    Ok(None)
                } else {
                    Err(Error::EntryUnavailable)
                };
            }
            Ok(Some(CStr::from_ptr((*entry).d_name.as_ptr())))
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the stream.
        unsafe { libc::closedir(self.0) };
    }
}

fn scan(file: &File, device: u64) -> Result<Vec<SharedDirectoryEntry>, Error> {
    let mut stream = DirectoryStream::new(file)?;
    let mut entries = Vec::new();
    for index in 0..=MAX_DIRECTORY_ENTRIES + 2 {
        let Some(raw_name) = stream.next()? else {
            entries.sort_unstable_by(|a: &SharedDirectoryEntry, b| a.name.cmp(&b.name));
            return Ok(entries);
        };
        if index == MAX_DIRECTORY_ENTRIES + 2 {
            break;
        }
        let Ok(name) = raw_name.to_str() else {
            continue;
        };
        if !allowed_name(name) {
            continue;
        }
        let Ok(stat) = stat_at(file.as_raw_fd(), raw_name) else {
            continue;
        };
        if stat_device(&stat) != device {
            continue;
        }
        let kind = if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            SharedFileKind::Directory
        } else if secure_file(&stat, device) {
            SharedFileKind::File
        } else {
            continue;
        };
        entries.push(SharedDirectoryEntry {
            name: name.to_owned(),
            kind,
        });
    }
    Err(Error::DirectoryTooLarge)
}

struct Cursor {
    token: String,
    root_id: String,
    path: String,
    fingerprint: [u8; 32],
    offset: usize,
    created: Instant,
}

#[derive(Clone, Default)]
pub(crate) struct Cursors(Arc<Mutex<VecDeque<Cursor>>>);

impl Cursors {
    pub(crate) fn page(
        &self,
        home: &LatchHome,
        device: &str,
        root_id: &str,
        path: String,
        cursor: Option<&str>,
    ) -> Result<SharedDirectoryPage, Error> {
        let grant = grant(home, device, root_id)?;
        let parts = relative_parts(&path)?;
        let continuation = cursor
            .map(|token| {
                self.0
                    .lock()
                    .map_err(|_| Error::Failed)?
                    .iter()
                    .find(|cursor| {
                        cursor.token == token
                            && cursor.root_id == root_id
                            && cursor.path == path
                            && cursor.created.elapsed() < CURSOR_TTL
                    })
                    .map(|cursor| (cursor.fingerprint, cursor.offset, cursor.created))
                    .ok_or(Error::StaleCursor)
            })
            .transpose()?;
        let file = match open_relative(&grant, &parts, true) {
            Err(Error::EntryUnavailable | Error::Changed) if continuation.is_some() => {
                return Err(Error::StaleCursor);
            }
            result => result?,
        };
        let before = file.metadata().map_err(|_| Error::EntryUnavailable)?;
        let entries = scan(&file, grant.identity.device)?;
        let validation = validate_open(&grant, &parts, &file, &before, true);
        if continuation.is_some() && validation.is_err() {
            return Err(Error::StaleCursor);
        }
        validation?;

        let mut digest = Sha256::new();
        digest.update(before.ino().to_le_bytes());
        digest.update(before.ctime().to_le_bytes());
        digest.update(before.ctime_nsec().to_le_bytes());
        for entry in &entries {
            digest.update(entry.name.as_bytes());
            digest.update([0, u8::from(entry.kind == SharedFileKind::Directory)]);
        }
        let fingerprint = digest.finalize().into();
        let offset = match continuation {
            Some((expected, offset, created))
                if fingerprint == expected
                    && created.elapsed() < CURSOR_TTL
                    && offset < entries.len() =>
            {
                offset
            }
            Some(_) => return Err(Error::StaleCursor),
            None => 0,
        };
        validate_grant(home, device, &grant)?;
        let end = entries.len().min(offset + PAGE_SIZE);
        let next_cursor = if end < entries.len() {
            let token = random_hex(32).map_err(|_| Error::Failed)?;
            let mut cursors = self.0.lock().map_err(|_| Error::Failed)?;
            cursors.retain(|cursor| cursor.created.elapsed() < CURSOR_TTL);
            if cursors.len() >= MAX_CURSORS {
                cursors.pop_front();
            }
            cursors.push_back(Cursor {
                token: token.clone(),
                root_id: root_id.to_owned(),
                path: path.clone(),
                fingerprint,
                offset: end,
                created: Instant::now(),
            });
            Some(token)
        } else {
            None
        };
        Ok(SharedDirectoryPage {
            path,
            entries: entries[offset..end].to_vec(),
            next_cursor,
        })
    }
}

pub(crate) fn preview(
    home: &LatchHome,
    device: &str,
    root_id: &str,
    path: &str,
) -> Result<SharedFilePreview, Error> {
    let grant = grant(home, device, root_id)?;
    let parts = relative_parts(path)?;
    let mut file = open_relative(&grant, &parts, false)?;
    let before = file.metadata().map_err(|_| Error::EntryUnavailable)?;
    validate_open(&grant, &parts, &file, &before, false)?;
    let mut bytes = Vec::with_capacity((before.len().min(MAX_PREVIEW_BYTES as u64 + 1)) as usize);
    (&mut file)
        .take((MAX_PREVIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::EntryUnavailable)?;
    validate_open(&grant, &parts, &file, &before, false)?;
    let truncated = bytes.len() > MAX_PREVIEW_BYTES;
    bytes.truncate(MAX_PREVIEW_BYTES);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => Some(text),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
    .filter(|text| {
        !text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n'))
    });
    validate_grant(home, device, &grant)?;
    Ok(match (text, truncated) {
        (Some(text), false) => SharedFilePreview::Text {
            text: text.to_owned(),
        },
        (Some(text), true) => SharedFilePreview::Truncated {
            text: text.to_owned(),
        },
        (None, _) => SharedFilePreview::Binary,
    })
}

#[cfg(test)]
mod tests;
