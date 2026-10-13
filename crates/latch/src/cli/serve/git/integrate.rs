//! Bringing other commits in: fetch, fast-forward, merge and rebase, and
//! Continue or Abort for a merge or rebase Git stopped.
//!
//! Each is checked under the worktree lock against what the phone reviewed:
//! the branch and HEAD, the index and working tree, the operation in
//! progress and, for an update, the upstream and the exact commit it named.
//! Git then runs with a fixed argument list. Nothing here stashes, resets,
//! cleans or forces; autostash is turned off explicitly, because a Mac's
//! `rebase.autoStash` or `merge.autoStash` would otherwise stash for us.
//! When Git refuses, its refusal is the answer.
//!
//! A merge or rebase that stops part-way is not a failure: Git did what was
//! asked and is waiting for the user. That is reported as `stopped`, with the
//! conflicted files, and the phone never claims to have resolved them.

use std::ffi::OsString;
use std::time::{Duration, SystemTime};

use super::command::{self, args, Extras, GitOutput};
use super::ops::{self, Interruption, Mutation, Outcome, Plan};
use super::receipts::Before;
use super::status::{self, RecordKind};
use super::{observe, GitError, Observed, Worktree};
use crate::cli::serve::contract::{
    GitControlledOperation, GitOperation, GitOperationKind, GitPreconditions,
};

/// A fetch of a large repository over a slow link. Past this the whole
/// process group (including `ssh` or a credential helper) is killed.
const FETCH_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 6 } else { 300 });
/// A merge or fast-forward, including its checkout and hooks.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 20 } else { 180 });
/// A rebase replays every commit, running hooks for each.
const REBASE_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 20 } else { 300 });
/// Continue commits (hooks run); Abort restores a tree.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 20 } else { 180 });
/// Files a refusal or a stop names.
const MAX_FILES: usize = 200;
/// Bytes of one conflicted file searched for conflict markers.
const MARKER_SCAN_LIMIT: u64 = 8 * 1024 * 1024;
/// Remotes listed by the sync preview.
pub(crate) const MAX_REMOTES: usize = 50;

fn kind_name(operation: GitControlledOperation) -> &'static str {
    match operation {
        GitControlledOperation::Merge => "merge",
        GitControlledOperation::Rebase => "rebase",
    }
}

fn matches_kind(operation: GitControlledOperation, kind: GitOperationKind) -> bool {
    matches!(
        (operation, kind),
        (GitControlledOperation::Merge, GitOperationKind::Merge)
            | (GitControlledOperation::Rebase, GitOperationKind::Rebase)
    )
}

/// A remote name as the phone may send one: what `git remote` could list,
/// and never something Git would read as an option.
pub(crate) fn is_plausible_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && !name.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The remotes configured for this repository, with their fetch URLs
/// stripped of any user name or password.
pub(crate) async fn remotes(
    worktree: &Worktree,
) -> Result<Vec<(String, Option<String>)>, GitError> {
    let names = command::run(&worktree.root, &args(["remote"]), 64 * 1024).await?;
    if !names.succeeded() {
        return Err(GitError::Failed);
    }
    let urls = command::run(
        &worktree.root,
        &args(["config", "--get-regexp", r"^remote\..*\.url$"]),
        256 * 1024,
    )
    .await?;
    let url_text = String::from_utf8_lossy(&urls.stdout).into_owned();
    let mut found = Vec::new();
    for name in String::from_utf8_lossy(&names.stdout).lines() {
        let name = name.trim();
        if name.is_empty() || !is_plausible_remote_name(name) {
            continue;
        }
        let key = format!("remote.{name}.url ");
        let url = url_text
            .lines()
            .find_map(|line| line.strip_prefix(&key))
            .map(redact_url);
        found.push((name.to_owned(), url));
        if found.len() >= MAX_REMOTES {
            break;
        }
    }
    Ok(found)
}

