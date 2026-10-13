//! Pushing a reviewed branch: the preview, the push itself, and deciding a
//! push whose outcome was lost.
//!
//! The preview is read-only and offline. It reviews the current branch's
//! upstream, or a remote and destination the phone named explicitly; a
//! branch without an upstream on a remote is never given a destination
//! silently. The push sends exactly the reviewed commit to exactly the
//! reviewed branch with a normal push: no force, no lease, no tags, no
//! submodules, and nothing from the phone reaches Git as an option. When the
//! remote refuses, its refusal is the answer; nothing is repaired or retried.
//!
//! An interrupted push stays uncertain until the user explicitly checks
//! the reviewed push URL. Local tracking refs and negative remote readings
//! cannot prove a started request never ran, so they never enable a retry.

use std::ffi::OsString;
use std::time::Duration;

use super::command::{self, args, GitOutput};
use super::integrate::{self, redact_url};
use super::ops::{self, Interruption, Mutation, Outcome, Plan};
use super::receipts::Before;
use super::{last_fetched_at, observe, status, worktree, GitError, Observed, Worktree};
use crate::cli::serve::contract::{
    GitCommitSummary, GitPushPreview, GitPushSelectedBy, GitPushSource, GitPushSuggestion,
    GitPushTarget, GitRemote,
};

/// Uploading a large branch over a slow link, plus `pre-push` and the
/// remote's own hooks. Past this the whole process group (including `ssh`
/// or a credential helper) is killed and the push is uncertain.
const PUSH_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 6 } else { 300 });
/// Asking the remote where one branch is.
const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 6 } else { 60 });
/// How long after Git stopped the remote is left to finish a push it had
/// already received, before "unchanged" counts as evidence.
const SETTLE_DELAY: Duration = Duration::from_secs(if cfg!(test) { 0 } else { 10 });
/// Outgoing commits per page.
pub(crate) const PAGE: usize = 50;
/// Stdout bound for one page of `git log`.
const LOG_LIMIT: usize = 256 * 1024;
const CURSOR_PREFIX: &str = "p1";

/// A destination branch name as the phone may send one.
pub(crate) fn is_plausible_destination(name: &str) -> bool {
    ops::is_plausible_branch_name(name)
}

/// The fetch refspec Git writes for a remote by default. A destination's
/// remote-tracking ref is known only under it; any other mapping leaves the
/// destination's last-fetched commit unknown rather than guessed.
async fn tracking_ref(worktree: &Worktree, remote: &str, destination: &str) -> Option<String> {
    let key = format!("remote.{remote}.fetch");
    let output = command::run(
        &worktree.root,
        &args(["config", "--get-all", key.as_str()]),
        64 * 1024,
    )
    .await
    .ok()?;
    let default = format!("refs/heads/*:refs/remotes/{remote}/*");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().trim_start_matches('+'))
        .any(|refspec| refspec == default)
        .then(|| format!("refs/remotes/{remote}/{destination}"))
}

/// The URL a push to `remote` uses (its pushurl if set), credentials removed.
pub(crate) async fn push_url(worktree: &Worktree, remote: &str) -> Option<String> {
    single_push_url(worktree, remote)
        .await
        .map(|url| redact_url(&url))
}

