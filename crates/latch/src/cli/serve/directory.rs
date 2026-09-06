//! Bounded, non-recursive directory browsing for the remote gateway.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::contract::{DirectoryEntry, DirectoryPage};

pub(crate) const MAX_PATH_BYTES: usize = 4096;
pub(crate) const PAGE_SIZE: usize = 200;
const MAX_CURSOR_BYTES: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowseError {
    InvalidPath,
    UnavailablePath,
    UnreadableDirectory,
    StaleCursor,
}

/// Lists one page of directories. An omitted path starts at the current user's
/// home directory; every returned path is canonical and absolute.
pub(crate) fn browse(
    path: Option<&str>,
    cursor: Option<&str>,
) -> Result<DirectoryPage, BrowseError> {
    let home = std::env::var_os("HOME").ok_or(BrowseError::UnavailablePath)?;
    browse_with_home(path, cursor, Path::new(&home))
}

fn browse_with_home(
    path: Option<&str>,
    cursor: Option<&str>,
    home: &Path,
) -> Result<DirectoryPage, BrowseError> {
    let requested = match path {
        Some(path) if path.is_empty() || path.len() > MAX_PATH_BYTES => {
            return Err(BrowseError::InvalidPath)
        }
        Some(path) => PathBuf::from(path),
        None => home.to_owned(),
    };
    if !requested.is_absolute() {
        return Err(BrowseError::InvalidPath);
    }

    let canonical = canonical_directory(&requested)?;

    let reader = fs::read_dir(&canonical).map_err(map_open_error)?;
    let mut entries = Vec::new();
    for candidate in reader {
        // A child may disappear, become inaccessible, or be a broken symlink
        // while its parent is open. None of those races invalidates the page.
        let Ok(candidate) = candidate else { continue };
        let Some(name) = candidate.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(resolved) = fs::canonicalize(candidate.path()) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&resolved) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(path) = resolved.to_str() else {
            continue;
        };
        if path.len() > MAX_PATH_BYTES {
            continue;
        }
        entries.push(DirectoryEntry {
            name,
            path: path.to_owned(),
        });
    }
    entries.sort_by(compare_entries);

    let fingerprint = fingerprint(&canonical, &entries);
    let offset = match cursor {
        None => 0,
        Some(cursor) => parse_cursor(cursor, &fingerprint, entries.len())?,
    };
    let end = entries.len().min(offset.saturating_add(PAGE_SIZE));
    let next_cursor = (end < entries.len()).then(|| format!("v1-{fingerprint}-{end}"));
    let parent = canonical.parent().and_then(Path::to_str).map(str::to_owned);
    let path = canonical
        .to_str()
        .ok_or(BrowseError::InvalidPath)?
        .to_owned();

    Ok(DirectoryPage {
        path,
        parent,
        entries: entries[offset..end].to_vec(),
        next_cursor,
    })
}

/// Resolves one caller-supplied path to a canonical, accessible directory.
///
/// Both browsing and creation validate through here, so the phone cannot reach
/// a directory by one route that the other would have refused.
pub(crate) fn canonical_directory(requested: &Path) -> Result<PathBuf, BrowseError> {
    if !requested.is_absolute() {
        return Err(BrowseError::InvalidPath);
    }
    let canonical = fs::canonicalize(requested).map_err(map_open_error)?;
    if canonical.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(BrowseError::InvalidPath);
    }
    // A canonical path that is not UTF-8 cannot round-trip through the JSON
    // contract, so it is not a path this gateway can serve.
    if canonical.to_str().is_none() {
        return Err(BrowseError::InvalidPath);
    }
    if !fs::metadata(&canonical).map_err(map_open_error)?.is_dir() {
        return Err(BrowseError::InvalidPath);
    }
    Ok(canonical)
}

/// Validates a caller-supplied absolute path string before canonicalizing it.
pub(crate) fn canonical_directory_from_str(path: &str) -> Result<PathBuf, BrowseError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err(BrowseError::InvalidPath);
    }
    canonical_directory(Path::new(path))
}

fn compare_entries(left: &DirectoryEntry, right: &DirectoryEntry) -> Ordering {
    let folded = left.name.to_lowercase().cmp(&right.name.to_lowercase());
    if folded == Ordering::Equal {
        left.name.as_bytes().cmp(right.name.as_bytes())
    } else {
        folded
    }
}

fn map_open_error(error: io::Error) -> BrowseError {
    match error.kind() {
        io::ErrorKind::NotFound => BrowseError::UnavailablePath,
        io::ErrorKind::PermissionDenied => BrowseError::UnreadableDirectory,
        _ => BrowseError::UnavailablePath,
    }
}