/// A URL without the user name and password a Mac may keep in it
/// (`https://user:token@host/repo`); scp-style `git@host:repo` keeps its
/// user, which is not a secret.
pub(crate) fn redact_url(url: &str) -> String {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = match rest.find('/') {
            Some(at) => rest.split_at(at),
            None => (rest, ""),
        };
        if let Some((_, host)) = authority.rsplit_once('@') {
            return format!("{scheme}://{host}{path}");
        }
    }
    url.to_owned()
}

/// The current branch's upstream: its short name, full ref and remote.
pub(crate) struct Upstream {
    pub name: String,
    pub reference: String,
    pub remote: Option<String>,
}

pub(crate) async fn upstream_of(worktree: &Worktree, branch: &str) -> Option<Upstream> {
    if !ops::is_plausible_branch_name(branch) {
        return None;
    }
    let words = vec![
        OsString::from("for-each-ref"),
        "--format=%(upstream)%00%(upstream:short)%00%(upstream:remotename)".into(),
        format!("refs/heads/{branch}").into(),
    ];
    let output = command::run(&worktree.root, &words, 8192).await.ok()?;
    if !output.succeeded() {
        return None;
    }
    let line = output.stdout.split(|byte| *byte == b'\n').next()?;
    let fields = line.split(|byte| *byte == 0).collect::<Vec<_>>();
    let [reference, name, remote] = fields[..] else {
        return None;
    };
    let text = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .ok()
            .filter(|t| !t.is_empty())
    };
    Some(Upstream {
        reference: text(reference)?,
        name: text(name)?,
        remote: text(remote).filter(|remote| remote != "."),
    })
}

/// The commit a full ref points at.
pub(crate) async fn ref_oid(worktree: &Worktree, reference: &str) -> Option<String> {
    let words = vec![
        OsString::from("rev-parse"),
        "--verify".into(),
        "--quiet".into(),
        "--end-of-options".into(),
        format!("{reference}^{{commit}}").into(),
    ];
    let output = command::run(&worktree.root, &words, 256).await.ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|oid| !oid.is_empty())
}

/// `rev-list --count <from>..<to>`.
pub(crate) async fn count(worktree: &Worktree, from: &str, to: &str) -> Option<u64> {
    let range = format!("{from}..{to}");
    let output = command::run(
        &worktree.root,
        &args(["rev-list", "--count", range.as_str()]),
        64,
    )
    .await
    .ok()?;
    if !output.succeeded() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub(crate) async fn is_ancestor(worktree: &Worktree, ancestor: &str, of: &str) -> bool {
    command::run(
        &worktree.root,
        &args(["merge-base", "--is-ancestor", ancestor, of]),
        64,
    )
    .await
    .is_ok_and(|output| output.succeeded())
}

/// FETCH_HEAD's modification time, in nanoseconds, as text.
fn fetch_head_stamp(worktree: &Worktree) -> Option<String> {
    [&worktree.git_dir, &worktree.common_dir]
        .into_iter()
        .filter_map(|dir| {
            std::fs::metadata(dir.join("FETCH_HEAD"))
                .ok()?
                .modified()
                .ok()
        })
        .max()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos().to_string())
}

fn conflicted_paths(observed: &Observed) -> Vec<String> {
    observed
        .porcelain
        .records
        .iter()
        .filter(|record| matches!(record.kind, RecordKind::Unmerged { .. }))
        .take(MAX_FILES)
        .map(|record| status::display(&record.path))
        .collect()
}

fn same_tree(expected: &GitPreconditions, now: &GitPreconditions) -> bool {
    expected.index == now.index && expected.worktree == now.worktree
}

const TREE_CHANGED: &str =
    "Files changed on your Mac after you reviewed them. Refresh and review it again.";

fn plan(words: Vec<OsString>, timeout: Duration) -> Plan {
    Plan {
        args: words,
        input: None,
        timeout,
        targets: Vec::new(),
        extras: Extras::default(),
        source: None,
        fetch_head: None,
        destination: None,
    }
}