/// Git pushes to every configured push URL. The phone reviews one target,
/// so refuse a multi-target configuration instead of silently sending more.
async fn single_push_url(worktree: &Worktree, remote: &str) -> Option<String> {
    let words = vec![
        OsString::from("remote"),
        "get-url".into(),
        "--push".into(),
        "--all".into(),
        "--".into(),
        remote.into(),
    ];
    let output = command::run(&worktree.root, &words, 16 * 1024).await.ok()?;
    if !output.succeeded() || output.truncated {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    let mut urls = text.lines();
    let url = urls.next().filter(|url| !url.is_empty())?;
    urls.next().is_none().then(|| url.to_owned())
}

/// The remote's default branch as last fetched (`refs/remotes/<remote>/HEAD`).
async fn default_branch(worktree: &Worktree, remote: &str) -> Option<String> {
    let reference = format!("refs/remotes/{remote}/HEAD");
    let output = command::run(
        &worktree.root,
        &args(["symbolic-ref", "--quiet", reference.as_str()]),
        1024,
    )
    .await
    .ok()?;
    let target = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    output
        .succeeded()
        .then(|| {
            target
                .strip_prefix(&format!("refs/remotes/{remote}/"))
                .map(str::to_owned)
        })
        .flatten()
}

/// The branch on the remote the upstream of `branch` names, when that
/// upstream is a branch on a remote (not a local branch).
async fn upstream_destination(worktree: &Worktree, branch: &str) -> Option<(String, String)> {
    if !ops::is_plausible_branch_name(branch) {
        return None;
    }
    let words = vec![
        OsString::from("for-each-ref"),
        "--format=%(upstream:remotename)%00%(upstream:remoteref)".into(),
        format!("refs/heads/{branch}").into(),
    ];
    let output = command::run(&worktree.root, &words, 8192).await.ok()?;
    let line = output.stdout.split(|byte| *byte == b'\n').next()?;
    let fields = line.split(|byte| *byte == 0).collect::<Vec<_>>();
    let [remote, remote_ref] = fields[..] else {
        return None;
    };
    let remote = String::from_utf8(remote.to_vec()).ok()?;
    let destination = String::from_utf8(remote_ref.to_vec())
        .ok()?
        .strip_prefix("refs/heads/")?
        .to_owned();
    (!remote.is_empty()
        && remote != "."
        && integrate::is_plausible_remote_name(&remote)
        && is_plausible_destination(&destination))
    .then_some((remote, destination))
}

pub(crate) fn cursor(source: &str, destination: Option<&str>, skip: usize) -> String {
    format!(
        "{CURSOR_PREFIX}-{source}-{}-{skip}",
        destination.unwrap_or("none")
    )
}

/// The page start a cursor names, if it was made for this source and
/// destination.
pub(crate) fn parse_cursor(cursor: &str, source: &str, destination: Option<&str>) -> Option<usize> {
    let rest = cursor.strip_prefix(CURSOR_PREFIX)?.strip_prefix('-')?;
    let (made_for, skip) = rest.rsplit_once('-')?;
    (made_for == format!("{source}-{}", destination.unwrap_or("none"))).then_some(())?;
    skip.parse().ok()
}

/// The revision arguments naming what a push would send: the source minus
/// the destination as last fetched, or, for a destination this Mac has not
/// seen, minus everything it knows the remote has.
fn outgoing_range(source: &str, destination_oid: Option<&str>, remote: &str) -> Vec<OsString> {
    match destination_oid {
        Some(destination) => vec![format!("{destination}..{source}").into()],
        None => vec![
            source.into(),
            "--not".into(),
            format!("--remotes={remote}").into(),
        ],
    }
}

async fn count_outgoing(
    worktree: &Worktree,
    source: &str,
    destination_oid: Option<&str>,
    remote: &str,
) -> Option<u64> {
    let mut words = args(["rev-list", "--count"]);
    words.extend(outgoing_range(source, destination_oid, remote));
    words.push("--".into());
    let output = command::run(&worktree.root, &words, 64).await.ok()?;
    output
        .succeeded()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .flatten()
}

async fn page(
    worktree: &Worktree,
    source: &str,
    destination_oid: Option<&str>,
    remote: &str,
    skip: usize,
) -> Result<(Vec<GitCommitSummary>, Option<String>), GitError> {
    let mut words = vec![
        OsString::from("log"),
        "--no-show-signature".into(),
        "--no-color".into(),
        "-z".into(),
        "--format=%H%x00%an%x00%cI%x00%s".into(),
        format!("--skip={skip}").into(),
        format!("--max-count={}", PAGE + 1).into(),
    ];
    words.extend(outgoing_range(source, destination_oid, remote));
    words.push("--".into());
    let output = command::run(&worktree.root, &words, LOG_LIMIT).await?;
    if !output.succeeded() && !output.truncated {
        return Err(GitError::Failed);
    }
    let mut commits = super::sync::parse_log(&output.stdout);
    let more = commits.len() > PAGE || output.truncated;
    commits.truncate(PAGE);
    if more && commits.is_empty() {
        return Err(GitError::Failed);
    }
    let next = more.then(|| cursor(source, destination_oid, skip + commits.len()));
    Ok((commits, next))
}

fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `GET /v2/git/push/preview`. `chosen` is the remote and destination the
/// phone named, if any; both or neither.
pub(crate) async fn preview(
    path: &str,
    chosen: Option<(&str, &str)>,
    cursor: Option<&str>,
) -> Result<GitPushPreview, GitError> {
    let worktree = worktree::resolve(path).await?;
    let observed = observe(&worktree).await?;
    let head = status::head(&observed.porcelain.branch);
    let preconditions = observed.snapshot.preconditions.clone();
    let remotes = integrate::remotes(&worktree)
        .await?
        .into_iter()
        .map(|(name, url)| GitRemote { name, url })
        .collect::<Vec<_>>();
    let source = match (&preconditions.branch, &preconditions.head) {
        (Some(branch), Some(oid)) if !head.detached => Some(GitPushSource {
            branch: branch.clone(),
            oid: oid.clone(),
        }),
        _ => None,
    };

    let mut selection_reason = None;
    let mut suggestion = None;
    let selected = match (chosen, &source) {
        (Some((remote, destination)), _) => {
            if remotes.iter().any(|known| known.name == remote) {
                Some((
                    remote.to_owned(),
                    destination.to_owned(),
                    GitPushSelectedBy::Request,
                ))
            } else {
                selection_reason = Some(format!("There is no remote named {remote} on your Mac."));
                None
            }
        }
        (None, Some(source)) => match upstream_destination(&worktree, &source.branch).await {
            Some((remote, destination)) if remotes.iter().any(|known| known.name == remote) => {
                Some((remote, destination, GitPushSelectedBy::Upstream))
            }
            _ => {
                if !remotes.is_empty() {
                    selection_reason = Some(format!(
                        "{} has no upstream on a remote. Choose where to push it.",
                        source.branch
                    ));
                    let remote = remotes
                        .iter()
                        .find(|known| known.name == "origin")
                        .or_else(|| remotes.first())
                        .map(|known| known.name.clone());
                    suggestion = remote.map(|remote| GitPushSuggestion {
                        remote,
                        destination: source.branch.clone(),
                    });
                }
                None
            }
        },
        (None, None) => None,
    };

    let target = match selected {
        Some((remote, destination, selected_by)) => {
            let tracking = tracking_ref(&worktree, &remote, &destination).await;
            let destination_oid = match &tracking {
                Some(reference) => integrate::ref_oid(&worktree, reference).await,
                None => None,
            };
            let default = default_branch(&worktree, &remote).await;
            Some(GitPushTarget {
                remote_url: push_url(&worktree, &remote).await,
                destination_ref: format!("refs/heads/{destination}"),
                tracking_ref: tracking,
                new_branch: destination_oid.is_none(),
                destination_oid,
                selected_by,
                is_default_branch: default.as_deref() == Some(destination.as_str()),
                default_branch: default,
                remote,
                destination,
            })
        }
        None => None,
    };
    let selection_required = target.is_none() && source.is_some() && !remotes.is_empty();

    let (ahead, behind) = match (&source, &target) {
        (Some(source), Some(target)) => {
            let ahead = count_outgoing(
                &worktree,
                &source.oid,
                target.destination_oid.as_deref(),
                &target.remote,
            )
            .await;
            let behind = match &target.destination_oid {
                Some(destination) => {
                    count_outgoing(&worktree, destination, Some(&source.oid), &target.remote).await
                }
                None => None,
            };
            (ahead, behind)
        }
        _ => (None, None),
    };

    let branch = preconditions.branch.as_deref().unwrap_or("HEAD");
    let blocked_reason = if head.detached {
        Some("HEAD is detached on your Mac, so there is no branch to push.".to_owned())
    } else if preconditions.head.is_none() {
        Some(format!("{branch} has no commits yet."))
    } else if remotes.is_empty() {
        Some("This repository has no remotes on your Mac. Add one there first.".to_owned())
    } else if let Some(operation) = &observed.operation {
        Some(format!(
            "A {} is in progress on your Mac. Finish or abort it before pushing.",
            integrate::operation_noun(operation.kind)
        ))
    } else {
        match &target {
            Some(target) if target.remote_url.is_none() => Some(
                "This remote must have exactly one push URL. Configure it on your Mac before pushing.".to_owned()
            ),
            Some(target) if !target.new_branch && ahead == Some(0) => Some(format!(
                "{}/{} already has every commit on {branch}, as last fetched.",
                target.remote, target.destination
            )),
            _ => None,
        }
    };
    let rejection_likely_reason = match (&target, behind) {
        (Some(target), Some(behind)) if behind > 0 => Some(format!(
            "{}/{} has {} that {branch} doesn't (as last fetched), so the remote will probably refuse a normal push. Fetch and update {branch} first.",
            target.remote,
            target.destination,
            plural(behind, "commit")
        )),
        _ => None,
    };

    let (outgoing, next_cursor) = match (&source, &target) {
        (Some(source), Some(target)) if ahead.unwrap_or(0) > 0 => {
            let destination = target.destination_oid.as_deref();
            let skip = match cursor {
                None => 0,
                Some(cursor) => {
                    parse_cursor(cursor, &source.oid, destination).ok_or(GitError::StaleCursor)?
                }
            };
            page(&worktree, &source.oid, destination, &target.remote, skip).await?
        }
        _ if cursor.is_some() => return Err(GitError::StaleCursor),
        _ => (Vec::new(), None),
    };

    Ok(GitPushPreview {
        root: worktree.root_str().to_owned(),
        head,
        source,
        remotes,
        target,
        selection_required,
        selection_reason: if selection_required {
            selection_reason
        } else {
            None
        },
        suggestion: if selection_required { suggestion } else { None },
        last_fetched_at: last_fetched_at(&worktree),
        ahead,
        behind,
        blocked_reason,
        rejection_likely_reason,
        outgoing,
        next_cursor,
        operation: observed.operation,
        preconditions,
    })
}

/// The reviewed push, as a mutation carries it.
pub(crate) struct Reviewed<'a> {
    pub remote: &'a str,
    pub remote_url: Option<&'a str>,
    pub destination: &'a str,
    pub source_branch: &'a str,
    pub source_oid: &'a str,
    pub destination_oid: Option<&'a str>,
}

