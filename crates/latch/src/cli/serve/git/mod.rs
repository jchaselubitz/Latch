//! Git for paired devices: resolve a folder to its working tree, read its
//! status, list repositories and branches, page through one file's diff, and
//! (in `mutation` and `ops`) stage, unstage, commit and create or switch
//! local branches; bring other commits in (`integrate`) and push a reviewed
//! branch (`push`).
//!
//! Reads never touch the network. Only an explicit fetch, push, or check of
//! an uncertain push contacts a remote, with the Mac's own credentials. The
//! phone names a folder, a file by its exact bytes, an area, a branch or a
//! remote; every Git argument is chosen by this module, and every call
//! resolves the folder again, so the target is revalidated each time rather
//! than trusted from a previous answer.

mod branches;
mod command;
mod commit_review;
mod diff;
mod integrate;
mod mutation;
mod ops;
mod push;
mod receipts;
mod recent;
mod session;
mod snapshot;
mod status;
mod sync;
mod worktree;

#[cfg(test)]
mod integrate_tests;
#[cfg(test)]
mod mutation_tests;
#[cfg(test)]
mod push_tests;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) use branches::branches;
use command::args;
pub(crate) use integrate::is_plausible_remote_name;
pub(crate) use mutation::{check_push, receipt, submit, MutationError, PushCheckError, Submitted};
pub(crate) use ops::{is_plausible_branch_name, FileTarget, Mutation, Selection};
pub(crate) use push::{is_plausible_destination, preview as push_preview};
pub(crate) use session::refresh_directories;
pub(crate) use status::{hex as status_hex, unhex as status_unhex};
pub(crate) use sync::sync;
pub(crate) use worktree::Worktree;

use super::contract::{
    GitChangeArea, GitDiffKind, GitFileDiff, GitRepository, GitRepositoryList,
    GitRepositorySummary, GitSessionRef, GitStatus,
};
use crate::cli::json::SessionSummary;
use crate::session::paths::LatchHome;

/// A session whose output is newer than this counts as working.
const ACTIVE_WITHIN_MS: u64 = 10_000;

/// Stdout bound for `git status`. Far more than the entries one response may
/// carry; past it the list is reported as cut.
const STATUS_READ_LIMIT: usize = 8 * 1024 * 1024;

/// Stdout bound for `--numstat`. Counts are decoration: if they do not fit,
/// rows simply show none.
const NUMSTAT_READ_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitError {
    InvalidPath,
    UnavailablePath,
    UnreadableDirectory,
    NotARepository,
    BareRepository,
    /// Git refused the repository because another user owns it.
    UnsafeRepository,
    GitUnavailable,
    TimedOut,
    Failed,
    InvalidRequest,
    /// The file is not listed in that area by the current status.
    FileNotChanged,
    StaleCursor,
}

/// `GET /v2/git/resolve`.
pub(crate) async fn resolve(path: &str) -> Result<GitRepository, GitError> {
    Ok(worktree::resolve(path).await?.repository())
}

/// `GET /v2/git/status`. Reading a repository's status is what opening it
/// means, so it also moves the repository to the front of the Mac's recent
/// list.
pub(crate) async fn status(
    home: &LatchHome,
    path: &str,
    sessions: &[SessionSummary],
) -> Result<GitStatus, GitError> {
    let worktree = worktree::resolve(path).await?;
    let mut status = read_status(&worktree).await?;
    status.sessions = sessions_in(&worktree.root, sessions).await;
    // The list is a convenience; failing to write it does not fail the read.
    let _ = recent::record(home, worktree.root_str(), SystemTime::now());
    Ok(status)
}

/// One porcelain listing of the working tree, read exactly as the status
/// the phone sees is, with the snapshot mutations are checked against.
pub(crate) struct Observed {
    pub porcelain: status::Porcelain,
    /// The listing was cut at the read limit.
    pub truncated: bool,
    pub operation: Option<super::contract::GitOperation>,
    pub snapshot: snapshot::Snapshot,
}