/// Checks a fetch, update, Continue or Abort and builds its invocation.
/// Branch and HEAD were already checked.
pub(crate) async fn check(
    mutation: &Mutation,
    expected: &GitPreconditions,
    worktree: &Worktree,
    observed: &Observed,
) -> Result<Plan, Outcome> {
    let now = &observed.snapshot.preconditions;
    match mutation {
        Mutation::Fetch { remote } => {
            let known = remotes(worktree).await.map_err(ops::from_git_error)?;
            if !known.iter().any(|(name, _)| name == remote) {
                return Err(Outcome::rejected(
                    "remote_not_found",
                    format!("There is no remote named {remote} on your Mac any more."),
                ));
            }
            let mut fetch = plan(
                vec![
                    OsString::from("fetch"),
                    "--progress".into(),
                    "--end-of-options".into(),
                    remote.into(),
                ],
                FETCH_TIMEOUT,
            );
            fetch.fetch_head = fetch_head_stamp(worktree);
            // The upstream's commit before fetching, to say what arrived.
            if let Some(branch) = now.branch.as_deref() {
                if let Some(upstream) = upstream_of(worktree, branch).await {
                    if upstream.remote.as_deref() == Some(remote.as_str()) {
                        fetch.source = ref_oid(worktree, &upstream.reference).await;
                    }
                }
            }
            Ok(fetch)
        }
        Mutation::FastForward {
            upstream,
            upstream_oid,
        }
        | Mutation::Merge {
            upstream,
            upstream_oid,
        }
        | Mutation::Rebase {
            upstream,
            upstream_oid,
        } => {
            if !same_tree(expected, now) {
                return Err(Outcome::refresh_required(TREE_CHANGED));
            }
            if let Some(operation) = &observed.operation {
                return Err(Outcome::rejected(
                    "operation_in_progress",
                    format!(
                        "A {} is already in progress on your Mac. Continue or abort it first.",
                        operation_noun(operation.kind)
                    ),
                ));
            }
            let Some(branch) = now.branch.as_deref() else {
                return Err(Outcome::rejected(
                    "no_upstream",
                    "HEAD is detached on your Mac, so there is no branch to update.",
                ));
            };
            let Some(current) = upstream_of(worktree, branch).await else {
                return Err(Outcome::rejected(
                    "no_upstream",
                    format!("{branch} has no upstream branch on your Mac."),
                ));
            };
            if current.name != *upstream {
                return Err(Outcome::refresh_required(format!(
                    "{branch} now tracks {} instead of {upstream}. Refresh and review it again.",
                    current.name
                )));
            }
            match ref_oid(worktree, &current.reference).await {
                None => {
                    return Err(Outcome::rejected(
                        "no_upstream",
                        format!("{upstream} no longer exists on your Mac. Fetch to see the remote again."),
                    ))
                }
                Some(oid) if oid != *upstream_oid => {
                    return Err(Outcome::refresh_required(format!(
                        "{upstream} moved on your Mac after you reviewed it. Refresh and review the incoming commits again."
                    )))
                }
                Some(_) => {}
            }
            let (words, timeout) = match mutation {
                Mutation::FastForward { .. } => (
                    vec![
                        OsString::from("merge"),
                        "--ff-only".into(),
                        "--no-autostash".into(),
                        "--no-edit".into(),
                        "--end-of-options".into(),
                        upstream_oid.into(),
                    ],
                    UPDATE_TIMEOUT,
                ),
                Mutation::Merge { .. } => {
                    let what = if current.remote.is_some() {
                        "remote-tracking branch"
                    } else {
                        "branch"
                    };
                    (
                        vec![
                            OsString::from("merge"),
                            "--no-autostash".into(),
                            "--no-edit".into(),
                            format!("--message=Merge {what} '{upstream}' into {branch}").into(),
                            "--end-of-options".into(),
                            upstream_oid.into(),
                        ],
                        UPDATE_TIMEOUT,
                    )
                }
                _ => (
                    vec![
                        OsString::from("rebase"),
                        "--no-autostash".into(),
                        "--end-of-options".into(),
                        upstream_oid.into(),
                    ],
                    REBASE_TIMEOUT,
                ),
            };
            let mut update = plan(words, timeout);
            update.source = Some(upstream_oid.clone());
            update.extras.progress = None;
            Ok(update)
        }
        Mutation::ContinueOperation { operation } | Mutation::AbortOperation { operation } => {
            let continuing = matches!(mutation, Mutation::ContinueOperation { .. });
            let name = kind_name(*operation);
            match &observed.operation {
                Some(current) if matches_kind(*operation, current.kind) => {}
                Some(current) => {
                    return Err(Outcome::refresh_required(format!(
                    "A {} is in progress on your Mac, not a {name}. Refresh and review it again.",
                    operation_noun(current.kind)
                )))
                }
                None => {
                    return Err(Outcome::rejected(
                        "no_operation",
                        format!("There is no {name} in progress on your Mac any more."),
                    ))
                }
            }
            if !same_tree(expected, now) {
                return Err(Outcome::refresh_required(TREE_CHANGED));
            }
            if continuing {
                let conflicted = conflicted_paths(observed);
                if !conflicted.is_empty() {
                    let mut refusal = Outcome::rejected(
                        "unresolved_conflicts",
                        format!(
                            "{} still {} conflicts. Resolve {} on your Mac, stage {} here, then Continue.",
                            plural(conflicted.len(), "file"),
                            if conflicted.len() == 1 { "has" } else { "have" },
                            if conflicted.len() == 1 { "it" } else { "them" },
                            if conflicted.len() == 1 { "it" } else { "them" },
                        ),
                    );
                    refusal.outcome.files = conflicted;
                    return Err(refusal);
                }
                let mut words = plan(
                    vec![OsString::from(name), "--continue".into()],
                    CONTROL_TIMEOUT,
                );
                words.extras.accept_default_message = true;
                Ok(words)
            } else {
                Ok(plan(
                    vec![OsString::from(name), "--abort".into()],
                    CONTROL_TIMEOUT,
                ))
            }
        }
        _ => unreachable!("integrate::check handles fetch, updates and operation control"),
    }
}