impl<'a> Reviewed<'a> {
    pub fn of(mutation: &'a Mutation) -> Option<Self> {
        match mutation {
            Mutation::Push {
                remote,
                remote_url,
                destination,
                source_branch,
                source_oid,
                destination_oid,
            } => Some(Self {
                remote,
                remote_url: remote_url.as_deref(),
                destination,
                source_branch,
                source_oid,
                destination_oid: destination_oid.as_deref(),
            }),
            _ => None,
        }
    }
}

/// Checks a push against what the phone reviewed, under the worktree lock.
/// Branch and HEAD were already checked; the index and working tree do not
/// matter to a push and are not required to be unchanged.
pub(crate) async fn check(
    mutation: &Mutation,
    worktree: &Worktree,
    observed: &Observed,
) -> Result<Plan, Outcome> {
    let Some(push) = Reviewed::of(mutation) else {
        unreachable!("push::check handles pushes");
    };
    let now = &observed.snapshot.preconditions;
    if now.branch.as_deref() != Some(push.source_branch) {
        return Err(Outcome::refresh_required(format!(
            "{} is no longer the branch checked out on your Mac. Refresh and review the push again.",
            push.source_branch
        )));
    }
    if ops::branch_oid(worktree, push.source_branch)
        .await
        .as_deref()
        != Some(push.source_oid)
        || now.head.as_deref() != Some(push.source_oid)
    {
        return Err(Outcome::refresh_required(format!(
            "{} moved on your Mac after you reviewed the push. Refresh and review it again.",
            push.source_branch
        )));
    }
    if let Some(operation) = &observed.operation {
        return Err(Outcome::rejected(
            "operation_in_progress",
            format!(
                "A {} is in progress on your Mac. Finish or abort it before pushing.",
                integrate::operation_noun(operation.kind)
            ),
        ));
    }
    let known = integrate::remotes(worktree)
        .await
        .map_err(ops::from_git_error)?;
    if !known.iter().any(|(name, _)| name == push.remote) {
        return Err(Outcome::rejected(
            "remote_not_found",
            format!(
                "There is no remote named {} on your Mac any more.",
                push.remote
            ),
        ));
    }
    let current_url = push_url(worktree, push.remote).await;
    if current_url.is_none() || current_url.as_deref() != push.remote_url {
        return Err(Outcome::refresh_required(format!(
            "The address {} pushes to changed on your Mac. Refresh and review the push again.",
            push.remote
        )));
    }
    if !is_plausible_destination(push.destination)
        || !matches!(
            command::run(
                &worktree.root,
                &[
                    OsString::from("check-ref-format"),
                    format!("refs/heads/{}", push.destination).into()
                ],
                1024
            )
            .await,
            Ok(ref output) if output.succeeded()
        )
    {
        return Err(Outcome::rejected(
            "invalid_branch_name",
            format!(
                "Git does not allow {:?} as a branch name.",
                push.destination
            ),
        ));
    }
    let tracking = tracking_ref(worktree, push.remote, push.destination).await;
    let destination_now = match &tracking {
        Some(reference) => integrate::ref_oid(worktree, reference).await,
        None => None,
    };
    if destination_now.as_deref() != push.destination_oid {
        return Err(Outcome::refresh_required(format!(
            "{}/{} changed on your Mac after you reviewed the push (it was fetched). Refresh and review it again.",
            push.remote, push.destination
        )));
    }
    Ok(Plan {
        args: vec![
            OsString::from("push"),
            "--porcelain".into(),
            "--progress".into(),
            "--no-follow-tags".into(),
            "--recurse-submodules=no".into(),
            "--end-of-options".into(),
            push.remote.into(),
            format!("{}:refs/heads/{}", push.source_oid, push.destination).into(),
        ],
        input: None,
        timeout: PUSH_TIMEOUT,
        targets: Vec::new(),
        extras: Default::default(),
        source: Some(push.source_oid.to_owned()),
        fetch_head: None,
        destination: push.destination_oid.map(str::to_owned),
    })
}