pub(crate) async fn observe(worktree: &Worktree) -> Result<Observed, GitError> {
    let output = command::run(
        &worktree.root,
        &args([
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
            "--find-renames",
        ]),
        STATUS_READ_LIMIT,
    )
    .await?;
    if !output.succeeded() && !output.truncated {
        return Err(GitError::Failed);
    }
    let porcelain = status::parse(&output.stdout, !output.truncated);
    let operation = status::operation(&worktree.git_dir);
    let snapshot = snapshot::take(&worktree.root, &porcelain, operation.as_ref());
    Ok(Observed {
        porcelain,
        truncated: output.truncated,
        operation,
        snapshot,
    })
}

async fn read_status(worktree: &Worktree) -> Result<GitStatus, GitError> {
    let observed = observe(worktree).await?;
    let parsed = &observed.porcelain;
    let numstat = |cached: bool| {
        let mut words = vec!["diff", "--numstat", "-z", "--no-ext-diff", "--no-textconv"];
        if cached {
            words.extend(["--cached", "-M"]);
        }
        let root = worktree.root.clone();
        async move {
            match command::run(&root, &args(words), NUMSTAT_READ_LIMIT).await {
                Ok(output) if output.succeeded() && !output.truncated => {
                    status::parse_numstat(&output.stdout)
                }
                _ => HashMap::new(),
            }
        }
    };
    let (staged, unstaged) = tokio::join!(numstat(true), numstat(false));
    let mut entries = status::entries(&parsed.records, &staged, &unstaged);
    observed.snapshot.stamp(&mut entries);
    let total_entries = entries.len() as u64;
    let cut = status::bound(&mut entries);
    Ok(GitStatus {
        root: worktree.root_str().to_owned(),
        name: worktree.name(),
        kind: worktree.kind,
        head: status::head(&parsed.branch),
        upstream: status::upstream(&parsed.branch),
        last_fetched_at: last_fetched_at(worktree),
        operation: observed.operation.clone(),
        entries,
        entries_truncated: cut || observed.truncated,
        total_entries,
        sessions: Vec::new(),
        observed_at: crate::engine::format_rfc3339(SystemTime::now()),
        preconditions: observed.snapshot.preconditions.clone(),
    })
}

/// When this worktree last fetched: FETCH_HEAD's modification time. Git
/// writes it in the worktree's own git directory; an older layout or a fetch
/// from the main worktree may have left it in the common one.
fn last_fetched_at(worktree: &Worktree) -> Option<String> {
    [&worktree.git_dir, &worktree.common_dir]
        .into_iter()
        .filter_map(|dir| {
            std::fs::metadata(dir.join("FETCH_HEAD"))
                .ok()?
                .modified()
                .ok()
        })
        .max()
        .map(crate::engine::format_rfc3339)
}

/// `GET /v2/git/repositories`: working trees of running sessions, then the
/// Mac's recent list minus those, each resolved again so a deleted or moved
/// repository drops out.
pub(crate) async fn repositories(
    home: &LatchHome,
    sessions: &[SessionSummary],
) -> GitRepositoryList {
    let recent = recent::load(home);
    let opened_at = recent
        .iter()
        .map(|entry| (entry.root.clone(), entry.opened_at.clone()))
        .collect::<HashMap<_, _>>();

    let mut by_root: Vec<(Worktree, Vec<GitSessionRef>)> = Vec::new();
    for (session, worktree) in resolved_sessions(sessions).await {
        let reference = session_ref(session);
        match by_root
            .iter_mut()
            .find(|(known, _)| known.root == worktree.root)
        {
            Some((_, refs)) => refs.push(reference),
            None => by_root.push((worktree, vec![reference])),
        }
    }
    // The busiest working tree first; ties keep the session list's order.
    by_root.sort_by_key(|(_, refs)| std::cmp::Reverse(refs.iter().filter(|r| r.active).count()));
    let from_sessions = by_root
        .into_iter()
        .map(|(worktree, sessions)| GitRepositorySummary {
            root: worktree.root_str().to_owned(),
            name: worktree.name(),
            kind: worktree.kind,
            last_opened_at: opened_at.get(worktree.root_str()).cloned(),
            sessions,
        })
        .collect::<Vec<_>>();

    let mut tasks = tokio::task::JoinSet::new();
    for (order, entry) in recent.into_iter().enumerate() {
        if from_sessions.iter().any(|known| known.root == entry.root) {
            continue;
        }
        tasks.spawn(async move {
            let worktree = worktree::resolve(&entry.root).await.ok()?;
            // Only a root that still resolves to itself is offered: a folder
            // that became part of another repository is no longer this one.
            (worktree.root_str() == entry.root).then_some((order, worktree, entry.opened_at))
        });
    }
    let mut still_there = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        if let Ok(Some(found)) = joined {
            still_there.push(found);
        }
    }
    still_there.sort_by_key(|(order, _, _)| *order);
    let recent = still_there
        .into_iter()
        .take(recent::RECENT_LIMIT)
        .map(|(_, worktree, opened_at)| GitRepositorySummary {
            root: worktree.root_str().to_owned(),
            name: worktree.name(),
            kind: worktree.kind,
            sessions: Vec::new(),
            last_opened_at: Some(opened_at),
        })
        .collect();
    GitRepositoryList {
        from_sessions,
        recent,
    }
}