pub(crate) fn operation_noun(kind: GitOperationKind) -> &'static str {
    match kind {
        GitOperationKind::Merge => "merge",
        GitOperationKind::Rebase => "rebase",
        GitOperationKind::CherryPick => "cherry-pick",
        GitOperationKind::Revert => "revert",
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn commits(count: u64) -> String {
    plural(count as usize, "commit")
}

/// Refuses to stage conflicted files that still contain conflict markers,
/// unless the user confirmed it.
pub(crate) fn conflict_markers(worktree: &Worktree, paths: &[Vec<u8>]) -> Option<Outcome> {
    let marked = paths
        .iter()
        .filter(|path| has_markers(&worktree.root.join(super::path_of(path))))
        .take(MAX_FILES)
        .map(|path| status::display(path))
        .collect::<Vec<_>>();
    if marked.is_empty() {
        return None;
    }
    let mut refusal = Outcome::rejected(
        "conflict_markers",
        if marked.len() == 1 {
            format!(
                "{} still contains conflict markers. Stage it anyway only if they belong there.",
                marked[0]
            )
        } else {
            format!(
                "{} files still contain conflict markers. Stage them anyway only if they belong there.",
                marked.len()
            )
        },
    );
    refusal.outcome.files = marked;
    Some(refusal)
}

/// Whether a file has a line Git writes around a conflict.
fn has_markers(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(MARKER_SCAN_LIMIT)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    bytes.split(|byte| *byte == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ")
            || line.starts_with(b">>>>>>> ")
            || line == b"<<<<<<<"
            || line == b">>>>>>>"
            || line == b"======="
            || line.starts_with(b"||||||| ")
    })
}