fn fingerprint(path: &Path, entries: &[DirectoryEntry]) -> String {
    let mut digest = Sha256::new();
    digest.update(path.as_os_str().as_encoded_bytes());
    for entry in entries {
        digest.update([0]);
        digest.update(entry.name.as_bytes());
        digest.update([0]);
        digest.update(entry.path.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn parse_cursor(cursor: &str, fingerprint: &str, len: usize) -> Result<usize, BrowseError> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(BrowseError::StaleCursor);
    }
    let Some(rest) = cursor.strip_prefix("v1-") else {
        return Err(BrowseError::StaleCursor);
    };
    let Some((observed, offset)) = rest.rsplit_once('-') else {
        return Err(BrowseError::StaleCursor);
    };
    let offset = offset
        .parse::<usize>()
        .map_err(|_| BrowseError::StaleCursor)?;
    if observed != fingerprint || offset == 0 || offset >= len || offset % PAGE_SIZE != 0 {
        return Err(BrowseError::StaleCursor);
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn make_dir(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn omitted_path_resolves_home_and_root_has_no_parent() {
        let temp = tempfile::tempdir().unwrap();
        let home = make_dir(temp.path(), "home");
        let page = browse_with_home(None, None, &home).unwrap();
        assert_eq!(
            page.path,
            fs::canonicalize(&home).unwrap().to_str().unwrap()
        );
        assert_eq!(
            page.parent,
            fs::canonicalize(temp.path())
                .unwrap()
                .to_str()
                .map(str::to_owned)
        );

        let root = browse_with_home(Some("/"), None, &home).unwrap();
        assert_eq!(root.parent, None);
    }

    #[test]
    fn rejects_relative_missing_files_and_plain_files() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        fs::write(&file, b"secret").unwrap();
        assert_eq!(
            browse_with_home(Some("relative"), None, temp.path()),
            Err(BrowseError::InvalidPath)
        );
        assert_eq!(
            browse_with_home(
                Some(temp.path().join("missing").to_str().unwrap()),
                None,
                temp.path()
            ),
            Err(BrowseError::UnavailablePath)
        );
        assert_eq!(
            browse_with_home(Some(file.to_str().unwrap()), None, temp.path()),
            Err(BrowseError::InvalidPath)
        );
    }

    #[test]
    fn returns_only_directories_with_canonical_symlink_targets() {
        let temp = tempfile::tempdir().unwrap();
        let target = make_dir(temp.path(), "target");
        fs::write(temp.path().join("file"), b"not listed").unwrap();
        symlink(&target, temp.path().join("alias")).unwrap();
        symlink(temp.path().join("gone"), temp.path().join("broken")).unwrap();

        let page =
            browse_with_home(Some(temp.path().to_str().unwrap()), None, temp.path()).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].name, "alias");
        assert_eq!(
            page.entries[0].path,
            fs::canonicalize(&target).unwrap().to_str().unwrap()
        );
        assert_eq!(page.entries[1].name, "target");
    }

    #[test]
    fn unicode_sorting_is_case_insensitive_with_a_bytewise_tie_breaker() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["éclair", "Zoo", "alpha"] {
            make_dir(temp.path(), name);
        }
        let page =
            browse_with_home(Some(temp.path().to_str().unwrap()), None, temp.path()).unwrap();
        let names = page
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "Zoo", "éclair"]);

        let lower = DirectoryEntry {
            name: "alpha".to_owned(),
            path: "/a".to_owned(),
        };
        let upper = DirectoryEntry {
            name: "Alpha".to_owned(),
            path: "/A".to_owned(),
        };
        assert_eq!(compare_entries(&upper, &lower), Ordering::Less);
    }

    #[test]
    fn pagination_is_bounded_deterministic_and_detects_stale_cursors() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..205 {
            make_dir(temp.path(), &format!("folder-{index:03}"));
        }
        let first =
            browse_with_home(Some(temp.path().to_str().unwrap()), None, temp.path()).unwrap();
        assert_eq!(first.entries.len(), PAGE_SIZE);
        let cursor = first.next_cursor.clone().unwrap();
        assert!(cursor.len() <= MAX_CURSOR_BYTES);
        let second = browse_with_home(
            Some(temp.path().to_str().unwrap()),
            Some(&cursor),
            temp.path(),
        )
        .unwrap();
        assert_eq!(second.entries.len(), 5);
        assert_eq!(second.entries[0].name, "folder-200");
        assert_eq!(second.next_cursor, None);

        make_dir(temp.path(), "added-later");
        assert_eq!(
            browse_with_home(
                Some(temp.path().to_str().unwrap()),
                Some(&cursor),
                temp.path()
            ),
            Err(BrowseError::StaleCursor)
        );
    }

    #[test]
    fn unreadable_directory_is_a_stable_error() {
        let temp = tempfile::tempdir().unwrap();
        let locked = make_dir(temp.path(), "locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let parent =
            browse_with_home(Some(temp.path().to_str().unwrap()), None, temp.path()).unwrap();
        assert!(parent.entries.iter().any(|entry| entry.name == "locked"));
        let result = browse_with_home(Some(locked.to_str().unwrap()), None, temp.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result, Err(BrowseError::UnreadableDirectory));
    }
}
