//! Fetch, fast-forward, merge, rebase, Continue and Abort against throwaway
//! repositories and a disposable local bare remote. Nothing here touches
//! the network or a real repository: every "remote" is a bare repository in
//! the same temporary directory, or an address that cannot answer.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::receipts::{self, Before, Stored};
use super::tests::{entry, git, home, repository, Fixture};
use super::*;
use crate::cli::serve::contract::{
    GitControlledOperation, GitMutationReceipt, GitOperationKind, GitPreconditions,
    GitReceiptState, GitSyncPreview, GitSyncRelation,
};

pub(super) struct Remote {
    pub(super) fixture: Fixture,
    pub(super) bare: PathBuf,
    /// Another clone, standing in for a teammate who pushes.
    pub(super) other: PathBuf,
}

pub(super) fn configure(dir: &Path) {
    for (key, value) in [
        ("user.name", "Latch Test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        git(dir, &["config", key, value]);
    }
}

/// `main` tracking `origin/main` in a bare repository next to it.
pub(super) fn with_remote() -> Remote {
    let fixture = repository();
    configure(&fixture.root);
    let bare = fixture.base.join("remote.git");
    let other = fixture.base.join("other");
    git(
        &fixture.base,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    git(
        &fixture.root,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(&fixture.root, &["push", "-q", "-u", "origin", "main"]);
    git(
        &fixture.base,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    configure(&other);
    Remote {
        fixture,
        bare,
        other,
    }
}

pub(super) fn commit_file(dir: &Path, file: &str, content: &str, message: &str) {
    fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// A teammate's commit, pushed to the remote but not yet fetched here.
pub(super) fn push_upstream(remote: &Remote, file: &str, content: &str, message: &str) {
    commit_file(&remote.other, file, content, message);
    git(&remote.other, &["push", "-q", "origin", "main"]);
}

pub(super) fn rev(dir: &Path, revision: &str) -> String {
    git(dir, &["rev-parse", revision]).trim().to_owned()
}

pub(super) fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

pub(super) async fn read(fixture: &Fixture) -> GitStatus {
    status(&home(fixture), fixture.path(), &[]).await.unwrap()
}

async fn preview(fixture: &Fixture) -> GitSyncPreview {
    sync(fixture.path(), None).await.unwrap()
}

pub(super) async fn submit_now(
    fixture: &Fixture,
    request: u32,
    expected: &GitPreconditions,
    mutation: Mutation,
) -> Submitted {
    submit(
        &home(fixture),
        Some("phone-a".to_owned()),
        &id(request),
        fixture.path(),
        expected.clone(),
        mutation,
    )
    .await
    .expect("accepted")
}

/// Submits and, like the phone, keeps reading the receipt until it settles.
pub(super) async fn run(
    fixture: &Fixture,
    request: u32,
    expected: &GitPreconditions,
    mutation: Mutation,
) -> GitMutationReceipt {
    match submit_now(fixture, request, expected, mutation).await {
        Submitted::Settled(receipt) => receipt,
        Submitted::Pending(_) => settled(fixture, request).await,
    }
}

pub(super) async fn settled(fixture: &Fixture, request: u32) -> GitMutationReceipt {
    for _ in 0..300 {
        let receipt = receipt(&home(fixture), Some("phone-a"), &id(request))
            .await
            .unwrap()
            .expect("receipt");
        if receipt.state != GitReceiptState::Pending {
            return receipt;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("request {request} never settled");
}

pub(super) fn code(receipt: &GitMutationReceipt) -> &str {
    receipt
        .outcome
        .as_ref()
        .map_or("", |outcome| outcome.code.as_str())
}

pub(super) fn message(receipt: &GitMutationReceipt) -> &str {
    receipt
        .outcome
        .as_ref()
        .map_or("", |outcome| outcome.message.as_str())
}

pub(super) fn files(receipt: &GitMutationReceipt) -> Vec<String> {
    receipt
        .outcome
        .as_ref()
        .map(|outcome| outcome.files.clone())
        .unwrap_or_default()
}

fn fetch() -> Mutation {
    Mutation::Fetch {
        remote: "origin".to_owned(),
    }
}

fn update(kind: &str, preview: &GitSyncPreview) -> Mutation {
    let upstream = preview.upstream.as_ref().expect("an upstream");
    let (upstream, upstream_oid) = (upstream.name.clone(), upstream.oid.clone().unwrap());
    match kind {
        "fast_forward" => Mutation::FastForward {
            upstream,
            upstream_oid,
        },
        "merge" => Mutation::Merge {
            upstream,
            upstream_oid,
        },
        _ => Mutation::Rebase {
            upstream,
            upstream_oid,
        },
    }
}

pub(super) async fn fetch_now(fixture: &Fixture, request: u32) -> GitMutationReceipt {
    let expected = read(fixture).await.preconditions;
    run(fixture, request, &expected, fetch()).await
}

/// Local and upstream commits that both rewrite README.md.
async fn conflicting(remote: &Remote) {
    push_upstream(remote, "README.md", "theirs\n", "Upstream edit");
    commit_file(&remote.fixture.root, "README.md", "ours\n", "Local edit");
    assert_eq!(code(&fetch_now(&remote.fixture, 90).await), "fetched");
}

// MARK: - Fetch

#[tokio::test]
async fn opening_reviewing_and_refreshing_never_touch_the_remote() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    let tracking = rev(root, "origin/main");
    push_upstream(&remote, "NEW.md", "new\n", "Upstream");

    let status = read(&remote.fixture).await;
    let first = preview(&remote.fixture).await;
    let again = preview(&remote.fixture).await;
    let _ = branches(remote.fixture.path()).await.unwrap();

    assert_eq!(rev(root, "origin/main"), tracking, "nothing was fetched");
    assert!(!root.join(".git/FETCH_HEAD").exists());
    assert_eq!(status.last_fetched_at, None);
    assert_eq!(first.relation, GitSyncRelation::UpToDate);
    assert_eq!(first.behind, Some(0));
    assert!(first.incoming.is_empty());
    assert_eq!(first, again);
    assert!(first.fast_forward_blocked_reason.is_some());
}

#[tokio::test]
async fn an_explicit_fetch_updates_the_tracking_ref_and_the_fetch_time() {
    let remote = with_remote();
    push_upstream(&remote, "NEW.md", "new\n", "Teammate's change");

    let fetched = fetch_now(&remote.fixture, 1).await;
    assert_eq!(fetched.state, GitReceiptState::Completed, "{fetched:?}");
    assert_eq!(code(&fetched), "fetched");
    assert!(message(&fetched).contains("1 new commit"), "{fetched:?}");
    assert!(fetched.started_at.is_some());
    assert!(
        fetched.progress.is_none(),
        "settled receipts carry no progress"
    );

    let status = read(&remote.fixture).await;
    assert!(status.last_fetched_at.is_some());
    assert_eq!(status.upstream.as_ref().unwrap().behind, Some(1));
    // Fetching changed nothing but remote-tracking refs.
    assert!(status.entries.is_empty());

    let sync = preview(&remote.fixture).await;
    assert_eq!(sync.relation, GitSyncRelation::Behind);
    assert_eq!(sync.fast_forward_blocked_reason, None);
    assert_eq!(sync.incoming.len(), 1);
    assert_eq!(sync.incoming[0].subject, "Teammate's change");
    assert_eq!(sync.incoming[0].oid, rev(&remote.other, "HEAD"));
    let upstream = sync.upstream.unwrap();
    assert_eq!(upstream.name, "origin/main");
    assert_eq!(upstream.reference, "refs/remotes/origin/main");
    assert_eq!(upstream.remote.as_deref(), Some("origin"));
    assert_eq!(upstream.remote_url.as_deref(), remote.bare.to_str());
}

#[tokio::test]
async fn fetching_an_unknown_remote_is_refused_without_running_git() {
    let remote = with_remote();
    let expected = read(&remote.fixture).await.preconditions;
    let refused = run(
        &remote.fixture,
        1,
        &expected,
        Mutation::Fetch {
            remote: "elsewhere".to_owned(),
        },
    )
    .await;
    assert_eq!(code(&refused), "remote_not_found");
    assert_eq!(refused.started_at, None);
}

#[tokio::test]
async fn an_unreachable_remote_fails_promptly_without_prompting() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    // Port 9 refuses at once; a credential prompt would hang instead.
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            "http://user@127.0.0.1:9/repo.git",
        ],
    );
    let started = std::time::Instant::now();
    let refused = fetch_now(&remote.fixture, 1).await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert!(
        matches!(code(&refused), "network_failed" | "auth_failed"),
        "{refused:?}"
    );
    assert!(refused.outcome.unwrap().git_output.is_some());
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn remote_urls_never_carry_credentials_to_the_phone() {
    let remote = with_remote();
    git(
        &remote.fixture.root,
        &[
            "remote",
            "set-url",
            "origin",
            "https://someone:s3cret-token@example.invalid/team/repo.git",
        ],
    );
    let sync = preview(&remote.fixture).await;
    let text = serde_json::to_string(&sync).unwrap();
    assert!(!text.contains("s3cret-token"), "{text}");
    assert!(!text.contains("someone:"), "{text}");
    assert_eq!(
        sync.upstream.unwrap().remote_url.as_deref(),
        Some("https://example.invalid/team/repo.git")
    );
    assert_eq!(
        integrate::redact_url("git@github.com:team/repo.git"),
        "git@github.com:team/repo.git"
    );
}

/// A fetch that hangs answers early with a pending receipt and progress, is
/// stopped at its deadline (its whole process group, including the fake
/// `ssh`), and settles as uncertain, never as done or not done.
#[tokio::test]
async fn a_hanging_fetch_answers_early_reports_progress_and_times_out_uncertain() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://example.invalid/repo.git",
        ],
    );
    // Skip Git's `-G` probe of the ssh command, which discards its output.
    git(root, &["config", "ssh.variant", "ssh"]);
    git(
        root,
        &[
            "config",
            "core.sshCommand",
            "printf 'Receiving objects:  45%% (9/20)\\r' >&2; sleep 60;:",
        ],
    );
    let expected = read(&remote.fixture).await.preconditions;
    let started = std::time::Instant::now();
    let answered = submit_now(&remote.fixture, 1, &expected, fetch()).await;
    let Submitted::Pending(pending) = answered else {
        panic!("expected an early pending answer, got {answered:?}");
    };
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(pending.state, GitReceiptState::Pending);

    // The phone stops waiting and asks again later: still pending, with
    // Git's progress, and not claimed to have stopped.
    let mut progress = None;
    for _ in 0..50 {
        let again = receipt(&home(&remote.fixture), Some("phone-a"), &id(1))
            .await
            .unwrap()
            .unwrap();
        if again.state != GitReceiptState::Pending {
            break;
        }
        if let Some(found) = again.progress {
            progress = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let progress = progress.expect("progress while pending");
    assert!(
        progress.message.contains("Receiving objects"),
        "{progress:?}"
    );
    assert_eq!(progress.percent, Some(45));

    let done = settled(&remote.fixture, 1).await;
    assert_eq!(done.state, GitReceiptState::Uncertain, "{done:?}");
    assert_eq!(code(&done), "timed_out");
    assert!(message(&done).contains("Fetching again is safe"));
    assert!(done.progress.is_none());
}

// MARK: - Fast-forward

#[tokio::test]
async fn fast_forward_moves_the_branch_to_exactly_the_reviewed_commit() {
    let remote = with_remote();
    push_upstream(&remote, "NEW.md", "new\n", "One");
    push_upstream(&remote, "NEW.md", "newer\n", "Two");
    fetch_now(&remote.fixture, 1).await;
    let sync = preview(&remote.fixture).await;
    assert_eq!(sync.behind, Some(2));

    let done = run(
        &remote.fixture,
        2,
        &sync.preconditions,
        update("fast_forward", &sync),
    )
    .await;
    assert_eq!(code(&done), "fast_forwarded", "{done:?}");
    assert!(message(&done).contains("2 commits"), "{done:?}");
    let root = &remote.fixture.root;
    assert_eq!(rev(root, "HEAD"), rev(&remote.other, "HEAD"));
    assert_eq!(
        read(&remote.fixture).await.upstream.unwrap().behind,
        Some(0)
    );

    // Nothing incoming any more: Git says so, and nothing changes.
    let sync = preview(&remote.fixture).await;
    assert_eq!(sync.relation, GitSyncRelation::UpToDate);
    let again = run(
        &remote.fixture,
        3,
        &sync.preconditions,
        update("fast_forward", &sync),
    )
    .await;
    assert_eq!(code(&again), "up_to_date", "{again:?}");
}

#[tokio::test]
async fn fast_forward_on_divergence_is_refused_and_never_merges_or_rebases() {
    let remote = with_remote();
    push_upstream(&remote, "THEIRS.md", "theirs\n", "Upstream");
    commit_file(&remote.fixture.root, "OURS.md", "ours\n", "Local");
    fetch_now(&remote.fixture, 1).await;
    let sync = preview(&remote.fixture).await;
    assert_eq!(sync.relation, GitSyncRelation::Diverged);
    assert_eq!((sync.ahead, sync.behind), (Some(1), Some(1)));
    let reason = sync.fast_forward_blocked_reason.clone().unwrap();
    assert!(reason.contains("Merge or Rebase"), "{reason}");

    let root = &remote.fixture.root;
    let head = rev(root, "HEAD");
    let refused = run(
        &remote.fixture,
        2,
        &sync.preconditions,
        update("fast_forward", &sync),
    )
    .await;
    assert_eq!(refused.state, GitReceiptState::Rejected);
    assert_eq!(code(&refused), "not_fast_forward", "{refused:?}");
    assert!(refused.outcome.as_ref().unwrap().git_output.is_some());
    assert_eq!(rev(root, "HEAD"), head);
    assert!(!root.join(".git/MERGE_HEAD").exists());
    assert!(!root.join(".git/rebase-merge").exists());
}

// MARK: - Merge and rebase

#[tokio::test]
async fn merge_and_rebase_bring_in_diverged_commits() {
    for kind in ["merge", "rebase"] {
        let remote = with_remote();
        push_upstream(&remote, "THEIRS.md", "theirs\n", "Upstream");
        commit_file(&remote.fixture.root, "OURS.md", "ours\n", "Local");
        fetch_now(&remote.fixture, 1).await;
        let sync = preview(&remote.fixture).await;
        let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();
        let local = rev(&remote.fixture.root, "HEAD");

        let done = run(&remote.fixture, 2, &sync.preconditions, update(kind, &sync)).await;
        let root = &remote.fixture.root;
        if kind == "merge" {
            assert_eq!(code(&done), "merged", "{done:?}");
            assert_eq!(rev(root, "HEAD^1"), local);
            assert_eq!(rev(root, "HEAD^2"), upstream_oid);
            let subject = git(root, &["log", "-1", "--format=%s"]);
            assert_eq!(
                subject.trim(),
                "Merge remote-tracking branch 'origin/main' into main"
            );
        } else {
            assert_eq!(code(&done), "rebased", "{done:?}");
            assert_eq!(rev(root, "HEAD^"), upstream_oid);
            assert_eq!(git(root, &["log", "-1", "--format=%s"]).trim(), "Local");
        }
        let status = read(&remote.fixture).await;
        assert!(status.operation.is_none());
        assert_eq!(status.upstream.unwrap().behind, Some(0));
    }
}

#[tokio::test]
async fn updates_never_stash_and_git_refuses_to_overwrite_local_work() {
    for kind in ["merge", "rebase"] {
        let remote = with_remote();
        push_upstream(&remote, "README.md", "theirs\n", "Upstream");
        commit_file(&remote.fixture.root, "OURS.md", "ours\n", "Local");
        fetch_now(&remote.fixture, 1).await;
        // Turn on every autostash a Mac might have; Latch must not use it.
        git(
            &remote.fixture.root,
            &["config", "rebase.autoStash", "true"],
        );
        git(&remote.fixture.root, &["config", "merge.autoStash", "true"]);
        fs::write(remote.fixture.root.join("README.md"), "uncommitted\n").unwrap();
        let sync = preview(&remote.fixture).await;
        let head = rev(&remote.fixture.root, "HEAD");

        let refused = run(&remote.fixture, 2, &sync.preconditions, update(kind, &sync)).await;
        assert_eq!(
            refused.state,
            GitReceiptState::Rejected,
            "{kind}: {refused:?}"
        );
        assert!(
            matches!(code(&refused), "would_overwrite" | "local_changes"),
            "{kind}: {refused:?}"
        );
        let root = &remote.fixture.root;
        assert_eq!(rev(root, "HEAD"), head);
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "uncommitted\n"
        );
        assert_eq!(git(root, &["stash", "list"]), "", "{kind}: nothing stashed");
    }
}

#[tokio::test]
async fn a_merge_conflict_stops_and_continues_only_after_resolution() {
    let remote = with_remote();
    conflicting(&remote).await;
    let sync = preview(&remote.fixture).await;
    let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();
    let local = rev(&remote.fixture.root, "HEAD");

    let stopped = run(
        &remote.fixture,
        1,
        &sync.preconditions,
        update("merge", &sync),
    )
    .await;
    assert_eq!(stopped.state, GitReceiptState::Completed, "{stopped:?}");
    assert_eq!(code(&stopped), "stopped");
    assert_eq!(files(&stopped), vec!["README.md".to_owned()]);
    assert!(message(&stopped).contains("Resolve them on your Mac"));

    let status = read(&remote.fixture).await;
    let operation = status.operation.clone().unwrap();
    assert_eq!(operation.kind, GitOperationKind::Merge);
    assert_eq!(operation.merge_head.as_deref(), Some(upstream_oid.as_str()));
    assert!(operation
        .summary
        .unwrap()
        .starts_with("Merge remote-tracking branch"));
    let conflicted = entry(&status, "README.md", GitChangeArea::Conflicted).clone();

    // Another update while it is in progress is refused.
    let busy = run(
        &remote.fixture,
        2,
        &status.preconditions,
        update("merge", &sync),
    )
    .await;
    assert!(
        matches!(code(&busy), "refresh_required" | "operation_in_progress"),
        "{busy:?}"
    );
    // Continue is refused while the file is still conflicted.
    let control = |operation| Mutation::ContinueOperation { operation };
    let early = run(
        &remote.fixture,
        3,
        &status.preconditions,
        control(GitControlledOperation::Merge),
    )
    .await;
    assert_eq!(code(&early), "unresolved_conflicts", "{early:?}");
    assert_eq!(files(&early), vec!["README.md".to_owned()]);
    // Staging it with the markers still in asks first.
    let file = FileTarget {
        path_key: conflicted.path_key.clone(),
        area: GitChangeArea::Conflicted,
        fingerprint: conflicted.fingerprint.clone(),
    };
    let stage = |confirm| Mutation::Stage {
        selection: Selection::Files(vec![file.clone()]),
        confirm_conflict_markers: confirm,
    };
    let asked = run(&remote.fixture, 4, &status.preconditions, stage(false)).await;
    assert_eq!(asked.state, GitReceiptState::Rejected);
    assert_eq!(code(&asked), "conflict_markers", "{asked:?}");
    assert_eq!(files(&asked), vec!["README.md".to_owned()]);
    assert_eq!(asked.started_at, None, "Git never ran");

    // The user resolves it on the Mac, then stages it from the phone.
    fs::write(remote.fixture.root.join("README.md"), "ours and theirs\n").unwrap();
    let status = read(&remote.fixture).await;
    let conflicted = entry(&status, "README.md", GitChangeArea::Conflicted);
    let resolved = run(
        &remote.fixture,
        5,
        &status.preconditions,
        Mutation::Stage {
            selection: Selection::Files(vec![FileTarget {
                path_key: conflicted.path_key.clone(),
                area: GitChangeArea::Conflicted,
                fingerprint: conflicted.fingerprint.clone(),
            }]),
            confirm_conflict_markers: false,
        },
    )
    .await;
    assert_eq!(code(&resolved), "staged", "{resolved:?}");

    let status = read(&remote.fixture).await;
    assert!(status.operation.is_some(), "still merging until Continue");
    let done = run(
        &remote.fixture,
        6,
        &status.preconditions,
        control(GitControlledOperation::Merge),
    )
    .await;
    assert_eq!(code(&done), "continued", "{done:?}");
    let root = &remote.fixture.root;
    assert_eq!(rev(root, "HEAD^1"), local);
    assert_eq!(rev(root, "HEAD^2"), upstream_oid);
    assert_eq!(
        fs::read_to_string(root.join("README.md")).unwrap(),
        "ours and theirs\n"
    );
    assert!(read(&remote.fixture).await.operation.is_none());
}

#[tokio::test]
async fn a_confirmed_stage_keeps_the_markers_the_user_chose_to_keep() {
    let remote = with_remote();
    conflicting(&remote).await;
    let sync = preview(&remote.fixture).await;
    run(
        &remote.fixture,
        1,
        &sync.preconditions,
        update("merge", &sync),
    )
    .await;
    let status = read(&remote.fixture).await;
    let conflicted = entry(&status, "README.md", GitChangeArea::Conflicted);
    let staged = run(
        &remote.fixture,
        2,
        &status.preconditions,
        Mutation::Stage {
            selection: Selection::Files(vec![FileTarget {
                path_key: conflicted.path_key.clone(),
                area: GitChangeArea::Conflicted,
                fingerprint: conflicted.fingerprint.clone(),
            }]),
            confirm_conflict_markers: true,
        },
    )
    .await;
    assert_eq!(code(&staged), "staged", "{staged:?}");
    let text = git(&remote.fixture.root, &["show", ":README.md"]);
    assert!(text.contains("<<<<<<<"), "{text}");
}

#[tokio::test]
async fn a_rebase_conflict_stops_and_abort_restores_the_branch() {
    let remote = with_remote();
    conflicting(&remote).await;
    let root = &remote.fixture.root;
    let local = rev(root, "HEAD");
    let sync = preview(&remote.fixture).await;
    let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();

    let stopped = run(
        &remote.fixture,
        1,
        &sync.preconditions,
        update("rebase", &sync),
    )
    .await;
    assert_eq!(code(&stopped), "stopped", "{stopped:?}");
    assert_eq!(files(&stopped), vec!["README.md".to_owned()]);
    assert!(
        message(&stopped).contains("at commit 1 of 1"),
        "{stopped:?}"
    );

    let status = read(&remote.fixture).await;
    let operation = status.operation.clone().unwrap();
    assert_eq!(operation.kind, GitOperationKind::Rebase);
    assert_eq!(operation.head_name.as_deref(), Some("main"));
    assert_eq!(operation.onto.as_deref(), Some(upstream_oid.as_str()));
    assert_eq!((operation.step, operation.total), (Some(1), Some(1)));

    // The wrong operation, or a stale view, is refused.
    let wrong = run(
        &remote.fixture,
        2,
        &status.preconditions,
        Mutation::AbortOperation {
            operation: GitControlledOperation::Merge,
        },
    )
    .await;
    assert_eq!(code(&wrong), "refresh_required", "{wrong:?}");
    fs::write(root.join("SCRATCH.md"), "x\n").unwrap();
    let stale = run(
        &remote.fixture,
        3,
        &status.preconditions,
        Mutation::AbortOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(code(&stale), "refresh_required", "{stale:?}");
    fs::remove_file(root.join("SCRATCH.md")).unwrap();

    let status = read(&remote.fixture).await;
    let aborted = run(
        &remote.fixture,
        4,
        &status.preconditions,
        Mutation::AbortOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(code(&aborted), "aborted", "{aborted:?}");
    assert_eq!(rev(root, "HEAD"), local);
    assert_eq!(
        fs::read_to_string(root.join("README.md")).unwrap(),
        "ours\n"
    );
    let status = read(&remote.fixture).await;
    assert!(status.operation.is_none());
    assert_eq!(status.head.branch.as_deref(), Some("main"));

    // Nothing left to continue or abort.
    let none = run(
        &remote.fixture,
        5,
        &status.preconditions,
        Mutation::ContinueOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(code(&none), "no_operation", "{none:?}");
}

#[tokio::test]
async fn a_resolved_rebase_continues_to_the_end() {
    let remote = with_remote();
    conflicting(&remote).await;
    let root = &remote.fixture.root;
    let sync = preview(&remote.fixture).await;
    let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();
    run(
        &remote.fixture,
        1,
        &sync.preconditions,
        update("rebase", &sync),
    )
    .await;

    fs::write(root.join("README.md"), "merged by hand\n").unwrap();
    let status = read(&remote.fixture).await;
    let conflicted = entry(&status, "README.md", GitChangeArea::Conflicted);
    let staged = run(
        &remote.fixture,
        2,
        &status.preconditions,
        Mutation::Stage {
            selection: Selection::Files(vec![FileTarget {
                path_key: conflicted.path_key.clone(),
                area: GitChangeArea::Conflicted,
                fingerprint: conflicted.fingerprint.clone(),
            }]),
            confirm_conflict_markers: false,
        },
    )
    .await;
    assert_eq!(code(&staged), "staged");
    let status = read(&remote.fixture).await;
    let done = run(
        &remote.fixture,
        3,
        &status.preconditions,
        Mutation::ContinueOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(code(&done), "continued", "{done:?}");
    assert_eq!(rev(root, "HEAD^"), upstream_oid);
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "Local edit"
    );
    let status = read(&remote.fixture).await;
    assert!(status.operation.is_none());
    assert_eq!(status.head.branch.as_deref(), Some("main"));
}

/// Starts an interactive rebase the way someone on the Mac would, stopping
/// at `edit` on the first of two commits: a stop with nothing conflicted.
fn rebase_stopped_for_edit(root: &Path) {
    commit_file(root, "ONE.md", "1\n", "One");
    commit_file(root, "TWO.md", "2\n", "Two");
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["rebase", "-i", "HEAD~2"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_SEQUENCE_EDITOR", "sed -i.bak '1s/^pick/edit/'")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// A rebase can stop with nothing conflicted and still need the user: here
/// it stopped for an edit and a tracked file is left modified. Continue is
/// offered, Git refuses, and its own message is the answer; the rebase is
/// still there to finish on the Mac or abort.
#[tokio::test]
async fn a_rebase_stop_without_conflicts_returns_gits_refusal() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    rebase_stopped_for_edit(root);
    fs::write(root.join("README.md"), "unfinished edit\n").unwrap();

    let status = read(&remote.fixture).await;
    assert!(status.operation.is_some());
    assert!(status
        .entries
        .iter()
        .all(|entry| entry.area != GitChangeArea::Conflicted));
    let refused = run(
        &remote.fixture,
        2,
        &status.preconditions,
        Mutation::ContinueOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert_eq!(code(&refused), "git_refused", "{refused:?}");
    assert!(refused.outcome.unwrap().git_output.is_some());
    assert!(read(&remote.fixture).await.operation.is_some());
    assert_eq!(
        fs::read_to_string(root.join("README.md")).unwrap(),
        "unfinished edit\n",
        "nothing discarded"
    );
}

/// An interactive rebase started on the Mac stopped at `edit`: no conflict,
/// and Continue from the phone carries it on.
#[tokio::test]
async fn continue_carries_on_a_rebase_stopped_for_an_edit() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    rebase_stopped_for_edit(root);
    let status = read(&remote.fixture).await;
    let operation = status.operation.clone().unwrap();
    assert_eq!(operation.kind, GitOperationKind::Rebase);
    assert!(operation.stopped_at.is_some());
    assert!(status
        .entries
        .iter()
        .all(|entry| entry.area != GitChangeArea::Conflicted));

    let done = run(
        &remote.fixture,
        1,
        &status.preconditions,
        Mutation::ContinueOperation {
            operation: GitControlledOperation::Rebase,
        },
    )
    .await;
    assert_eq!(code(&done), "continued", "{done:?}");
    assert!(read(&remote.fixture).await.operation.is_none());
    assert_eq!(git(root, &["log", "-1", "--format=%s"]).trim(), "Two");
}

#[tokio::test]
async fn aborting_a_merge_returns_to_the_reviewed_commit() {
    let remote = with_remote();
    conflicting(&remote).await;
    let root = &remote.fixture.root;
    let local = rev(root, "HEAD");
    let sync = preview(&remote.fixture).await;
    run(
        &remote.fixture,
        1,
        &sync.preconditions,
        update("merge", &sync),
    )
    .await;
    let status = read(&remote.fixture).await;
    let aborted = run(
        &remote.fixture,
        2,
        &status.preconditions,
        Mutation::AbortOperation {
            operation: GitControlledOperation::Merge,
        },
    )
    .await;
    assert_eq!(code(&aborted), "aborted", "{aborted:?}");
    assert_eq!(rev(root, "HEAD"), local);
    assert!(!root.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        fs::read_to_string(root.join("README.md")).unwrap(),
        "ours\n"
    );
}

// MARK: - Stale state

#[tokio::test]
async fn an_upstream_that_moved_after_review_requires_a_refresh() {
    let remote = with_remote();
    push_upstream(&remote, "NEW.md", "one\n", "One");
    fetch_now(&remote.fixture, 1).await;
    let reviewed = preview(&remote.fixture).await;
    // A terminal on the Mac fetches again after the phone reviewed.
    push_upstream(&remote, "NEW.md", "two\n", "Two");
    git(&remote.fixture.root, &["fetch", "-q", "origin"]);
    let head = rev(&remote.fixture.root, "HEAD");

    for (request, kind) in [(2, "fast_forward"), (3, "merge"), (4, "rebase")] {
        let refused = run(
            &remote.fixture,
            request,
            &reviewed.preconditions,
            update(kind, &reviewed),
        )
        .await;
        assert_eq!(code(&refused), "refresh_required", "{kind}: {refused:?}");
        assert_eq!(refused.started_at, None);
    }
    assert_eq!(rev(&remote.fixture.root, "HEAD"), head);
}

#[tokio::test]
async fn edits_made_on_the_mac_after_review_require_a_refresh() {
    let remote = with_remote();
    push_upstream(&remote, "NEW.md", "one\n", "One");
    fetch_now(&remote.fixture, 1).await;
    let reviewed = preview(&remote.fixture).await;
    // An agent edits a file in the same worktree.
    fs::write(remote.fixture.root.join("README.md"), "agent edit\n").unwrap();
    let refused = run(
        &remote.fixture,
        2,
        &reviewed.preconditions,
        update("fast_forward", &reviewed),
    )
    .await;
    assert_eq!(code(&refused), "refresh_required", "{refused:?}");
    // A commit made outside Latch moves HEAD: also refused.
    git(&remote.fixture.root, &["commit", "-qam", "Outside"]);
    let refused = run(
        &remote.fixture,
        3,
        &reviewed.preconditions,
        update("merge", &reviewed),
    )
    .await;
    assert_eq!(code(&refused), "refresh_required", "{refused:?}");
}

#[tokio::test]
async fn sync_pages_incoming_commits_and_refuses_a_cursor_once_upstream_moves() {
    let remote = with_remote();
    for n in 0..(sync::PAGE + 5) {
        commit_file(
            &remote.other,
            "LOG.md",
            &format!("{n}\n"),
            &format!("Commit {n}"),
        );
    }
    git(&remote.other, &["push", "-q", "origin", "main"]);
    fetch_now(&remote.fixture, 1).await;
    let first = preview(&remote.fixture).await;
    assert_eq!(first.behind, Some(sync::PAGE as u64 + 5));
    assert_eq!(first.incoming.len(), sync::PAGE);
    let cursor = first.next_cursor.clone().expect("another page");
    let second = sync(remote.fixture.path(), Some(&cursor)).await.unwrap();
    assert_eq!(second.incoming.len(), 5);
    assert_eq!(second.next_cursor, None);
    let mut seen = first
        .incoming
        .iter()
        .chain(&second.incoming)
        .map(|commit| commit.oid.clone())
        .collect::<Vec<_>>();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), sync::PAGE + 5, "each commit exactly once");
    assert!(serde_json::to_vec(&first).unwrap().len() < 64 * 1024);

    // The incoming range also depends on HEAD, even if upstream did not
    // move. Reusing a page after a Mac commit must require a new review.
    commit_file(&remote.fixture.root, "LOCAL.md", "local\n", "Local");
    assert_eq!(
        sync(remote.fixture.path(), Some(&cursor)).await,
        Err(GitError::StaleCursor)
    );

    push_upstream(&remote, "LOG.md", "more\n", "More");
    git(&remote.fixture.root, &["fetch", "-q", "origin"]);
    assert_eq!(
        sync(remote.fixture.path(), Some(&cursor)).await,
        Err(GitError::StaleCursor)
    );
}

#[tokio::test]
async fn commit_text_delimiters_do_not_hide_outgoing_or_incoming_commits() {
    let remote = with_remote();
    commit_file(
        &remote.other,
        "REMOTE.md",
        "remote\n",
        "Subject with \u{1f} separator",
    );
    git(&remote.other, &["push", "-q", "origin", "main"]);
    fetch_now(&remote.fixture, 501).await;
    let incoming = preview(&remote.fixture).await;
    assert_eq!(incoming.incoming.len(), 1);
    assert_eq!(incoming.incoming[0].oid, rev(&remote.other, "HEAD"));
    commit_file(
        &remote.fixture.root,
        "LOCAL.md",
        "local\n",
        "Local \u{1f} subject",
    );
    let outgoing = push_preview(remote.fixture.path(), None, None)
        .await
        .unwrap();
    assert_eq!(outgoing.outgoing.len(), 1);
    assert_eq!(outgoing.outgoing[0].oid, rev(&remote.fixture.root, "HEAD"));
}

#[tokio::test]
async fn sync_explains_branches_without_an_upstream() {
    let remote = with_remote();
    git(&remote.fixture.root, &["switch", "-q", "-c", "topic"]);
    let sync = preview(&remote.fixture).await;
    assert_eq!(sync.relation, GitSyncRelation::NoUpstream);
    assert!(sync.upstream.is_none());
    assert_eq!(sync.remotes.len(), 1);
    assert!(sync
        .fast_forward_blocked_reason
        .unwrap()
        .contains("no upstream"));
    let refused = run(
        &remote.fixture,
        1,
        &sync.preconditions,
        Mutation::Merge {
            upstream: "origin/main".to_owned(),
            upstream_oid: rev(&remote.fixture.root, "origin/main"),
        },
    )
    .await;
    assert_eq!(code(&refused), "no_upstream", "{refused:?}");
}

// MARK: - Receipts

#[tokio::test]
async fn a_repeated_update_returns_the_first_receipt_and_never_runs_twice() {
    let remote = with_remote();
    push_upstream(&remote, "THEIRS.md", "theirs\n", "Upstream");
    commit_file(&remote.fixture.root, "OURS.md", "ours\n", "Local");
    fetch_now(&remote.fixture, 1).await;
    let sync = preview(&remote.fixture).await;
    let first = run(
        &remote.fixture,
        2,
        &sync.preconditions,
        update("merge", &sync),
    )
    .await;
    assert_eq!(code(&first), "merged");
    let merged = rev(&remote.fixture.root, "HEAD");

    // The answer was lost; the phone repeats the same request.
    let again = run(
        &remote.fixture,
        2,
        &sync.preconditions,
        update("merge", &sync),
    )
    .await;
    assert_eq!(again, first);
    assert_eq!(rev(&remote.fixture.root, "HEAD"), merged, "merged once");

    // The same id for different arguments is refused outright.
    let conflict = submit(
        &home(&remote.fixture),
        Some("phone-a".to_owned()),
        &id(2),
        remote.fixture.path(),
        sync.preconditions.clone(),
        update("rebase", &sync),
    )
    .await;
    assert_eq!(conflict.err(), Some(MutationError::RequestIdConflict));
    // And another device cannot see it.
    assert_eq!(
        receipt(&home(&remote.fixture), Some("phone-b"), &id(2))
            .await
            .unwrap(),
        None
    );
}

/// Writes a receipt the way a gateway that stopped mid-request leaves it.
pub(super) fn abandoned(
    fixture: &Fixture,
    request: u32,
    mutation: Mutation,
    before: Option<Before>,
) {
    let mut stored = Stored::new(
        Some("phone-a".to_owned()),
        "digest".to_owned(),
        mutation.clone(),
        GitMutationReceipt {
            request_id: id(request),
            operation: mutation.kind(),
            root: fixture.path().to_owned(),
            state: GitReceiptState::Pending,
            accepted_at: "2026-10-13T00:00:00Z".to_owned(),
            finished_at: None,
            outcome: None,
            started_at: None,
            progress: None,
        },
    );
    stored.before = before;
    assert!(matches!(
        receipts::claim(&home(fixture), &stored).unwrap(),
        receipts::Claim::Claimed
    ));
    receipts::clear_pid(&home(fixture), Some("phone-a"), &id(request));
}

#[tokio::test]
async fn updates_left_pending_by_a_restart_are_reconciled_from_the_repository() {
    let remote = with_remote();
    push_upstream(&remote, "THEIRS.md", "theirs\n", "Upstream");
    fetch_now(&remote.fixture, 1).await;
    let root = remote.fixture.root.clone();
    let sync = preview(&remote.fixture).await;
    let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();
    let head = rev(&root, "HEAD");
    let before = Before {
        head: Some(head.clone()),
        branch: Some("main".to_owned()),
        source: Some(upstream_oid.clone()),
        ..Before::default()
    };
    let lookup = |request: u32| {
        let home = home(&remote.fixture);
        async move {
            receipt(&home, Some("phone-a"), &id(request))
                .await
                .unwrap()
                .unwrap()
        }
    };

    // Accepted but Git never started: nothing happened.
    abandoned(&remote.fixture, 10, update("fast_forward", &sync), None);
    let settled = lookup(10).await;
    assert_eq!(settled.state, GitReceiptState::Rejected);
    assert_eq!(code(&settled), "not_applied");

    // Git started, but the branch never moved.
    abandoned(
        &remote.fixture,
        11,
        update("fast_forward", &sync),
        Some(before.clone()),
    );
    let settled = lookup(11).await;
    assert_eq!(code(&settled), "not_applied", "{settled:?}");
    assert!(settled.outcome.unwrap().reconciled);

    // Git finished before the gateway stopped.
    git(&root, &["merge", "-q", "--ff-only", &upstream_oid]);
    abandoned(
        &remote.fixture,
        12,
        update("fast_forward", &sync),
        Some(before.clone()),
    );
    let settled = lookup(12).await;
    assert_eq!(settled.state, GitReceiptState::Completed, "{settled:?}");
    assert_eq!(code(&settled), "fast_forwarded");

    // The branch went somewhere else: nobody can say.
    commit_file(&root, "OTHER.md", "x\n", "Someone else");
    abandoned(
        &remote.fixture,
        13,
        update("fast_forward", &sync),
        Some(before.clone()),
    );
    let settled = lookup(13).await;
    assert_eq!(settled.state, GitReceiptState::Uncertain, "{settled:?}");
}

#[tokio::test]
async fn a_merge_left_in_progress_by_a_restart_stays_uncertain() {
    let remote = with_remote();
    conflicting(&remote).await;
    let root = remote.fixture.root.clone();
    let sync = preview(&remote.fixture).await;
    let upstream_oid = sync.upstream.as_ref().unwrap().oid.clone().unwrap();
    let before = Before {
        head: Some(rev(&root, "HEAD")),
        branch: Some("main".to_owned()),
        source: Some(upstream_oid.clone()),
        ..Before::default()
    };
    git(&root, &["merge", "--no-edit", &upstream_oid]);
    assert!(root.join(".git/MERGE_HEAD").exists());
    abandoned(&remote.fixture, 20, update("merge", &sync), Some(before));
    let settled = receipt(&home(&remote.fixture), Some("phone-a"), &id(20))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Uncertain, "{settled:?}");
}

#[tokio::test]
async fn a_fetch_left_pending_is_settled_by_the_fetch_time() {
    let remote = with_remote();
    let before = Before {
        head: Some(rev(&remote.fixture.root, "HEAD")),
        branch: Some("main".to_owned()),
        ..Before::default()
    };
    abandoned(&remote.fixture, 30, fetch(), Some(before.clone()));
    let unsure = receipt(&home(&remote.fixture), Some("phone-a"), &id(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unsure.state, GitReceiptState::Uncertain, "{unsure:?}");
    assert!(message(&unsure).contains("Fetching again is safe"));

    // A fetch that started and was cut off leaves FETCH_HEAD emptied:
    // still uncertain.
    fs::write(remote.fixture.root.join(".git/FETCH_HEAD"), "").unwrap();
    abandoned(&remote.fixture, 31, fetch(), Some(before.clone()));
    let unsure = receipt(&home(&remote.fixture), Some("phone-a"), &id(31))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unsure.state, GitReceiptState::Uncertain, "{unsure:?}");

    git(&remote.fixture.root, &["fetch", "-q", "origin"]);
    abandoned(&remote.fixture, 32, fetch(), Some(before));
    let done = receipt(&home(&remote.fixture), Some("phone-a"), &id(32))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, GitReceiptState::Completed, "{done:?}");
    assert_eq!(code(&done), "fetched");
}

#[tokio::test]
async fn a_receipt_from_before_slice_three_still_reads() {
    let remote = with_remote();
    let stored = serde_json::json!({
        "formatVersion": 1,
        "device": "phone-a",
        "digest": "d",
        "mutation": {"kind": "stage", "selection": {"all": ["unstaged"]}},
        "before": {"head": null, "branch": "main", "targets": []},
        "receipt": {
            "requestId": id(40),
            "operation": "stage",
            "root": remote.fixture.path(),
            "state": "completed",
            "acceptedAt": "2026-10-13T00:00:00Z",
            "finishedAt": "2026-10-13T00:00:01Z",
            "outcome": null
        }
    });
    let dir = home(&remote.fixture)
        .root()
        .join("git/receipts/device-phone-a");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{}.json", id(40))),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let read = receipt(&home(&remote.fixture), Some("phone-a"), &id(40))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.state, GitReceiptState::Completed);
}

#[test]
fn progress_percentages_are_read_from_gits_lines() {
    assert_eq!(command::percent("Receiving objects:  45% (9/20)"), Some(45));
    assert_eq!(
        command::percent("Resolving deltas: 100% (3/3), done."),
        Some(100)
    );
    assert_eq!(command::percent("remote: Counting objects: 7, done."), None);
    assert_eq!(command::percent("Rebasing (2/5)"), None);
}

#[test]
fn remote_names_that_could_be_options_are_refused() {
    for name in ["origin", "upstream", "team/fork", "my-remote"] {
        assert!(is_plausible_remote_name(name), "{name}");
    }
    for name in ["", "-u", "--upload-pack=x", "a b", "a\nb", "x\u{0}"] {
        assert!(!is_plausible_remote_name(name), "{name:?}");
    }
}