/// What Git did, read back after it exited.
pub(crate) async fn outcome(
    mutation: &Mutation,
    worktree: &Worktree,
    before: &Before,
    output: &GitOutput,
) -> Outcome {
    let head = ops::rev(worktree, "HEAD").await;
    let branch = ops::current_branch(worktree).await;
    let after = match observe(worktree).await {
        Ok(after) => after,
        Err(_) => {
            return Outcome::uncertain(
                "interrupted",
                "Git finished, but Latch couldn't read the repository afterwards. Refresh to see what changed.",
            )
            .with_output(output)
        }
    };
    let branch_name = before.branch.as_deref().unwrap_or("HEAD");
    let outcome = match mutation {
        Mutation::Fetch { remote } => fetched(worktree, remote, before, output).await,
        Mutation::FastForward { upstream, .. }
        | Mutation::Merge { upstream, .. }
        | Mutation::Rebase { upstream, .. } => {
            if let Some(operation) = &after.operation {
                stopped(operation, &after, output)
            } else if head != before.head && !output.succeeded() {
                Outcome::uncertain(
                    "git_refused",
                    "Git reported a failure, but the branch moved. Refresh to see what happened.",
                )
                .with_output(output)
            } else if !output.succeeded() {
                update_refusal(mutation, worktree, branch_name, upstream, output)
                    .await
                    .with_output(output)
            } else if head == before.head {
                Outcome::completed(
                    "up_to_date",
                    format!("{branch_name} already has everything from {upstream}."),
                )
            } else {
                let moved = match (&before.head, &head) {
                    (Some(from), Some(to)) => count(worktree, from, to).await,
                    _ => None,
                };
                match mutation {
                    Mutation::FastForward { .. } => Outcome::completed(
                        "fast_forwarded",
                        match moved {
                            Some(moved) => format!(
                                "Fast-forwarded {branch_name} by {} to {}.",
                                commits(moved),
                                ops::short(head.as_deref())
                            ),
                            None => format!(
                                "Fast-forwarded {branch_name} to {}.",
                                ops::short(head.as_deref())
                            ),
                        },
                    ),
                    Mutation::Merge { .. } if head == before.source => Outcome::completed(
                        "fast_forwarded",
                        format!(
                            "{branch_name} could fast-forward, so Git moved it to {upstream} without a merge commit."
                        ),
                    ),
                    Mutation::Merge { .. } => {
                        let mut done = Outcome::completed(
                            "merged",
                            format!(
                                "Merged {upstream} into {branch_name} as {}.",
                                ops::short(head.as_deref())
                            ),
                        );
                        done.outcome.commit = head.clone();
                        done
                    }
                    _ => Outcome::completed(
                        "rebased",
                        format!("Rebased {branch_name} onto {upstream}."),
                    ),
                }
            }
        }
        Mutation::ContinueOperation { operation } => {
            let name = kind_name(*operation);
            match &after.operation {
                // A rebase that went on to stop at a later commit.
                Some(current)
                    if output.succeeded()
                        || current.stopped_at
                            != before.operation.as_ref().and_then(|o| o.stopped_at.clone())
                        || head != before.head =>
                {
                    stopped(current, &after, output)
                }
                Some(_) => continue_refusal(worktree, name).await.with_output(output),
                None => {
                    let mut done = match operation {
                        GitControlledOperation::Merge => Outcome::completed(
                            "continued",
                            format!(
                                "Committed the merge on {} as {}.",
                                branch.as_deref().unwrap_or("HEAD"),
                                ops::short(head.as_deref())
                            ),
                        ),
                        GitControlledOperation::Rebase => Outcome::completed(
                            "continued",
                            format!(
                                "Finished the rebase of {}.",
                                branch.as_deref().unwrap_or("HEAD")
                            ),
                        ),
                    };
                    done.outcome.commit = head.clone();
                    if !output.succeeded() {
                        done = done.with_output(output);
                    }
                    done
                }
            }
        }
        Mutation::AbortOperation { operation } => {
            let name = kind_name(*operation);
            if after.operation.is_none() {
                Outcome::completed(
                    "aborted",
                    format!(
                        "Aborted the {name}. {} is back at {}.",
                        branch.as_deref().unwrap_or("HEAD"),
                        ops::short(head.as_deref())
                    ),
                )
            } else {
                Outcome::rejected(
                    "git_refused",
                    format!("Git did not abort the {name}. Its message says why."),
                )
                .with_output(output)
            }
        }
        _ => unreachable!("integrate::outcome handles fetch, updates and operation control"),
    };
    outcome.at(head, branch)
}

