//! `GET /v2/git/branches`: the working tree's local branches.

use std::ffi::OsString;

use super::command::{self, args};
use super::{observe, status, worktree, GitError};
use crate::cli::serve::contract::{GitBranch, GitBranchList};

/// Branches one response carries; with subjects bounded this stays far below
/// the phone's 512 KiB response limit.
pub(crate) const MAX_BRANCHES: usize = 500;

/// Characters of a commit subject sent per branch.
const SUBJECT_CHARS: usize = 200;

/// Stdout bound for `for-each-ref`.
const READ_LIMIT: usize = 4 * 1024 * 1024;

const FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(committerdate:iso-strict)%00%(worktreepath)%00%(contents:subject)";

pub(crate) async fn branches(path: &str) -> Result<GitBranchList, GitError> {
    let worktree = worktree::resolve(path).await?;
    let observed = observe(&worktree).await?;
    let output = command::run(
        &worktree.root,
        &args([
            "for-each-ref",
            "--sort=-committerdate",
            FORMAT,
            "refs/heads/",
        ]),
        READ_LIMIT,
    )
    .await?;
    if !output.succeeded() && !output.truncated {
        return Err(GitError::Failed);
    }
    let mut branches = parse(&output.stdout, !output.truncated, &worktree.root);
    // The current branch first, then most recently committed.
    branches.sort_by_key(|branch| !branch.current);
    let truncated = output.truncated || branches.len() > MAX_BRANCHES;
    branches.truncate(MAX_BRANCHES);
    Ok(GitBranchList {
        root: worktree.root_str().to_owned(),
        head: status::head(&observed.porcelain.branch),
        branches,
        truncated,
        preconditions: observed.snapshot.preconditions,
    })
}

/// Parses `for-each-ref` output in [`FORMAT`]: one line per branch, fields
/// separated by NUL. Ref names cannot contain a newline or NUL, and a
/// subject is a single line.
pub(crate) fn parse(output: &[u8], complete: bool, root: &std::path::Path) -> Vec<GitBranch> {
    let mut lines = output.split(|byte| *byte == b'\n').peekable();
    let mut branches = Vec::new();
    while let Some(line) = lines.next() {
        if lines.peek().is_none() && (line.is_empty() || !complete) {
            break;
        }
        let fields = line.split(|byte| *byte == 0).collect::<Vec<_>>();
        let [refname, oid, head, upstream, track, date, worktree_path, subject] = fields[..] else {
            continue;
        };
        // A name that is not UTF-8 could not be sent back exactly; it is
        // left out rather than shown as something it is not.
        let Some(name) = std::str::from_utf8(refname)
            .ok()
            .and_then(|name| name.strip_prefix("refs/heads/"))
        else {
            continue;
        };
        let Ok(oid) = std::str::from_utf8(oid) else {
            continue;
        };
        let current = head == b"*";
        let (ahead, behind, gone) = tracking(&String::from_utf8_lossy(track));
        let upstream =
            (!upstream.is_empty()).then(|| String::from_utf8_lossy(upstream).into_owned());
        let elsewhere = !current
            && !worktree_path.is_empty()
            && std::fs::canonicalize(std::path::PathBuf::from(OsString::from(
                String::from_utf8_lossy(worktree_path).into_owned(),
            )))
            .map_or(true, |path| path != root);
        branches.push(GitBranch {
            name: name.to_owned(),
            oid: oid.to_owned(),
            current,
            ahead: upstream.as_ref().and(ahead),
            behind: upstream.as_ref().and(behind),
            upstream,
            upstream_gone: gone,
            subject: String::from_utf8_lossy(subject)
                .chars()
                .take(SUBJECT_CHARS)
                .collect(),
            committed_at: (!date.is_empty()).then(|| String::from_utf8_lossy(date).into_owned()),
            checked_out_elsewhere: elsewhere,
        });
    }
    branches
}

/// `%(upstream:track,nobracket)`: empty when in step, `ahead N`, `behind N`,
/// both, or `gone`.
fn tracking(text: &str) -> (Option<u64>, Option<u64>, bool) {
    if text.trim() == "gone" {
        return (None, None, true);
    }
    let (mut ahead, mut behind) = (Some(0), Some(0));
    for part in text.split(',') {
        let mut words = part.split_whitespace();
        match (words.next(), words.next().and_then(|n| n.parse().ok())) {
            (Some("ahead"), Some(count)) => ahead = Some(count),
            (Some("behind"), Some(count)) => behind = Some(count),
            _ => {}
        }
    }
    (ahead, behind, false)
}
