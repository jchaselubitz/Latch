//! Workspace file handoff.
//!
//! A phone cannot hand an agent a file directly: the agent reads its working
//! directory, not the conversation channel. So the phone uploads the file
//! here, the gateway places it under the session's working directory in one
//! dedicated folder, and the message the phone sends next names it by path.
//!
//! What the caller controls is deliberately narrow. The directory is the
//! session's own recorded working directory, never a path from the request.
//! The folder is fixed. The file name is a suggestion, reduced to a safe
//! alphabet and made unique by the gateway, and the file is created with
//! `O_EXCL | O_NOFOLLOW` relative to a directory descriptor, so neither an
//! existing file nor a symlink planted in the folder can redirect the write.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The folder, under the session's working directory, that uploads land in.
/// Hidden so it stays out of the way in a listing, and ignored by git through
/// the `.gitignore` the gateway writes into it.
pub(crate) const ATTACHMENTS_DIR: &str = ".latch-attachments";

/// Longest file stem the gateway keeps, in bytes of the safe alphabet.
const MAX_STEM: usize = 64;
/// Longest extension kept; anything longer is not an extension a person typed.
const MAX_EXTENSION: usize = 12;
/// Suffixes tried before the gateway gives up finding a free name.
const MAX_NAME_ATTEMPTS: u32 = 1_000;
/// Created files are readable by the Mac user who runs the agent, and nobody else.
const ATTACHMENT_MODE: libc::c_uint = 0o600;
const ATTACHMENTS_DIR_MODE: libc::mode_t = 0o700;

/// The answer the route returns: where the file landed, for the message.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentReceipt {
    /// Absolute path on the Mac, which the agent can open from any directory.
    pub path: String,
    /// Path relative to the session's working directory.
    pub relative_path: String,
    /// The final file name the gateway chose.
    pub name: String,
    /// Bytes written.
    pub bytes: u64,
}

#[derive(Debug)]
pub(crate) enum AttachmentError {
    /// The session's working directory is gone or is not a directory.
    WorkspaceUnavailable,
    /// The attachments folder exists but is not a real directory — a symlink
    /// or a file planted where the folder should be.
    UnsafeFolder,
    /// No free name was found after `MAX_NAME_ATTEMPTS` suffixes.
    NameExhausted,
    Io(io::Error),
}

impl From<io::Error> for AttachmentError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reduces a caller-suggested name to `[A-Za-z0-9._-]`, keeping a short
/// lowercased extension. Never empty, never a dotfile, never a path.
pub(crate) fn sanitize_file_name(raw: Option<&str>) -> (String, Option<String>) {
    let raw = raw.unwrap_or_default();
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut mapped = String::with_capacity(last.len());
    for character in last.chars() {
        let next = if character.is_ascii_alphanumeric() || character == '_' {
            character
        } else if character == '.' {
            '.'
        } else {
            '-'
        };
        // Collapse runs, so `a..b` and `a  b` cannot survive as `..` or `--`.
        if (next == '.' || next == '-') && mapped.ends_with(next) {
            continue;
        }
        mapped.push(next);
    }
    let trimmed = mapped.trim_matches(|character| character == '.' || character == '-');
    let (stem, extension) = match trimmed.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= MAX_EXTENSION
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()) =>
        {
            (stem, Some(extension.to_ascii_lowercase()))
        }
        _ => (trimmed, None),
    };
    let stem =
        truncate(stem, MAX_STEM).trim_matches(|character| character == '.' || character == '-');
    let stem = if stem.is_empty() { "attachment" } else { stem };
    (stem.to_owned(), extension)
}

fn truncate(text: &str, max: usize) -> &str {
    // The alphabet is ASCII, so a byte index is a character boundary.
    &text[..text.len().min(max)]
}

fn candidate(stem: &str, extension: Option<&str>, attempt: u32) -> String {
    let stem = if attempt == 0 {
        stem.to_owned()
    } else {
        format!("{stem}-{}", attempt + 1)
    };
    match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem,
    }
}