/// Git stopped a merge or rebase part-way: on conflicts, or at a commit that
/// needs the user (an `edit`, or a commit that became empty).
fn stopped(operation: &GitOperation, after: &Observed, output: &GitOutput) -> Outcome {
    let conflicted = conflicted_paths(after);
    let noun = operation_noun(operation.kind);
    let position = match (operation.step, operation.total) {
        (Some(step), Some(total)) => format!(" at commit {step} of {total}"),
        _ => String::new(),
    };
    let mut stop = if conflicted.is_empty() {
        Outcome::completed(
            "stopped",
            format!(
                "Git stopped the {noun}{position} and needs you. Its message says what it needs; finish on your Mac, then Continue or Abort here."
            ),
        )
    } else {
        Outcome::completed(
            "stopped",
            format!(
                "Git stopped the {noun}{position} on conflicts in {}. Resolve them on your Mac or in a terminal, stage them here, then Continue.",
                plural(conflicted.len(), "file")
            ),
        )
    };
    stop.outcome.files = conflicted;
    stop.with_output(output)
}

async fn fetched(
    worktree: &Worktree,
    remote: &str,
    before: &Before,
    output: &GitOutput,
) -> Outcome {
    if !output.succeeded() {
        return fetch_refusal(remote, output).with_output(output);
    }
    let Some(branch) = before.branch.as_deref() else {
        return Outcome::completed("fetched", format!("Fetched {remote}."));
    };
    let Some(upstream) = upstream_of(worktree, branch).await else {
        return Outcome::completed("fetched", format!("Fetched {remote}."));
    };
    if upstream.remote.as_deref() != Some(remote) {
        return Outcome::completed("fetched", format!("Fetched {remote}."));
    }
    let now = ref_oid(worktree, &upstream.reference).await;
    let message = match (&before.source, &now) {
        (Some(old), Some(new)) if old == new => {
            format!("Fetched {remote} · no new commits on {}", upstream.name)
        }
        (Some(old), Some(new)) => match count(worktree, old, new).await {
            Some(arrived) => format!(
                "Fetched {remote} · {} on {}",
                plural(arrived as usize, "new commit"),
                upstream.name
            ),
            None => format!("Fetched {remote} · {} moved", upstream.name),
        },
        (None, Some(_)) => format!("Fetched {remote} · {} is now known", upstream.name),
        (_, None) => format!(
            "Fetched {remote} · {} no longer exists there",
            upstream.name
        ),
    };
    Outcome::completed("fetched", message)
}

fn fetch_refusal(remote: &str, output: &GitOutput) -> Outcome {
    let text = output.stderr_text();
    let has = |needle: &str| text.contains(needle);
    if has("Authentication failed")
        || has("could not read Username")
        || has("could not read Password")
        || has("terminal prompts disabled")
        || has("Permission denied")
        || has("Host key verification failed")
        || has("invalid credentials")
        || has("HTTP Basic: Access denied")
    {
        return Outcome::rejected(
            "auth_failed",
            format!(
                "Git on your Mac has no working credentials for {remote}. Set them up on the Mac (for example by fetching once in Terminal), then try again."
            ),
        );
    }
    if has("Could not resolve host")
        || has("Connection refused")
        || has("Connection timed out")
        || has("Operation timed out")
        || has("Network is unreachable")
        || has("Could not connect")
        || has("unable to access")
    {
        return Outcome::rejected(
            "network_failed",
            format!("Your Mac couldn't reach {remote}. Nothing was changed."),
        );
    }
    if has("does not appear to be a git repository") || has("not found") {
        return Outcome::rejected(
            "remote_not_found",
            format!("{remote} could not be found where your Mac's Git config says it is."),
        );
    }
    Outcome::rejected(
        "git_refused",
        format!("Git could not fetch {remote}. Its message says why."),
    )
}

