//! `GET /v2/git/sync`: what an update would bring in, as last fetched.
//!
//! Read-only and offline: the upstream is whatever the last fetch left in
//! the remote-tracking ref, and the answer says when that was. The phone
//! sends the upstream's name and commit back with an update, which is
//! refused if either moved since.

use std::ffi::OsString;

use super::command::{self, args};
use super::integrate::{self, remotes, upstream_of};
use super::{last_fetched_at, observe, status, worktree, GitError};
use crate::cli::serve::contract::{
    GitCommitSummary, GitRemote, GitSyncPreview, GitSyncRelation, GitSyncUpstream,
};

/// Incoming commits per page.
pub(crate) const PAGE: usize = 50;
/// Characters of a subject or author name sent.
const TEXT_CHARS: usize = 200;
/// Stdout bound for one page of `git log`.
const LOG_LIMIT: usize = 256 * 1024;
const CURSOR_PREFIX: &str = "v2";

pub(crate) async fn sync(path: &str, cursor: Option<&str>) -> Result<GitSyncPreview, GitError> {
    let worktree = worktree::resolve(path).await?;
    let observed = observe(&worktree).await?;
    let head = status::head(&observed.porcelain.branch);
    let preconditions = observed.snapshot.preconditions.clone();
    let remotes = remotes(&worktree)
        .await?
        .into_iter()
        .map(|(name, url)| GitRemote { name, url })
        .collect::<Vec<_>>();

    let upstream = match preconditions.branch.as_deref() {
        Some(branch) => upstream_of(&worktree, branch).await,
        None => None,
    };
    let upstream = match upstream {
        Some(found) => {
            let oid = integrate::ref_oid(&worktree, &found.reference).await;
            let remote_url = found.remote.as_ref().and_then(|remote| {
                remotes
                    .iter()
                    .find(|known| &known.name == remote)
                    .and_then(|known| known.url.clone())
            });
            Some(GitSyncUpstream {
                name: found.name,
                reference: found.reference,
                remote: found.remote,
                remote_url,
                gone: oid.is_none(),
                oid,
            })
        }
        None => None,
    };

    let (ahead, behind) = match (&upstream, &preconditions.head) {
        (Some(GitSyncUpstream { oid: Some(oid), .. }), Some(_)) => counts(&worktree, oid).await,
        _ => (None, None),
    };
    let relation = if head.detached {
        GitSyncRelation::Detached
    } else if preconditions.head.is_none() {
        GitSyncRelation::Unborn
    } else {
        match &upstream {
            None => GitSyncRelation::NoUpstream,
            Some(found) if found.gone => GitSyncRelation::UpstreamGone,
            Some(_) => match (ahead.unwrap_or(0), behind.unwrap_or(0)) {
                (0, 0) => GitSyncRelation::UpToDate,
                (0, _) => GitSyncRelation::Behind,
                (_, 0) => GitSyncRelation::Ahead,
                _ => GitSyncRelation::Diverged,
            },
        }
    };

    let branch = preconditions.branch.as_deref().unwrap_or("HEAD");
    let upstream_name = upstream
        .as_ref()
        .map_or("its upstream", |found| found.name.as_str());
    let fast_forward_blocked_reason = if let Some(operation) = &observed.operation {
        Some(format!(
            "A {} is in progress on your Mac. Continue or abort it first.",
            match operation.kind {
                crate::cli::serve::contract::GitOperationKind::Merge => "merge",
                crate::cli::serve::contract::GitOperationKind::Rebase => "rebase",
                crate::cli::serve::contract::GitOperationKind::CherryPick => "cherry-pick",
                crate::cli::serve::contract::GitOperationKind::Revert => "revert",
            }
        ))
    } else {
        match relation {
            GitSyncRelation::Behind => None,
            GitSyncRelation::UpToDate | GitSyncRelation::Ahead => Some(format!(
                "{branch} already has every commit {upstream_name} had when it was last fetched."
            )),
            GitSyncRelation::Diverged => Some(format!(
                "{branch} has {} that {upstream_name} doesn't, so it can't simply move forward. Merge or Rebase can combine them.",
                plural(ahead.unwrap_or(0), "commit")
            )),
            GitSyncRelation::NoUpstream => Some(format!(
                "{branch} has no upstream branch, so there is nothing to update from."
            )),
            GitSyncRelation::UpstreamGone => Some(format!(
                "{upstream_name} no longer exists as of the last fetch."
            )),
            GitSyncRelation::Detached => {
                Some("HEAD is detached, so there is no branch to update.".to_owned())
            }
            GitSyncRelation::Unborn => {
                Some(format!("{branch} has no commits yet."))
            }
        }
    };

    let (incoming, next_cursor) = match (&upstream, &preconditions.head) {
        (Some(GitSyncUpstream { oid: Some(oid), .. }), Some(_)) if behind.unwrap_or(0) > 0 => {
            let skip = match cursor {
                None => 0,
                Some(cursor) => parse_cursor(cursor, preconditions.head.as_deref().unwrap(), oid)
                    .ok_or(GitError::StaleCursor)?,
            };
            page(&worktree, preconditions.head.as_deref().unwrap(), oid, skip).await?
        }
        _ if cursor.is_some() => return Err(GitError::StaleCursor),
        _ => (Vec::new(), None),
    };

    Ok(GitSyncPreview {
        root: worktree.root_str().to_owned(),
        head,
        upstream,
        remotes,
        last_fetched_at: last_fetched_at(&worktree),
        relation,
        ahead,
        behind,
        fast_forward_blocked_reason,
        incoming,
        next_cursor,
        operation: observed.operation,
        preconditions,
    })
}

fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Commits on HEAD not on `oid`, and on `oid` not on HEAD.
async fn counts(worktree: &worktree::Worktree, oid: &str) -> (Option<u64>, Option<u64>) {
    let range = format!("HEAD...{oid}");
    let Ok(output) = command::run(
        &worktree.root,
        &args(["rev-list", "--left-right", "--count", range.as_str()]),
        128,
    )
    .await
    else {
        return (None, None);
    };
    if !output.succeeded() {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut numbers = text.split_whitespace().map(|word| word.parse::<u64>().ok());
    (numbers.next().flatten(), numbers.next().flatten())
}

pub(crate) fn cursor(head: &str, oid: &str, skip: usize) -> String {
    format!("{CURSOR_PREFIX}-{head}-{oid}-{skip}")
}

/// The page start a cursor names, if it was made for the upstream at `oid`.
pub(crate) fn parse_cursor(cursor: &str, head: &str, oid: &str) -> Option<usize> {
    let rest = cursor.strip_prefix(CURSOR_PREFIX)?.strip_prefix('-')?;
    let (made_for, skip) = rest.rsplit_once('-')?;
    (made_for == format!("{head}-{oid}")).then_some(())?;
    skip.parse().ok()
}

/// One page of commits on `oid` that HEAD lacks, newest first.
async fn page(
    worktree: &worktree::Worktree,
    head: &str,
    oid: &str,
    skip: usize,
) -> Result<(Vec<GitCommitSummary>, Option<String>), GitError> {
    let words = vec![
        OsString::from("log"),
        "--no-show-signature".into(),
        "--no-color".into(),
        "-z".into(),
        "--format=%H%x00%an%x00%cI%x00%s".into(),
        format!("--skip={skip}").into(),
        format!("--max-count={}", PAGE + 1).into(),
        format!("{head}..{oid}").into(),
        "--".into(),
    ];
    let output = command::run(&worktree.root, &words, LOG_LIMIT).await?;
    if !output.succeeded() && !output.truncated {
        return Err(GitError::Failed);
    }
    let mut commits = parse_log(&output.stdout);
    let more = commits.len() > PAGE || output.truncated;
    commits.truncate(PAGE);
    if more && commits.is_empty() {
        return Err(GitError::Failed);
    }
    let next = more.then(|| cursor(head, oid, skip + commits.len()));
    Ok((commits, next))
}

pub(crate) fn parse_log(bytes: &[u8]) -> Vec<GitCommitSummary> {
    let bound = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .chars()
            .filter(|c| !c.is_control())
            .take(TEXT_CHARS)
            .collect::<String>()
    };
    // Each record is four NUL-terminated fields. Unit separators and
    // newlines are valid commit text and cannot be used as field delimiters.
    let fields = bytes
        .split_inclusive(|byte| *byte == 0)
        .filter_map(|field| field.strip_suffix(&[0]))
        .collect::<Vec<_>>();
    fields
        .chunks_exact(4)
        .filter_map(|fields| {
            let [oid, author, date, subject] = fields else {
                return None;
            };
            let oid = oid.strip_prefix(b"\n").unwrap_or(oid);
            let oid = std::str::from_utf8(oid).ok()?;
            if !(40..=64).contains(&oid.len()) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(GitCommitSummary {
                oid: oid.to_owned(),
                subject: bound(subject),
                author: bound(author),
                committed_at: std::str::from_utf8(date)
                    .ok()
                    .filter(|date| !date.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}
