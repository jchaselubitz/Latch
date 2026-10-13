//! The Git mutations a phone may ask for, each checked against the state it
//! was reviewed in and run with a fixed argument list.
//!
//! `check` runs under the worktree lock on a fresh observation and either
//! refuses (refresh required, or a state Git would act on differently) or
//! returns the exact invocation. `execute` runs it and reads back what Git
//! did. `reconcile` decides an interrupted or timed-out request from the
//! repository alone, and says "uncertain" whenever the repository does not
//! settle the question.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::command::{self, args, Extras, GitOutput, ProgressSink};
use super::receipts::Before;
use super::status::{self, RecordKind};
use super::{GitError, Observed, Worktree};
use crate::cli::serve::contract::{
    GitChangeArea, GitControlledOperation, GitMutationKind, GitMutationOutcome, GitPreconditions,
    GitReceiptState,
};

/// Index changes stay well under this even in a large repository.
const INDEX_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 20 } else { 60 });
/// Commit hooks (formatters, linters, tests) can be slow; past this the
/// commit is reconciled as timed out rather than waited on.
/// Tests use a short deadline so a hung hook can be shown being stopped.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 4 } else { 120 });
/// A checkout of a large tree, plus post-checkout hooks.
const BRANCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Bytes of Git output kept on a receipt.
pub(crate) const OUTPUT_LIMIT: usize = 16 * 1024;
/// Stdout read from a mutating command; only kept as diagnostic output.
const STDOUT_LIMIT: usize = 64 * 1024;
/// Files named by one refusal.
const MAX_REFUSAL_FILES: usize = 200;

/// One whole file the phone selected, as status listed it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileTarget {
    /// Exact path bytes, hex-encoded as status served them.
    pub path_key: String,
    pub area: GitChangeArea,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Selection {
    Files(Vec<FileTarget>),
    /// Every entry listed in these areas.
    All(Vec<GitChangeArea>),
}

/// A typed mutation. Nothing in it reaches Git as an option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Mutation {
    Stage {
        selection: Selection,
        /// Stage conflicted files even if they still contain conflict
        /// markers. Left out when false, so a slice-2 request digests as it
        /// always did.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        confirm_conflict_markers: bool,
    },
    Unstage {
        selection: Selection,
    },
    Commit {
        summary: String,
        description: Option<String>,
    },
    CreateBranch {
        name: String,
        start_point: String,
        start_oid: String,
        switch: bool,
    },
    SwitchBranch {
        name: String,
        oid: String,
    },
    Fetch {
        remote: String,
    },
    FastForward {
        upstream: String,
        upstream_oid: String,
    },
    Merge {
        upstream: String,
        upstream_oid: String,
    },
    Rebase {
        upstream: String,
        upstream_oid: String,
    },
    ContinueOperation {
        operation: GitControlledOperation,
    },
    AbortOperation {
        operation: GitControlledOperation,
    },
    /// The reviewed source commit to the reviewed branch on a remote.
    Push {
        remote: String,
        remote_url: Option<String>,
        destination: String,
        source_branch: String,
        source_oid: String,
        destination_oid: Option<String>,
    },
}

impl Mutation {
    pub fn kind(&self) -> GitMutationKind {
        match self {
            Self::Stage { .. } => GitMutationKind::Stage,
            Self::Unstage { .. } => GitMutationKind::Unstage,
            Self::Commit { .. } => GitMutationKind::Commit,
            Self::CreateBranch { .. } => GitMutationKind::CreateBranch,
            Self::SwitchBranch { .. } => GitMutationKind::SwitchBranch,
            Self::Fetch { .. } => GitMutationKind::Fetch,
            Self::FastForward { .. } => GitMutationKind::FastForward,
            Self::Merge { .. } => GitMutationKind::Merge,
            Self::Rebase { .. } => GitMutationKind::Rebase,
            Self::ContinueOperation { .. } => GitMutationKind::ContinueOperation,
            Self::AbortOperation { .. } => GitMutationKind::AbortOperation,
            Self::Push { .. } => GitMutationKind::Push,
        }
    }