async fn update_refusal(
    mutation: &Mutation,
    worktree: &Worktree,
    branch: &str,
    upstream: &str,
    output: &GitOutput,
) -> Outcome {
    let text = format!(
        "{}\n{}",
        output.stderr_text(),
        String::from_utf8_lossy(&output.stdout)
    );
    let has = |needle: &str| text.contains(needle);
    if has("Not possible to fast-forward")
        || has("Diverging branches can't be fast-forwarded")
        || has("not possible to fast-forward")
    {
        return Outcome::rejected(
            "not_fast_forward",
            format!(
                "{branch} and {upstream} have both moved, so Git won't fast-forward. Choose Merge or Rebase."
            ),
        );
    }
    if has("would be overwritten by") {
        let mut refusal = Outcome::rejected(
            "would_overwrite",
            "Git won't do this because it would overwrite changes on your Mac. Commit them first; Latch doesn't stash or discard anything.",
        );
        refusal.outcome.files = ops::overwritten_files(&text);
        return refusal;
    }
    if has("You have unstaged changes")
        || has("Your index contains uncommitted changes")
        || has("Please commit or stash them")
        || has("cannot rebase")
    {
        return Outcome::rejected(
            "local_changes",
            "Git won't rebase while there are uncommitted changes. Commit them first; Latch doesn't stash or discard anything.",
        );
    }
    let hooks: &[&str] = match mutation {
        Mutation::Rebase { .. } => &["pre-rebase"],
        _ => &["pre-merge-commit", "commit-msg", "prepare-commit-msg"],
    };
    for hook in hooks {
        if ops::hook_installed(worktree, hook).await {
            return Outcome::rejected(
                "hook_failed",
                "A hook on your Mac stopped Git. Nothing was changed. Its output says why.",
            );
        }
    }
    Outcome::rejected("git_refused", "Git refused. Its message says why.")
}

async fn continue_refusal(worktree: &Worktree, name: &str) -> Outcome {
    for hook in ["pre-commit", "commit-msg", "prepare-commit-msg"] {
        if ops::hook_installed(worktree, hook).await {
            return Outcome::rejected(
                "hook_failed",
                format!(
                    "A commit hook on your Mac stopped Git from continuing the {name}. Its output says why."
                ),
            );
        }
    }
    Outcome::rejected(
        "git_refused",
        format!(
            "Git couldn't continue the {name}. Its message says what it needs; finish it on your Mac, or Abort here."
        ),
    )
}