/// One `git push --porcelain` ref line: its flag and its summary.
pub(crate) fn ref_status(stdout: &[u8], destination: &str) -> Option<(char, String)> {
    let wanted = format!("refs/heads/{destination}");
    String::from_utf8_lossy(stdout).lines().find_map(|line| {
        let mut fields = line.splitn(3, '\t');
        let flag = fields.next()?.chars().next()?;
        let refs = fields.next()?;
        let summary = fields.next().unwrap_or_default();
        let (_, to) = refs.rsplit_once(':')?;
        (to == wanted).then(|| (flag, summary.to_owned()))
    })
}

fn named(remote: &str, destination: &str) -> String {
    format!("{remote}/{destination}")
}

/// What `git push` did, from its porcelain report.
pub(crate) async fn outcome(
    mutation: &Mutation,
    worktree: &Worktree,
    before: &Before,
    output: &GitOutput,
) -> Outcome {
    let Some(push) = Reviewed::of(mutation) else {
        unreachable!("push::outcome handles pushes");
    };
    let target = named(push.remote, push.destination);
    let short = ops::short(Some(push.source_oid));
    let pushed = |code: &str, message: String| {
        let mut done = Outcome::completed(code, message);
        done.outcome.commit = Some(push.source_oid.to_owned());
        done
    };
    match ref_status(&output.stdout, push.destination) {
        Some((' ' | '+', _)) => {
            let sent = match before.destination.as_deref() {
                Some(from) => {
                    count_outgoing(worktree, push.source_oid, Some(from), push.remote).await
                }
                None => None,
            };
            pushed(
                "pushed",
                match sent {
                    Some(sent) => format!(
                        "Pushed {} to {target}. It is now at {short}.",
                        plural(sent, "commit")
                    ),
                    None => format!("Pushed {} to {target}.", push.source_branch),
                },
            )
        }
        Some(('*', _)) => pushed(
            "pushed",
            format!(
                "Created {} on {} at {short}.",
                push.destination, push.remote
            ),
        ),
        Some(('=', _)) => pushed(
            "up_to_date",
            format!("{target} already had {short}. Nothing was sent."),
        ),
        Some(('!', summary)) => push_refusal(&target, &summary).with_output(output),
        _ if output.succeeded() => unconfirmed(&target).with_output(output),
        _ => no_ref_refusal(worktree, push.remote, output)
            .await
            .with_output(output),
    }
}