    /// Fetch, update, Continue and Abort can run for minutes; their
    /// submission answers early with the pending receipt rather than holding
    /// the request open.
    pub fn answers_early(&self) -> bool {
        matches!(
            self,
            Self::Fetch { .. }
                | Self::FastForward { .. }
                | Self::Merge { .. }
                | Self::Rebase { .. }
                | Self::ContinueOperation { .. }
                | Self::AbortOperation { .. }
                | Self::Push { .. }
        )
    }

    /// Fetch, updates, Continue and Abort (`integrate`).
    fn integrates(&self) -> bool {
        self.answers_early() && !self.pushes()
    }

    pub(crate) fn pushes(&self) -> bool {
        matches!(self, Self::Push { .. })
    }
}

/// What a request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub state: GitReceiptState,
    pub outcome: Box<GitMutationOutcome>,
}

impl Outcome {
    fn new(state: GitReceiptState, code: &str, message: impl Into<String>) -> Self {
        Self {
            state,
            outcome: Box::new(GitMutationOutcome {
                code: code.to_owned(),
                message: message.into(),
                git_output: None,
                git_output_truncated: false,
                head: None,
                branch: None,
                commit: None,
                files: Vec::new(),
                reconciled: false,
            }),
        }
    }

    pub fn completed(code: &str, message: impl Into<String>) -> Self {
        Self::new(GitReceiptState::Completed, code, message)
    }

    pub fn rejected(code: &str, message: impl Into<String>) -> Self {
        Self::new(GitReceiptState::Rejected, code, message)
    }

    pub fn uncertain(code: &str, message: impl Into<String>) -> Self {
        Self::new(GitReceiptState::Uncertain, code, message)
    }

    pub fn refresh_required(message: impl Into<String>) -> Self {
        Self::rejected("refresh_required", message)
    }

    pub(crate) fn with_output(mut self, output: &GitOutput) -> Self {
        let (text, truncated) = combined_output(output);
        self.outcome.git_output = (!text.is_empty()).then_some(text);
        self.outcome.git_output_truncated = truncated;
        self
    }

    pub(crate) fn at(mut self, head: Option<String>, branch: Option<String>) -> Self {
        self.outcome.head = head;
        self.outcome.branch = branch;
        self
    }

    pub fn reconciled(mut self) -> Self {
        self.outcome.reconciled = true;
        self
    }
}

/// Git's stderr then stdout, as text, bounded.
fn combined_output(output: &GitOutput) -> (String, bool) {
    let mut bytes = output.stderr.clone();
    if !output.stdout.is_empty() {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&output.stdout);
    }
    let mut truncated = output.truncated;
    if bytes.len() > OUTPUT_LIMIT {
        bytes.truncate(OUTPUT_LIMIT);
        truncated = true;
    }
    let text = String::from_utf8_lossy(&bytes).trim_end().to_owned();
    (text, truncated)
}

/// An invocation `check` approved.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) args: Vec<OsString>,
    /// NUL-separated pathspecs for `--pathspec-from-file=-`.
    pub(crate) input: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
    /// Paths the operation acts on, for the receipt and reconciliation.
    pub targets: Vec<Vec<u8>>,
    pub(crate) extras: Extras,
    /// The commit an update moves toward, or the upstream's commit before a
    /// fetch; recorded on the receipt for reconciliation.
    pub source: Option<String>,
    /// FETCH_HEAD's time before a fetch.
    pub fetch_head: Option<String>,
    /// The destination's commit as last fetched before a push.
    pub destination: Option<String>,
}

impl Plan {
    fn index(
        args: Vec<OsString>,
        input: Option<Vec<u8>>,
        timeout: Duration,
        targets: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            args,
            input,
            timeout,
            targets,
            extras: Extras::default(),
            source: None,
            fetch_head: None,
            destination: None,
        }
    }
}

const CHANGED_SINCE: &str =
    "The repository changed on your Mac after you reviewed it. Refresh and review it again.";

fn same_head(expected: &GitPreconditions, now: &GitPreconditions) -> bool {
    expected.branch == now.branch && expected.head == now.head
}