/// Running sessions whose folder resolves to the working tree at `root`.
pub(crate) async fn sessions_in(root: &Path, sessions: &[SessionSummary]) -> Vec<GitSessionRef> {
    // A working tree's top level contains every folder that resolves to it,
    // so anything outside `root` is skipped without asking Git.
    let candidates = sessions
        .iter()
        .filter(|session| canonical_cwd(session).is_some_and(|cwd| cwd.starts_with(root)))
        .cloned()
        .collect::<Vec<_>>();
    resolved_sessions(&candidates)
        .await
        .into_iter()
        .filter(|(_, worktree)| worktree.root == root)
        .map(|(session, _)| session_ref(session))
        .collect()
}

fn canonical_cwd(session: &SessionSummary) -> Option<PathBuf> {
    (session.state == "running")
        .then(|| std::fs::canonicalize(&session.cwd).ok())
        .flatten()
}

/// Each running session paired with the working tree its folder resolves
/// to, in the session list's order. A folder is resolved once however many
/// sessions share it.
async fn resolved_sessions(sessions: &[SessionSummary]) -> Vec<(&SessionSummary, Worktree)> {
    let mut folders: HashMap<PathBuf, Option<Worktree>> = HashMap::new();
    let mut paired = Vec::new();
    for session in sessions {
        let Some(cwd) = canonical_cwd(session) else {
            continue;
        };
        if !folders.contains_key(&cwd) {
            let resolved = worktree::resolve_canonical(&cwd).await.ok();
            folders.insert(cwd.clone(), resolved);
        }
        if let Some(Some(worktree)) = folders.get(&cwd) {
            paired.push((session, worktree.clone()));
        }
    }
    paired
}

fn session_ref(session: &SessionSummary) -> GitSessionRef {
    let working = session.agent_phase.as_deref() == Some("working");
    let recent_output = session.idle_ms.is_some_and(|idle| idle < ACTIVE_WITHIN_MS);
    GitSessionRef {
        id: session.id.clone(),
        name: session.name.clone(),
        title: session.title.clone(),
        command_label: session.command_label.clone(),
        connector: session.connector.clone(),
        agent_phase: session.agent_phase.clone(),
        idle_ms: session.idle_ms,
        active: working || recent_output,
    }
}