/// Why the remote refused this ref, from the porcelain summary.
fn push_refusal(target: &str, summary: &str) -> Outcome {
    if summary.contains("[remote rejected]") {
        let reason = summary
            .split_once('(')
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map(|(reason, _)| reason.trim().to_owned())
            .filter(|reason| !reason.is_empty());
        return Outcome::rejected(
            "remote_rejected",
            match reason {
                Some(reason) => format!(
                    "The remote refused the push to {target}: {reason}. Nothing was changed there."
                ),
                None => {
                    format!("The remote refused the push to {target}. Nothing was changed there.")
                }
            },
        );
    }
    if summary.contains("fetch first")
        || summary.contains("non-fast-forward")
        || summary.contains("[rejected]")
    {
        return Outcome::rejected(
            "non_fast_forward",
            format!(
                "{target} has commits this Mac doesn't have, so the remote refused a normal push. Fetch and choose how to update, then push again."
            ),
        );
    }
    Outcome::rejected(
        "git_refused",
        format!("Git did not push to {target}. Its message says why."),
    )
}

/// Git failed before reporting any ref: credentials, the network, the
/// remote's address, or a `pre-push` hook.
async fn no_ref_refusal(worktree: &Worktree, remote: &str, output: &GitOutput) -> Outcome {
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
        || has("The requested URL returned error: 403")
        || has("The requested URL returned error: 401")
    {
        return Outcome::rejected(
            "auth_failed",
            format!(
                "Git on your Mac has no working credentials to push to {remote}. Set them up on the Mac (for example by pushing once in Terminal), then try again. Nothing was sent."
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
            format!("Your Mac couldn't reach {remote}. Nothing was sent."),
        );
    }
    if has("does not appear to be a git repository") || has("repository not found") {
        return Outcome::rejected(
            "remote_not_found",
            format!("{remote} could not be found where your Mac's Git config says it is."),
        );
    }
    if ops::hook_installed(worktree, "pre-push").await {
        return Outcome::uncertain(
            "hook_failed",
            "A pre-push hook is installed on your Mac, but Git did not report the destination's result. Check its output and the remote before pushing again.",
        );
    }
    // A disconnect/protocol error can arrive after remote acceptance, with
    // no porcelain ref report. Lack of a success report is not a refusal.
    unconfirmed(remote)
}

fn unconfirmed(target: &str) -> Outcome {
    Outcome::uncertain(
        "push_unconfirmed",
        format!(
            "Latch can't tell from your Mac whether {target} accepted the push. Check the remote before pushing again."
        ),
    )
}

/// Local tracking refs can be changed by a fetch, another client, or a
/// terminal. They cannot settle an interrupted push; only an explicit
/// remote check may prove that the reviewed commit is present.
pub(crate) async fn reconcile(
    mutation: &Mutation,
    _before: &Before,
    _worktree: &Worktree,
    cause: Interruption,
) -> Outcome {
    let push = Reviewed::of(mutation).expect("a push");
    let target = named(push.remote, push.destination);
    match cause {
        Interruption::TimedOut => Outcome::uncertain(
            "timed_out",
            format!(
                "The push to {target} did not finish in time and Git was stopped. The remote may still have accepted it. Check the remote before pushing again."
            ),
        ),
        Interruption::Interrupted => unconfirmed(&target),
    }
}

/// What asking the remote showed about a push.
pub(crate) struct Verdict {
    /// `None`: the evidence does not settle it; the receipt stays as it is.
    pub outcome: Option<Outcome>,
    pub remote_checked: bool,
    pub remote_oid: Option<String>,
    pub message: String,
}

/// Asks the remote where the destination is now (`git ls-remote`, the
/// user's explicit request) and decides the push from that and from what
/// this Mac knows. `git_stopped` says whether Git (and anything it started)
/// is known to be gone; while it might still be running, "unchanged" proves
/// nothing.
pub(crate) async fn verdict(
    mutation: &Mutation,
    _before: Option<&Before>,
    worktree: &Worktree,
    git_stopped: bool,
) -> Verdict {
    let Some(push) = Reviewed::of(mutation) else {
        unreachable!("push::verdict handles pushes");
    };
    let target = named(push.remote, push.destination);
    let short_source = ops::short(Some(push.source_oid));
    let undecided = |remote_checked: bool, remote_oid: Option<String>, message: String| Verdict {
        outcome: None,
        remote_checked,
        remote_oid,
        message,
    };
    let Some(url) = single_push_url(worktree, push.remote).await else {
        return undecided(
            false,
            None,
            "The remote no longer has one push URL. The push is still unconfirmed.".to_owned(),
        );
    };
    if Some(redact_url(&url)).as_deref() != push.remote_url {
        return undecided(false, None, "The push address changed since review. Latch cannot check the original destination; the push is still unconfirmed.".to_owned());
    }
    let words = vec![
        OsString::from("ls-remote"),
        "--refs".into(),
        "--end-of-options".into(),
        url.into(),
        format!("refs/heads/{}", push.destination).into(),
    ];
    let output =
        match command::run_with_timeout(&worktree.root, &words, 64 * 1024, LS_REMOTE_TIMEOUT).await
        {
            Ok(output) => output,
            Err(GitError::TimedOut) => {
                return undecided(
                    false,
                    None,
                    format!(
                        "{} didn't answer in time. The push is still unconfirmed.",
                        push.remote
                    ),
                )
            }
            Err(_) => {
                return undecided(
                    false,
                    None,
                    "Git on your Mac couldn't ask the remote. The push is still unconfirmed."
                        .to_owned(),
                )
            }
        };
    if !output.succeeded() {
        let refusal = no_ref_refusal(worktree, push.remote, &output).await;
        return undecided(
            false,
            None,
            format!(
                "{} The push is still unconfirmed.",
                refusal
                    .outcome
                    .message
                    .trim_end_matches(" Nothing was sent.")
                    .replace("to push to", "to reach")
            ),
        );
    }
    let wanted = format!("refs/heads/{}", push.destination);
    let remote_oid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let (oid, name) = line.split_once('\t')?;
            (name.trim() == wanted).then(|| oid.trim().to_owned())
        })
        .filter(|oid| (40..=64).contains(&oid.len()));

    let settled = |outcome: Outcome, message: String, remote_oid: Option<String>| Verdict {
        outcome: Some(outcome.reconciled()),
        remote_checked: true,
        remote_oid,
        message,
    };
    match remote_oid {
        Some(oid) if oid == push.source_oid => {
            let message = format!("{target} is at {short_source}, the commit you pushed.");
            let mut done = Outcome::completed("pushed", message.clone());
            done.outcome.commit = Some(oid.clone());
            settled(done, message, Some(oid))
        }
        Some(oid) => {
            let short_remote = ops::short(Some(&oid));
            let known = ops::rev(worktree, &format!("{oid}^{{commit}}"))
                .await
                .is_some();
            if known && integrate::is_ancestor(worktree, push.source_oid, &oid).await {
                let message = format!(
                    "{target} is at {short_remote}, which already contains {short_source}."
                );
                let mut done = Outcome::completed("already_on_remote", message.clone());
                done.outcome.commit = Some(oid.clone());
                return settled(done, message, Some(oid));
            }
            if !git_stopped {
                return undecided(
                    true,
                    Some(oid),
                    format!(
                        "{target} doesn't have {short_source} yet, but Git may still be pushing on your Mac. Check again in a moment."
                    ),
                );
            }
            undecided(
                true,
                Some(oid),
                format!(
                    "{target} is at {short_remote} and does not prove this push completed. A remote may still finish after Git stopped on your Mac. The push remains unconfirmed; check on your Mac before starting another push."
                ),
            )
        }
        None => {
            if !git_stopped {
                return undecided(
                    true,
                    None,
                    format!(
                        "{} has no branch {} yet, but Git may still be pushing on your Mac. Check again in a moment.",
                        push.remote, push.destination
                    ),
                );
            }
            undecided(true, None, format!(
                "{} has no branch named {} now. That does not prove the earlier request never ran or that the remote has stopped; the push remains unconfirmed.",
                push.remote, push.destination
            ))
        }
    }
}

/// Whether the remote should be given longer to finish a push Git sent just
/// before it stopped.
pub(crate) fn too_soon(settled_at: Option<std::time::SystemTime>) -> bool {
    settled_at
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|elapsed| elapsed < SETTLE_DELAY)
}