/// Checks `mutation` against what the phone reviewed and what the
/// repository is now, and builds the invocation.
pub(crate) async fn check(
    mutation: &Mutation,
    expected: &GitPreconditions,
    worktree: &Worktree,
    observed: &Observed,
) -> Result<Plan, Outcome> {
    let now = &observed.snapshot.preconditions;
    if !same_head(expected, now) {
        return Err(Outcome::refresh_required(
            "The branch or its latest commit changed on your Mac. Refresh and review it again.",
        ));
    }
    if mutation.integrates() {
        return super::integrate::check(mutation, expected, worktree, observed).await;
    }
    if mutation.pushes() {
        return super::push::check(mutation, worktree, observed).await;
    }
    match mutation {
        Mutation::Stage {
            selection,
            confirm_conflict_markers,
        } => {
            let targets = select(selection, expected, observed, true)?;
            if !confirm_conflict_markers {
                let conflicted = conflicted_targets(selection);
                if let Some(refusal) = super::integrate::conflict_markers(worktree, &conflicted) {
                    return Err(refusal);
                }
            }
            Ok(Plan::index(
                args([
                    "add",
                    "--all",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ]),
                Some(pathspecs(&targets)),
                INDEX_TIMEOUT,
                targets,
            ))
        }
        Mutation::Unstage { selection } => {
            let targets = select(selection, expected, observed, false)?;
            // On an unborn branch there is no HEAD to restore from: removing
            // the paths from the index is what unstaging means there.
            let words = if now.head.is_some() {
                args([
                    "restore",
                    "--staged",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ])
            } else {
                args([
                    "rm",
                    "--cached",
                    "-r",
                    "-q",
                    "--ignore-unmatch",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ])
            };
            Ok(Plan::index(
                words,
                Some(pathspecs(&targets)),
                INDEX_TIMEOUT,
                targets,
            ))
        }
        Mutation::Commit {
            summary,
            description,
        } => {
            if observed.truncated
                || super::read_status(worktree)
                    .await
                    .map_err(from_git_error)?
                    .entries_truncated
            {
                return Err(Outcome::rejected("too_many_changes",
                    "Latch cannot show every changed file in this repository. Review and commit on your Mac."));
            }
            if expected.index != now.index {
                return Err(Outcome::refresh_required(
                    "The staged changes changed on your Mac after you reviewed them. Refresh and review the commit again.",
                ));
            }
            if observed.operation.is_some() {
                return Err(Outcome::rejected(
                    "operation_in_progress",
                    "A merge, rebase, cherry-pick or revert is in progress on your Mac. Finish or abort it there first.",
                ));
            }
            let mut words = vec![OsString::from("commit")];
            words.push(format!("--message={summary}").into());
            if let Some(description) = description
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                words.push(format!("--message={description}").into());
            }
            let review = super::commit_review::prepare(worktree)
                .await
                .map_err(from_git_error)?;
            // Copying the index is also followed by a recheck, so an
            // external edit during preparation is not silently reviewed.
            let fresh = super::observe(worktree).await.map_err(from_git_error)?;
            if fresh.snapshot.preconditions.index != expected.index {
                return Err(Outcome::refresh_required(CHANGED_SINCE));
            }
            let mut plan = Plan::index(words, None, COMMIT_TIMEOUT, Vec::new());
            plan.extras.index_file = Some(review.index.clone());
            plan.extras.commit_review = Some(review);
            Ok(plan)
        }
        Mutation::CreateBranch {
            name,
            start_point,
            start_oid,
            switch,
        } => {
            require_branch_name(worktree, name).await?;
            let resolved = if start_point == "HEAD" {
                now.head.clone()
            } else {
                require_branch_name(worktree, start_point).await?;
                branch_oid(worktree, start_point).await
            };
            if resolved.as_deref() != Some(start_oid.as_str()) {
                return Err(Outcome::refresh_required(format!(
                    "{start_point} no longer points at the commit you reviewed. Refresh and review it again."
                )));
            }
            let words = if *switch {
                vec![
                    OsString::from("switch"),
                    "--no-guess".into(),
                    format!("--create={name}").into(),
                    start_oid.into(),
                ]
            } else {
                vec![
                    OsString::from("branch"),
                    "--no-track".into(),
                    "--end-of-options".into(),
                    name.into(),
                    start_oid.into(),
                ]
            };
            Ok(Plan::index(words, None, BRANCH_TIMEOUT, Vec::new()))
        }
        Mutation::SwitchBranch { name, oid } => {
            require_branch_name(worktree, name).await?;
            match branch_oid(worktree, name).await {
                None => {
                    return Err(Outcome::rejected(
                        "not_found",
                        format!("There is no local branch named {name} on your Mac any more."),
                    ))
                }
                Some(current) if current != *oid => {
                    return Err(Outcome::refresh_required(format!(
                        "{name} moved on your Mac after you reviewed it. Refresh and review it again."
                    )))
                }
                Some(_) => {}
            }
            Ok(Plan::index(
                vec![
                    OsString::from("switch"),
                    "--no-guess".into(),
                    "--end-of-options".into(),
                    name.into(),
                ],
                None,
                BRANCH_TIMEOUT,
                Vec::new(),
            ))
        }
        _ => unreachable!("integrating mutations returned above"),
    }
}