/// A new, empty attachment file, removed again unless `commit` is called.
///
/// The handler writes the body into `file` as it arrives. A client that goes
/// away mid-upload drops the handler future, and with it this value, so a
/// half-written file never stays behind for an agent to find.
pub(crate) struct PendingAttachment {
    pub file: File,
    folder: OwnedFd,
    name: String,
    workspace: PathBuf,
    committed: bool,
}

impl PendingAttachment {
    /// Keeps the file and describes where it is.
    pub(crate) fn commit(mut self, bytes: u64) -> AttachmentReceipt {
        self.committed = true;
        let relative = format!("{ATTACHMENTS_DIR}/{}", self.name);
        AttachmentReceipt {
            path: self
                .workspace
                .join(&relative)
                .to_string_lossy()
                .into_owned(),
            relative_path: relative,
            name: self.name.clone(),
            bytes,
        }
    }
}

impl Drop for PendingAttachment {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(name) = CString::new(self.name.as_bytes()) {
            // SAFETY: `folder` is an open directory descriptor owned by self and
            // `name` is a NUL-terminated single path component.
            unsafe {
                libc::unlinkat(self.folder.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }
}

/// Creates a uniquely named, empty file in the attachments folder under
/// `workspace`, creating the folder on first use.
pub(crate) fn create(
    workspace: &Path,
    suggested: Option<&str>,
) -> Result<PendingAttachment, AttachmentError> {
    let workspace = workspace
        .canonicalize()
        .map_err(|_| AttachmentError::WorkspaceUnavailable)?;
    let root = open_directory(None, &workspace, false)
        .map_err(|_| AttachmentError::WorkspaceUnavailable)?;
    let folder_name = c_name(ATTACHMENTS_DIR)?;
    // SAFETY: `root` is an open directory descriptor; the name is one component.
    let made =
        unsafe { libc::mkdirat(root.as_raw_fd(), folder_name.as_ptr(), ATTACHMENTS_DIR_MODE) };
    if made != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    // `O_NOFOLLOW` refuses a symlink in the folder's place: whatever sits at
    // `.latch-attachments` must be a real directory inside the workspace.
    let folder = open_directory(Some(&root), Path::new(ATTACHMENTS_DIR), true)
        .map_err(|_| AttachmentError::UnsafeFolder)?;
    write_gitignore(&folder);

    let (stem, extension) = sanitize_file_name(suggested);
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let name = candidate(&stem, extension.as_deref(), attempt);
        match create_exclusive(&folder, &name) {
            Ok(file) => {
                return Ok(PendingAttachment {
                    file,
                    folder,
                    name,
                    workspace,
                    committed: false,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(AttachmentError::NameExhausted)
}

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn open_directory(parent: Option<&OwnedFd>, path: &Path, no_follow: bool) -> io::Result<OwnedFd> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if no_follow {
        flags |= libc::O_NOFOLLOW;
    }
    let base = parent.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `name` is NUL-terminated; `base` is AT_FDCWD or an open directory.
    let fd = unsafe { libc::openat(base, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn create_exclusive(folder: &OwnedFd, name: &str) -> io::Result<File> {
    let name = c_name(name)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `folder` is an open directory; `name` is one NUL-terminated component.
    let fd = unsafe { libc::openat(folder.as_raw_fd(), name.as_ptr(), flags, ATTACHMENT_MODE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor that nothing else owns.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Keeps uploads out of the repository the session is working in. Best
/// effort: an existing `.gitignore` is left alone, and failing to write one
/// never fails the upload.
fn write_gitignore(folder: &OwnedFd) {
    if let Ok(mut file) = create_exclusive(folder, ".gitignore") {
        use std::io::Write;
        let _ = file.write_all(b"# Files sent from Latch on a paired phone.\n*\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn names_are_reduced_to_a_safe_alphabet() {
        let cases: &[(Option<&str>, &str, Option<&str>)] = &[
            (Some("IMG_0001.HEIC"), "IMG_0001", Some("heic")),
            (Some("../../etc/passwd"), "passwd", None),
            (Some("..\\..\\boot.ini"), "boot", Some("ini")),
            (Some(".bashrc"), "bashrc", None),
            (
                Some("my report (final).pdf"),
                "my-report-final",
                Some("pdf"),
            ),
            (Some("a..b...c.txt"), "a.b.c", Some("txt")),
            (Some("résumé.docx"), "r-sum", Some("docx")),
            (Some("archive.tar.gz"), "archive.tar", Some("gz")),
            (
                Some("notes.this-is-not-an-ext"),
                "notes.this-is-not-an-ext",
                None,
            ),
            (Some("..."), "attachment", None),
            (Some(""), "attachment", None),
            (None, "attachment", None),
            (Some("/"), "attachment", None),
            (Some("line\nbreak\u{0}.png"), "line-break", Some("png")),
        ];
        for (raw, stem, extension) in cases {
            let (observed_stem, observed_extension) = sanitize_file_name(*raw);
            assert_eq!(observed_stem, *stem, "{raw:?}");
            assert_eq!(observed_extension.as_deref(), *extension, "{raw:?}");
        }
        let long = "x".repeat(500) + ".png";
        let (stem, extension) = sanitize_file_name(Some(&long));
        assert_eq!(stem.len(), MAX_STEM);
        assert_eq!(extension.as_deref(), Some("png"));
    }

    #[test]
    fn uploads_land_in_the_folder_under_unique_owner_only_names() {
        let dir = tempfile::tempdir().unwrap();
        let first = create(dir.path(), Some("photo.jpg")).unwrap();
        let first = {
            let mut pending = first;
            pending.file.write_all(b"one").unwrap();
            pending.commit(3)
        };
        let second = create(dir.path(), Some("photo.jpg")).unwrap().commit(0);
        assert_eq!(first.name, "photo.jpg");
        assert_eq!(second.name, "photo-2.jpg");
        assert_eq!(first.relative_path, ".latch-attachments/photo.jpg");
        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(
            PathBuf::from(&first.path),
            canonical.join(".latch-attachments/photo.jpg")
        );
        assert_eq!(std::fs::read(&first.path).unwrap(), b"one");
        let mode = std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let ignore =
            std::fs::read_to_string(canonical.join(".latch-attachments/.gitignore")).unwrap();
        assert!(ignore.lines().any(|line| line == "*"));
    }

    #[test]
    fn an_uncommitted_upload_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = create(dir.path(), Some("half.bin")).unwrap();
        pending.file.write_all(b"partial").unwrap();
        drop(pending);
        assert!(!dir.path().join(".latch-attachments/half.bin").exists());
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(ATTACHMENTS_DIR);
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("keep.txt"), b"original").unwrap();
        let receipt = create(dir.path(), Some("keep.txt")).unwrap().commit(0);
        assert_eq!(receipt.name, "keep-2.txt");
        assert_eq!(std::fs::read(folder.join("keep.txt")).unwrap(), b"original");
    }

    /// A symlink planted where the folder belongs would send the write
    /// anywhere the Mac user can write. It is refused, not followed.
    #[test]
    fn a_symlinked_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(ATTACHMENTS_DIR)).unwrap();
        assert!(matches!(
            create(dir.path(), Some("x.txt")),
            Err(AttachmentError::UnsafeFolder)
        ));
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    /// A symlink planted at the chosen name is treated as taken, so the
    /// gateway picks another name rather than writing through it.
    #[test]
    fn a_symlinked_file_name_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(ATTACHMENTS_DIR);
        std::fs::create_dir(&folder).unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"untouched").unwrap();
        std::os::unix::fs::symlink(&target, folder.join("x.txt")).unwrap();
        let receipt = create(dir.path(), Some("x.txt")).unwrap().commit(0);
        assert_eq!(receipt.name, "x-2.txt");
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    }

    #[test]
    fn a_missing_workspace_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            create(&dir.path().join("gone"), Some("x.txt")),
            Err(AttachmentError::WorkspaceUnavailable)
        ));
    }
}
