//! What a mutation was reviewed against.
//!
//! The status the phone shows carries a fingerprint per entry and four
//! preconditions: the branch, HEAD, a digest of the index's differences from
//! HEAD, and a digest of everything status listed together with the files on
//! disk. A mutation sends them back, and the Mac computes them again, the same
//! way, immediately before acting. Anything another terminal, agent or Git
//! client changed in between shows up as a different value.
//!
//! Git's porcelain records carry object ids for HEAD and the index but not for
//! the working tree, so a file edited again while it was already modified has
//! the same record. Each unstaged, untracked or conflicted entry therefore
//! also covers the file's size, modification time, change time, inode and mode.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::status::{self, Porcelain, Record, RecordKind};
use crate::cli::serve::contract::{GitChangeArea, GitOperation, GitPreconditions};

/// Most entries read below one untracked folder. Past this the folder's
/// fingerprint covers what was read and a marker, so a very large folder
/// still has a stable value.
const FOLDER_WALK_LIMIT: usize = 4096;

/// Bytes of SHA-256 kept for an entry fingerprint.
const FINGERPRINT_BYTES: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub preconditions: GitPreconditions,
    /// Fingerprint of each listed entry, by its exact path bytes and area.
    pub fingerprints: HashMap<(Vec<u8>, GitChangeArea), String>,
    /// Untracked folders that exceeded the bounded walk. They cannot be
    /// staged from a partial fingerprint.
    pub incomplete_paths: HashSet<Vec<u8>>,
}

impl Snapshot {
    pub fn fingerprint(&self, path: &[u8], area: GitChangeArea) -> Option<&str> {
        self.fingerprints
            .get(&(path.to_vec(), area))
            .map(String::as_str)
    }

    /// Writes each entry's fingerprint into the entries status serves.
    pub fn stamp(&self, entries: &mut [crate::cli::serve::contract::GitStatusEntry]) {
        for entry in entries {
            if let Some(path) = status::unhex(&entry.path_key) {
                if let Some(fingerprint) = self.fingerprint(&path, entry.area) {
                    entry.fingerprint = fingerprint.to_owned();
                }
            }
        }
    }
}

/// Computes the snapshot of one porcelain listing of the working tree at
/// `root`. `operation` is the merge or rebase in progress, if any.
pub(crate) fn take(
    root: &Path,
    porcelain: &Porcelain,
    operation: Option<&GitOperation>,
) -> Snapshot {
    let operation = serde_json::to_vec(&operation).unwrap_or_default();
    let mut index = Sha256::new();
    let mut worktree = Sha256::new();
    index.update(b"latch-git-index-v1\0");
    worktree.update(b"latch-git-worktree-v1\0");
    index.update(&operation);
    worktree.update(&operation);
    let mut fingerprints = HashMap::new();
    let mut incomplete_paths = HashSet::new();

    for record in &porcelain.records {
        if matches!(record.kind, RecordKind::Ignored) {
            continue;
        }
        let index_side = index_fields(record);
        let disk = touches_disk(record).then(|| stamp(root, &record.path));
        if disk
            .as_ref()
            .is_some_and(|stamp| stamp.ends_with(b"\0more"))
        {
            incomplete_paths.insert(record.path.clone());
        }
        if let Some(side) = &index_side {
            field(&mut index, side);
            field(&mut index, &record.path);
            field(&mut index, record.orig_path.as_deref().unwrap_or_default());
        }
        for part in &record.fields {
            field(&mut worktree, part);
        }
        field(&mut worktree, &record.path);
        field(
            &mut worktree,
            record.orig_path.as_deref().unwrap_or_default(),
        );
        field(&mut worktree, disk.as_deref().unwrap_or_default());

        for area in areas(record) {
            let mut hasher = Sha256::new();
            field(&mut hasher, format!("{area:?}").as_bytes());
            match area {
                // Unstaging depends on the index alone: an edit on disk
                // after the file was staged does not change what unstaging
                // it does.
                GitChangeArea::Staged => {
                    field(&mut hasher, index_side.as_deref().unwrap_or_default())
                }
                _ => {
                    for part in &record.fields {
                        field(&mut hasher, part);
                    }
                    field(&mut hasher, disk.as_deref().unwrap_or_default());
                }
            }
            field(&mut hasher, &record.path);
            field(&mut hasher, record.orig_path.as_deref().unwrap_or_default());
            let digest = hasher.finalize();
            fingerprints.insert(
                (record.path.clone(), area),
                status::hex(&digest[..FINGERPRINT_BYTES]),
            );
        }
    }

    Snapshot {
        preconditions: GitPreconditions {
            branch: porcelain.branch.head.clone(),
            head: porcelain.branch.oid.clone(),
            index: status::hex(&index.finalize()),
            worktree: status::hex(&worktree.finalize()),
        },
        fingerprints,
        incomplete_paths,
    }
}