/// The paths a stage or unstage acts on, each checked against what the
/// phone was shown.
fn select(
    selection: &Selection,
    expected: &GitPreconditions,
    observed: &Observed,
    staging: bool,
) -> Result<Vec<Vec<u8>>, Outcome> {
    let now = &observed.snapshot.preconditions;
    let records = &observed.porcelain.records;
    let mut targets: Vec<Vec<u8>> = Vec::new();
    let mut push = |path: &[u8]| {
        if !targets.iter().any(|known| known == path) {
            targets.push(path.to_vec());
        }
    };
    match selection {
        Selection::Files(files) => {
            for file in files {
                let allowed = if staging {
                    matches!(
                        file.area,
                        GitChangeArea::Unstaged
                            | GitChangeArea::Untracked
                            | GitChangeArea::Conflicted
                    )
                } else {
                    file.area == GitChangeArea::Staged
                };
                let path = status::unhex(&file.path_key)
                    .filter(|_| allowed)
                    .ok_or_else(|| {
                        Outcome::rejected(
                            "git_refused",
                            "That file cannot be changed that way from the phone.",
                        )
                    })?;
                if staging && observed.snapshot.incomplete_paths.contains(&path) {
                    return Err(Outcome::rejected("too_many_changes",
                        "This folder has too many files to review safely from the phone. Stage it on your Mac."));
                }
                if observed.snapshot.fingerprint(&path, file.area)
                    != Some(file.fingerprint.as_str())
                {
                    return Err(Outcome::refresh_required(format!(
                        "{} changed on your Mac after you reviewed it. Refresh and review it again.",
                        status::display(&path)
                    )));
                }
                push(&path);
                // Unstaging a rename restores its original path too.
                if !staging {
                    if let Some(orig) = records
                        .iter()
                        .find(|record| record.path == path)
                        .and_then(|record| record.orig_path.as_deref())
                    {
                        push(orig);
                    }
                }
            }
        }
        Selection::All(areas) => {
            if staging && !observed.snapshot.incomplete_paths.is_empty() {
                return Err(Outcome::rejected(
                    "too_many_changes",
                    "An untracked folder exceeds Latch's review limit. Stage it on your Mac.",
                ));
            }
            let (token, current) = if staging {
                (&expected.worktree, &now.worktree)
            } else {
                (&expected.index, &now.index)
            };
            if token != current {
                return Err(Outcome::refresh_required(CHANGED_SINCE));
            }
            if observed.truncated {
                return Err(Outcome::rejected(
                    "too_many_changes",
                    "This repository has more changes than Latch can list, so it cannot act on all of them. Use Git on your Mac.",
                ));
            }
            for record in records {
                let listed = match &record.kind {
                    RecordKind::Changed { x, y } => {
                        if staging {
                            areas.contains(&GitChangeArea::Unstaged)
                                && status::change_kind(*y).is_some()
                        } else {
                            status::change_kind(*x).is_some()
                        }
                    }
                    RecordKind::Untracked => staging && areas.contains(&GitChangeArea::Untracked),
                    RecordKind::Unmerged { .. } | RecordKind::Ignored => false,
                };
                if listed {
                    push(&record.path);
                    if !staging {
                        if let Some(orig) = &record.orig_path {
                            push(orig);
                        }
                    }
                }
            }
        }
    }
    if targets.is_empty() {
        return Err(Outcome::rejected(
            if staging {
                "nothing_to_stage"
            } else {
                "nothing_to_unstage"
            },
            if staging {
                "There is nothing there to stage."
            } else {
                "Nothing is staged."
            },
        ));
    }
    Ok(targets)
}