/// `GET /v2/git/diff`.
pub(crate) async fn file_diff(
    path: &str,
    file_key: &str,
    area: GitChangeArea,
    cursor: Option<&str>,
) -> Result<GitFileDiff, GitError> {
    let file = status::unhex(file_key).ok_or(GitError::InvalidRequest)?;
    if !is_relative_path(&file) {
        return Err(GitError::InvalidRequest);
    }
    let worktree = worktree::resolve(path).await?;

    // The file must be one the current status lists in this area. That is
    // what keeps the diff inside this working tree: the phone can only name
    // a change Git itself reported.
    let listing = command::run(
        &worktree.root,
        &args([
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=normal",
            "--find-renames",
        ]),
        STATUS_READ_LIMIT,
    )
    .await?;
    if !listing.succeeded() && !listing.truncated {
        return Err(GitError::Failed);
    }
    let parsed = status::parse(&listing.stdout, !listing.truncated);
    let record = parsed
        .records
        .iter()
        .find(|record| record.path == file && in_area(record, area))
        .ok_or(GitError::FileNotChanged)?;

    let mut result = GitFileDiff {
        root: worktree.root_str().to_owned(),
        path: status::display(&record.path),
        path_key: status::hex(&record.path),
        orig_path: (area == GitChangeArea::Staged)
            .then(|| record.orig_path.as_deref().map(status::display))
            .flatten(),
        area,
        kind: GitDiffKind::Text,
        additions: 0,
        deletions: 0,
        hunks: Vec::new(),
        total_hunks: 0,
        next_cursor: None,
        notes: Vec::new(),
    };
    if record.submodule {
        result.kind = GitDiffKind::Submodule;
        return Ok(result);
    }
    if area == GitChangeArea::Untracked {
        if record.path.ends_with(b"/") {
            result.kind = GitDiffKind::Directory;
            return Ok(result);
        }
        let on_disk = worktree.root.join(path_of(&record.path));
        let too_large = std::fs::symlink_metadata(&on_disk)
            .is_ok_and(|metadata| metadata.len() > diff::MAX_DIFF_BYTES as u64);
        if too_large {
            result.kind = GitDiffKind::TooLarge;
            return Ok(result);
        }
    }

    let output = command::run(
        &worktree.root,
        &diff_args(area, record),
        diff::MAX_DIFF_BYTES,
    )
    .await?;
    if output.truncated {
        result.kind = GitDiffKind::TooLarge;
        return Ok(result);
    }
    // `--no-index` exits 1 when the files differ, which is the point.
    if !matches!(output.code, Some(0 | 1)) {
        return Err(GitError::Failed);
    }
    let parsed = diff::parse(&output.stdout);
    result.notes = parsed.notes;
    if parsed.binary {
        result.kind = GitDiffKind::Binary;
        return Ok(result);
    }
    let fingerprint = diff::fingerprint(&output.stdout);
    let start = match cursor {
        None => 0,
        Some(cursor) => diff::parse_cursor(cursor, &fingerprint, parsed.hunks.len())
            .ok_or(GitError::StaleCursor)?,
    };
    let (hunks, next) = diff::page(&parsed.hunks, start);
    result.additions = parsed.additions;
    result.deletions = parsed.deletions;
    result.total_hunks = parsed.hunks.len() as u32;
    result.hunks = hunks;
    result.next_cursor = next.map(|index| diff::cursor(&fingerprint, index));
    Ok(result)
}

fn in_area(record: &status::Record, area: GitChangeArea) -> bool {
    use status::RecordKind;
    let changed = |letter: u8| matches!(letter, b'M' | b'A' | b'D' | b'R' | b'C' | b'T');
    match (&record.kind, area) {
        (RecordKind::Changed { x, .. }, GitChangeArea::Staged) => changed(*x),
        (RecordKind::Changed { y, .. }, GitChangeArea::Unstaged) => changed(*y),
        (RecordKind::Unmerged { .. }, GitChangeArea::Conflicted) => true,
        (RecordKind::Untracked, GitChangeArea::Untracked) => true,
        _ => false,
    }
}

fn diff_args(area: GitChangeArea, record: &status::Record) -> Vec<OsString> {
    let mut words = args([
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--submodule=short",
        "--unified=3",
    ]);
    let path = path_of(&record.path).into_os_string();
    match area {
        GitChangeArea::Staged => {
            words.extend(args(["--cached", "-M", "--"]));
            if let Some(orig) = &record.orig_path {
                words.push(path_of(orig).into_os_string());
            }
            words.push(path);
        }
        GitChangeArea::Unstaged => {
            words.push("--".into());
            words.push(path);
        }
        // Against HEAD, so the conflict markers Git wrote show as the lines
        // they are, read-only.
        GitChangeArea::Conflicted => {
            words.extend(args(["HEAD", "--"]));
            words.push(path);
        }
        GitChangeArea::Untracked => {
            words.extend(args(["--no-index", "--", "/dev/null"]));
            words.push(path);
        }
    }
    words
}

pub(crate) fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

/// A path relative to the working tree root that cannot leave it: no root,
/// no empty, `.` or `..` component, no NUL, and a bounded length.
pub(crate) fn is_relative_path(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > super::directory::MAX_PATH_BYTES || bytes[0] == b'/' {
        return false;
    }
    if bytes.contains(&0) {
        return false;
    }
    // An untracked folder is listed with a trailing slash.
    let trimmed = bytes.strip_suffix(b"/").unwrap_or(bytes);
    trimmed
        .split(|byte| *byte == b'/')
        .all(|part| !part.is_empty() && part != b"." && part != b"..")
}