/// Decides an interrupted fetch, update, Continue or Abort from the
/// repository as it is now. Never `completed` unless the repository shows
/// the whole result; a merge or rebase left in progress after an
/// interruption is uncertain, because Git may have stopped because it was
/// killed rather than on a conflict.
pub(crate) async fn reconcile(
    mutation: &Mutation,
    before: &Before,
    worktree: &Worktree,
    observed: &Observed,
    cause: Interruption,
) -> Outcome {
    let now = &observed.snapshot.preconditions;
    let unmoved = now.head == before.head && now.branch == before.branch;
    let operation = observed.operation.as_ref();
    match mutation {
        Mutation::Fetch { remote } => {
            // Git empties FETCH_HEAD as a fetch starts and fills it at the
            // end, so only a rewritten, non-empty one shows a finished
            // fetch. One Latch stopped at its deadline stays uncertain:
            // it was killed somewhere along the way.
            let stamp = fetch_head_stamp(worktree);
            let filled = [&worktree.git_dir, &worktree.common_dir]
                .into_iter()
                .any(|dir| {
                    std::fs::metadata(dir.join("FETCH_HEAD")).is_ok_and(|meta| meta.len() > 0)
                });
            if cause == Interruption::Interrupted
                && filled
                && stamp.is_some()
                && stamp != before.fetch_head
            {
                Outcome::completed(
                    "fetched",
                    format!("Fetched {remote}. Latch was interrupted, but the fetch had finished."),
                )
            } else {
                Outcome::uncertain(
                    if cause == Interruption::TimedOut {
                        "timed_out"
                    } else {
                        "interrupted"
                    },
                    format!(
                        "The fetch from {remote} may not have finished. Fetching again is safe."
                    ),
                )
            }
        }
        Mutation::FastForward { .. } | Mutation::Merge { .. } | Mutation::Rebase { .. } => {
            let source = before.source.as_deref();
            if operation.is_some() {
                ops::uncertain(cause)
            } else if unmoved {
                Outcome::rejected(
                    "not_applied",
                    "Nothing was changed: the branch is where it was before.",
                )
            } else if now.branch != before.branch {
                ops::uncertain(cause)
            } else {
                let head = now.head.as_deref();
                let done = match mutation {
                    Mutation::FastForward { .. } => head == source,
                    Mutation::Merge { .. } => {
                        head == source
                            || (ops::rev(worktree, "HEAD^1").await == before.head
                                && ops::rev(worktree, "HEAD^2").await.as_deref() == source)
                    }
                    _ => match (source, head) {
                        (Some(source), Some(head)) => is_ancestor(worktree, source, head).await,
                        _ => false,
                    },
                };
                if done {
                    let code = match mutation {
                        Mutation::FastForward { .. } => "fast_forwarded",
                        Mutation::Merge { .. } => "merged",
                        _ => "rebased",
                    };
                    Outcome::completed(code, "Git had finished before Latch was interrupted.")
                } else {
                    ops::uncertain(cause)
                }
            }
        }
        Mutation::ContinueOperation { operation: kind } => {
            let previous = before.operation.as_ref();
            match operation {
                Some(current)
                    if unmoved
                        && current.stopped_at == previous.and_then(|o| o.stopped_at.clone()) =>
                {
                    Outcome::rejected(
                        "not_applied",
                        format!("The {} did not continue.", kind_name(*kind)),
                    )
                }
                Some(_) => ops::uncertain(cause),
                None => {
                    let finished = match kind {
                        GitControlledOperation::Merge => {
                            ops::rev(worktree, "HEAD^1").await == before.head
                                && previous.and_then(|o| o.merge_head.clone()).is_some()
                                && ops::rev(worktree, "HEAD^2").await
                                    == previous.and_then(|o| o.merge_head.clone())
                        }
                        GitControlledOperation::Rebase => {
                            previous.and_then(|o| o.head_name.clone()).is_some()
                                && now.branch == previous.and_then(|o| o.head_name.clone())
                        }
                    };
                    if finished {
                        Outcome::completed(
                            "continued",
                            format!("The {} finished.", kind_name(*kind)),
                        )
                    } else {
                        ops::uncertain(cause)
                    }
                }
            }
        }
        Mutation::AbortOperation { operation: kind } => {
            let previous = before.operation.as_ref();
            match operation {
                Some(_) if unmoved => Outcome::rejected(
                    "not_applied",
                    format!("The {} is still in progress.", kind_name(*kind)),
                ),
                Some(_) => ops::uncertain(cause),
                None => {
                    let restored = match kind {
                        GitControlledOperation::Merge => now.head == before.head,
                        GitControlledOperation::Rebase => {
                            previous.and_then(|o| o.head_name.clone()).is_some()
                                && now.branch == previous.and_then(|o| o.head_name.clone())
                        }
                    };
                    if restored {
                        Outcome::completed(
                            "aborted",
                            format!("The {} was aborted.", kind_name(*kind)),
                        )
                    } else {
                        ops::uncertain(cause)
                    }
                }
            }
        }
        _ => unreachable!("integrate::reconcile handles fetch, updates and operation control"),
    }
}

/// Runs a fetch with progress. Split out so `ops::execute` stays one place
/// for every mutation.
pub(crate) fn wants_progress(mutation: &Mutation) -> bool {
    matches!(
        mutation,
        Mutation::Fetch { .. }
            | Mutation::Push { .. }
            | Mutation::FastForward { .. }
            | Mutation::Merge { .. }
            | Mutation::Rebase { .. }
            | Mutation::ContinueOperation { .. }
    )
}