/// The conflicted files a stage names, as path bytes.
fn conflicted_targets(selection: &Selection) -> Vec<Vec<u8>> {
    match selection {
        Selection::Files(files) => files
            .iter()
            .filter(|file| file.area == GitChangeArea::Conflicted)
            .filter_map(|file| status::unhex(&file.path_key))
            .collect(),
        Selection::All(_) => Vec::new(),
    }
}

fn pathspecs(targets: &[Vec<u8>]) -> Vec<u8> {
    let mut input = Vec::new();
    for target in targets {
        input.extend_from_slice(target);
        input.push(0);
    }
    input
}

/// A name that can only be a branch: never an option, a revision
/// expression (`~`, `^`, `@{`, `:`), a pattern, or anything else
/// `git check-ref-format` refuses. Git checks it again before it is used.
pub(crate) fn is_plausible_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && !name.contains("@{")
        && !name.contains("..")
        && !name.contains("//")
        && !name.split('/').any(|part| part.starts_with('.'))
        && name != "HEAD"
        && name != "@"
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}

async fn require_branch_name(worktree: &Worktree, name: &str) -> Result<(), Outcome> {
    let invalid = || {
        Outcome::rejected(
            "invalid_branch_name",
            format!("Git does not allow {name:?} as a branch name."),
        )
    };
    if !is_plausible_branch_name(name) {
        return Err(invalid());
    }
    let words = vec![
        OsString::from("check-ref-format"),
        format!("refs/heads/{name}").into(),
    ];
    match command::run(&worktree.root, &words, 1024).await {
        Ok(output) if output.succeeded() => Ok(()),
        Ok(_) => Err(invalid()),
        Err(error) => Err(from_git_error(error)),
    }
}

/// The commit `refs/heads/<name>` points at, if the branch exists.
pub(crate) async fn branch_oid(worktree: &Worktree, name: &str) -> Option<String> {
    let words = vec![
        OsString::from("rev-parse"),
        "--verify".into(),
        "--quiet".into(),
        "--end-of-options".into(),
        format!("refs/heads/{name}^{{commit}}").into(),
    ];
    let output = command::run(&worktree.root, &words, 256).await.ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|oid| !oid.is_empty())
}

pub(crate) async fn rev(worktree: &Worktree, revision: &str) -> Option<String> {
    let words = args([
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        revision,
    ]);
    let output = command::run(&worktree.root, &words, 256).await.ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|oid| !oid.is_empty())
}

pub(crate) async fn current_branch(worktree: &Worktree) -> Option<String> {
    let output = command::run(
        &worktree.root,
        &args(["symbolic-ref", "--quiet", "--short", "HEAD"]),
        1024,
    )
    .await
    .ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// Why `execute` had to fall back on reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interruption {
    /// Git did not finish before its deadline and was stopped.
    TimedOut,
    /// The gateway stopped (or lost track of Git) while it ran.
    Interrupted,
}