/// Length-prefixed, so no two different field lists hash the same.
fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// The areas a record is listed in, as `status::entries` splits it.
fn areas(record: &Record) -> Vec<GitChangeArea> {
    match &record.kind {
        RecordKind::Changed { x, y } => {
            let mut areas = Vec::new();
            if status::change_kind(*x).is_some() {
                areas.push(GitChangeArea::Staged);
            }
            if status::change_kind(*y).is_some() {
                areas.push(GitChangeArea::Unstaged);
            }
            areas
        }
        RecordKind::Unmerged { .. } => vec![GitChangeArea::Conflicted],
        RecordKind::Untracked => vec![GitChangeArea::Untracked],
        RecordKind::Ignored => Vec::new(),
    }
}

/// The parts of a record that describe the index against HEAD: the index
/// letter, HEAD and index modes and object ids, and a rename's score. `None`
/// for a record with nothing staged.
fn index_fields(record: &Record) -> Option<Vec<u8>> {
    let fields = &record.fields;
    let mut side = Vec::new();
    match &record.kind {
        // `1 XY sub mH mI mW hH hI` and, for a rename, `Xscore`.
        RecordKind::Changed { x, .. } if status::change_kind(*x).is_some() => {
            side.push(*x);
            for at in [3, 4, 6, 7, 8] {
                if let Some(part) = fields.get(at) {
                    side.push(0);
                    side.extend_from_slice(part);
                }
            }
        }
        // `u XY sub m1 m2 m3 mW h1 h2 h3`: every stage is index state.
        RecordKind::Unmerged { xy } => {
            side.extend_from_slice(xy);
            for at in [3, 4, 5, 7, 8, 9] {
                if let Some(part) = fields.get(at) {
                    side.push(0);
                    side.extend_from_slice(part);
                }
            }
        }
        _ => return None,
    }
    Some(side)
}

/// Whether the record's file on disk is part of what was shown.
fn touches_disk(record: &Record) -> bool {
    match &record.kind {
        RecordKind::Changed { y, .. } => status::change_kind(*y).is_some(),
        RecordKind::Unmerged { .. } | RecordKind::Untracked => true,
        RecordKind::Ignored => false,
    }
}

/// The file's identity on disk, without following a symbolic link. An
/// untracked folder (listed with a trailing slash) covers what is inside it.
pub(crate) fn stamp(root: &Path, path: &[u8]) -> Vec<u8> {
    let relative = Path::new(std::ffi::OsStr::from_bytes(
        path.strip_suffix(b"/").unwrap_or(path),
    ));
    let full = root.join(relative);
    let mut out = Vec::new();
    stamp_one(&full, &mut out);
    if path.ends_with(b"/") {
        let mut pending = vec![full];
        let mut seen = 0usize;
        while let Some(dir) = pending.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut children = read
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                seen += 1;
                if seen > FOLDER_WALK_LIMIT {
                    out.extend_from_slice(b"\0more");
                    return out;
                }
                out.push(0);
                out.extend_from_slice(child.as_os_str().as_bytes());
                stamp_one(&child, &mut out);
                let is_dir = std::fs::symlink_metadata(&child).is_ok_and(|meta| meta.is_dir());
                if is_dir {
                    pending.push(child);
                }
            }
        }
    }
    out
}

fn stamp_one(path: &Path, out: &mut Vec<u8>) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => out.extend_from_slice(
            format!(
                " {} {}.{} {}.{} {} {:o}",
                meta.size(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
                meta.ino(),
                meta.mode()
            )
            .as_bytes(),
        ),
        Err(_) => out.extend_from_slice(b" absent"),
    }
}