/// Runs an approved plan and reports what Git did.
pub(crate) async fn execute(
    mutation: &Mutation,
    plan: Plan,
    worktree: &Worktree,
    before: &Before,
    progress: Option<ProgressSink>,
) -> Outcome {
    let mut extras = plan.extras;
    if super::integrate::wants_progress(mutation) {
        extras.progress = progress;
    }
    let output = match command::run_with_extras(
        &worktree.root,
        &plan.args,
        plan.input,
        STDOUT_LIMIT,
        plan.timeout,
        extras,
    )
    .await
    {
        Ok(output) => output,
        Err(GitError::TimedOut) => {
            return after_interruption(mutation, before, worktree, Interruption::TimedOut).await
        }
        Err(GitError::GitUnavailable) => return from_git_error(GitError::GitUnavailable),
        Err(_) => {
            return after_interruption(mutation, before, worktree, Interruption::Interrupted).await
        }
    };
    if mutation.integrates() {
        return super::integrate::outcome(mutation, worktree, before, &output).await;
    }
    if mutation.pushes() {
        let outcome = super::push::outcome(mutation, worktree, before, &output).await;
        return outcome.at(rev(worktree, "HEAD").await, current_branch(worktree).await);
    }
    let head = rev(worktree, "HEAD").await;
    let branch = current_branch(worktree).await;
    let outcome = match mutation {
        Mutation::Stage { .. } | Mutation::Unstage { .. } => {
            let staging = matches!(mutation, Mutation::Stage { .. });
            if output.succeeded() {
                let count = plan.targets.len();
                let noun = if count == 1 { "file" } else { "files" };
                if staging {
                    Outcome::completed("staged", format!("Staged {count} {noun}."))
                } else {
                    Outcome::completed("unstaged", format!("Unstaged {count} {noun}."))
                }
            } else {
                Outcome::rejected(
                    "git_refused",
                    if staging {
                        "Git did not stage the files. Its message says why."
                    } else {
                        "Git did not unstage the files. Its message says why."
                    },
                )
                .with_output(&output)
            }
        }
        Mutation::Commit { .. } => {
            let parent = rev(worktree, "HEAD^").await;
            let moved = head != before.head && parent == before.head;
            if moved {
                let mut done = Outcome::completed(
                    "committed",
                    format!(
                        "Committed {} on {}.",
                        short(head.as_deref()),
                        branch.as_deref().unwrap_or("a detached HEAD")
                    ),
                );
                done.outcome.commit = head.clone();
                if !output.succeeded() {
                    done = done.with_output(&output);
                }
                done
            } else if output.succeeded() {
                // Git said yes but HEAD is not one commit past where it was:
                // something else moved it at the same moment.
                Outcome::uncertain(
                    "git_refused",
                    "Git reported a commit, but the branch is not where Latch expected. Refresh to see what happened.",
                )
                .with_output(&output)
            } else {
                commit_refusal(worktree, &output).await.with_output(&output)
            }
        }
        Mutation::CreateBranch { name, switch, .. } => {
            if output.succeeded() {
                let message = if *switch {
                    format!("Created {name} and switched to it.")
                } else {
                    format!("Created {name}.")
                };
                Outcome::completed("branch_created", message)
            } else {
                branch_refusal(&output, name, true).with_output(&output)
            }
        }
        Mutation::SwitchBranch { name, .. } => {
            if output.succeeded() {
                Outcome::completed("switched", format!("Switched to {name}."))
            } else {
                branch_refusal(&output, name, false).with_output(&output)
            }
        }
        _ => unreachable!("integrating mutations returned above"),
    };
    outcome.at(head, branch)
}

async fn after_interruption(
    mutation: &Mutation,
    before: &Before,
    worktree: &Worktree,
    cause: Interruption,
) -> Outcome {
    match super::observe(worktree).await {
        Ok(observed) => reconcile(mutation, Some(before), worktree, &observed, cause).await,
        Err(_) => uncertain(cause),
    }
}

pub(crate) fn uncertain(cause: Interruption) -> Outcome {
    match cause {
        Interruption::TimedOut => Outcome::uncertain(
            "timed_out",
            "Git did not finish in time and was stopped. Refresh to see what it changed before trying again.",
        ),
        Interruption::Interrupted => Outcome::uncertain(
            "interrupted",
            "Latch lost track of Git while it ran. Refresh to see what it changed before trying again.",
        ),
    }
}

/// Decides an interrupted request from the repository as it is now.
/// `before` is `None` when Git was never started for it.
pub(crate) async fn reconcile(
    mutation: &Mutation,
    before: Option<&Before>,
    worktree: &Worktree,
    observed: &Observed,
    cause: Interruption,
) -> Outcome {
    let now = &observed.snapshot.preconditions;
    let at = |outcome: Outcome| {
        outcome
            .at(now.head.clone(), now.branch.clone())
            .reconciled()
    };
    let Some(before) = before else {
        return at(Outcome::rejected(
            "not_applied",
            "Latch stopped before running Git for this request. Nothing was changed.",
        ));
    };
    // A Git that is still running (or died holding the lock) leaves the
    // index lock behind; nothing can be concluded until it is gone.
    if !mutation.pushes() && worktree.git_dir.join("index.lock").exists() {
        return at(uncertain(cause));
    }
    let records = &observed.porcelain.records;
    let listed = |path: &[u8], staged: bool| {
        records.iter().any(|record| {
            let matches = record.path == path || record.orig_path.as_deref() == Some(path);
            matches
                && match &record.kind {
                    RecordKind::Changed { x, y } => {
                        if staged {
                            status::change_kind(*x).is_some()
                        } else {
                            status::change_kind(*y).is_some()
                        }
                    }
                    RecordKind::Untracked => !staged && record.path == path,
                    // Still conflicted: not staged yet.
                    RecordKind::Unmerged { .. } => !staged && record.path == path,
                    RecordKind::Ignored => false,
                }
        })
    };
    if mutation.integrates() {
        return at(super::integrate::reconcile(mutation, before, worktree, observed, cause).await);
    }
    if mutation.pushes() {
        return at(super::push::reconcile(mutation, before, worktree, cause).await);
    }
    let targets = before
        .targets
        .iter()
        .filter_map(|key| status::unhex(key))
        .collect::<Vec<_>>();
    let outcome = match mutation {
        Mutation::Stage { .. } => {
            if !targets.is_empty() && targets.iter().all(|path| !listed(path, false)) {
                Outcome::completed("staged", "The files are staged.")
            } else {
                uncertain(cause)
            }
        }
        Mutation::Unstage { .. } => {
            if !targets.is_empty() && targets.iter().all(|path| !listed(path, true)) {
                Outcome::completed("unstaged", "The files are no longer staged.")
            } else {
                uncertain(cause)
            }
        }
        Mutation::Commit { summary, .. } => {
            if now.head == before.head {
                Outcome::rejected(
                    "not_applied",
                    "No commit was made: the branch is where it was before.",
                )
            } else if rev(worktree, "HEAD^").await == before.head
                && commit_subject(worktree).await.as_deref() == Some(summary.trim())
            {
                let mut done = Outcome::completed(
                    "committed",
                    format!("Committed {}.", short(now.head.as_deref())),
                );
                done.outcome.commit = now.head.clone();
                done
            } else {
                uncertain(cause)
            }
        }
        Mutation::CreateBranch {
            name,
            start_oid,
            switch,
            ..
        } => match branch_oid(worktree, name).await {
            None => Outcome::rejected("not_applied", format!("{name} was not created.")),
            Some(oid) if oid == *start_oid && (!switch || now.branch.as_deref() == Some(name)) => {
                Outcome::completed("branch_created", format!("Created {name}."))
            }
            Some(_) => uncertain(cause),
        },
        Mutation::SwitchBranch { name, .. } => {
            if now.branch.as_deref() == Some(name.as_str()) {
                Outcome::completed("switched", format!("Switched to {name}."))
            } else if now.branch == before.branch && now.head == before.head {
                Outcome::rejected(
                    "not_applied",
                    format!(
                        "Still on {}: the switch did not happen.",
                        before.branch.as_deref().unwrap_or("the same commit")
                    ),
                )
            } else {
                uncertain(cause)
            }
        }
        _ => unreachable!("integrating mutations returned above"),
    };
    at(outcome)
}

async fn commit_subject(worktree: &Worktree) -> Option<String> {
    let output = command::run(
        &worktree.root,
        &args(["log", "-1", "--no-show-signature", "--format=%s", "HEAD"]),
        4096,
    )
    .await
    .ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Why `git commit` refused, from its own words. Hooks get their own code
/// only when one is actually installed.
async fn commit_refusal(worktree: &Worktree, output: &GitOutput) -> Outcome {
    if output
        .stderr_text()
        .contains("[Latch] commit hook changed the reviewed index")
    {
        return Outcome::refresh_required("A commit hook staged different content on your Mac. No commit was made; review and stage its file edits before trying again.");
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let has = |needle: &str| text.contains(needle);
    if has("nothing to commit")
        || has("no changes added to commit")
        || has("nothing added to commit")
    {
        return Outcome::rejected(
            "nothing_to_commit",
            "Nothing is staged, so there is nothing to commit.",
        );
    }
    if has("Author identity unknown")
        || has("Committer identity unknown")
        || has("Please tell me who you are")
        || has("empty ident name")
        || has("unable to auto-detect email address")
        || has("no email was given")
        || has("no name was given")
    {
        return Outcome::rejected(
            "identity_missing",
            "Git on your Mac has no name and email to commit with. Set user.name and user.email on the Mac.",
        );
    }
    if has("gpg failed to sign")
        || has("failed to sign")
        || has("cannot run gpg")
        || has("error: Couldn't load public key")
        || has("signing failed")
    {
        return Outcome::rejected(
            "signing_failed",
            "Git on your Mac could not sign the commit. Nothing was committed. Check its signing setup on the Mac.",
        );
    }
    for hook in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        if hook_installed(worktree, hook).await {
            return Outcome::rejected(
                "hook_failed",
                "A commit hook on your Mac stopped the commit. Nothing was committed. Its output says why.",
            );
        }
    }
    Outcome::rejected(
        "git_refused",
        "Git did not make the commit. Its message says why.",
    )
}

pub(crate) async fn hook_installed(worktree: &Worktree, hook: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let words = vec![
        OsString::from("rev-parse"),
        "--git-path".into(),
        format!("hooks/{hook}").into(),
    ];
    let Ok(output) = command::run(&worktree.root, &words, 8192).await else {
        return false;
    };
    let line = output
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(&output.stdout)
        .to_vec();
    if !output.succeeded() || line.is_empty() {
        return false;
    }
    let path = std::path::PathBuf::from(OsString::from_vec(line));
    let path = if path.is_absolute() {
        path
    } else {
        worktree.root.join(path)
    };
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Why `git switch` or `git branch` refused.
fn branch_refusal(output: &GitOutput, name: &str, creating: bool) -> Outcome {
    let text = String::from_utf8_lossy(&output.stderr);
    if text.contains("would be overwritten by") {
        let mut refusal = Outcome::rejected(
            "would_overwrite",
            "Git won't switch because it would overwrite changes on your Mac. Commit them first; Latch doesn't stash or discard anything.",
        );
        refusal.outcome.files = overwritten_files(&text);
        return refusal;
    }
    if creating && text.contains("already exists") {
        return Outcome::rejected(
            "branch_exists",
            format!("A branch named {name} already exists."),
        );
    }
    if text.contains("is already used by worktree") || text.contains("is already checked out at") {
        return Outcome::rejected(
            "git_refused",
            format!("{name} is checked out in another worktree on your Mac, so Git won't switch to it here."),
        );
    }
    if text.contains("is not a valid branch name") {
        return Outcome::rejected(
            "invalid_branch_name",
            format!("Git does not allow {name:?} as a branch name."),
        );
    }
    Outcome::rejected("git_refused", "Git refused. Its message says why.")
}

/// The tab-indented file list Git prints under "would be overwritten".
pub(crate) fn overwritten_files(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut listing = false;
    for line in text.lines() {
        if line.contains("would be overwritten by") {
            listing = true;
            continue;
        }
        match line.strip_prefix('\t') {
            Some(file) if listing => {
                if files.len() < MAX_REFUSAL_FILES && !files.iter().any(|known| known == file) {
                    files.push(file.to_owned());
                }
            }
            _ => listing = false,
        }
    }
    files
}

pub(crate) fn short(oid: Option<&str>) -> String {
    oid.map(|oid| oid.chars().take(7).collect())
        .unwrap_or_else(|| "a commit".to_owned())
}

/// A failure to run Git at all, as a refusal: nothing was changed.
pub(crate) fn from_git_error(error: GitError) -> Outcome {
    match error {
        GitError::GitUnavailable => Outcome::rejected(
            "git_refused",
            "Git is not installed on your Mac, or Latch cannot find it.",
        ),
        GitError::TimedOut => {
            Outcome::rejected("git_refused", "Git on your Mac did not answer in time.")
        }
        GitError::NotARepository | GitError::UnavailablePath | GitError::BareRepository => {
            Outcome::refresh_required(
                "The repository is no longer where it was on your Mac. Refresh to find it again.",
            )
        }
        _ => Outcome::rejected(
            "git_refused",
            "Git on your Mac could not read this repository.",
        ),
    }
}
