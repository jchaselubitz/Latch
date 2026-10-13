//! Push review, push and the check of an uncertain push, against throwaway
//! repositories and disposable local bare remotes. Nothing here uses a real
//! repository, remote or credential: every remote is a bare repository in
//! the same temporary directory, a fake `ssh`, or an address that refuses.

use std::fs;
use std::path::Path;
use std::time::Duration;

use super::integrate_tests::{
    abandoned, code, commit_file, configure, fetch_now, id, message, push_upstream, read, rev, run,
    settled, submit_now, with_remote, Remote,
};
use super::receipts::{self, Before};
use super::tests::{git, home, repository, Fixture};
use super::*;
use crate::cli::serve::contract::{
    GitMutationReceipt, GitPushPreview, GitPushSelectedBy, GitReceiptState,
};

async fn review(fixture: &Fixture) -> GitPushPreview {
    push_preview(fixture.path(), None, None).await.unwrap()
}

async fn review_to(fixture: &Fixture, remote: &str, destination: &str) -> GitPushPreview {
    push_preview(fixture.path(), Some((remote, destination)), None)
        .await
        .unwrap()
}

/// The push exactly as the phone builds it from a reviewed preview.
fn push_of(preview: &GitPushPreview) -> Mutation {
    let source = preview.source.as_ref().expect("a source");
    let target = preview.target.as_ref().expect("a target");
    Mutation::Push {
        remote: target.remote.clone(),
        remote_url: target.remote_url.clone(),
        destination: target.destination.clone(),
        source_branch: source.branch.clone(),
        source_oid: source.oid.clone(),
        destination_oid: target.destination_oid.clone(),
    }
}

async fn push_now(fixture: &Fixture, request: u32, preview: &GitPushPreview) -> GitMutationReceipt {
    run(fixture, request, &preview.preconditions, push_of(preview)).await
}

#[tokio::test]
async fn reconciliation_checks_the_push_url_not_the_fetch_url() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    let push_bare = remote.fixture.base.join("push.git");
    git(
        &remote.fixture.base,
        &[
            "clone",
            "-q",
            "--bare",
            remote.bare.to_str().unwrap(),
            push_bare.to_str().unwrap(),
        ],
    );
    git(
        root,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            push_bare.to_str().unwrap(),
        ],
    );
    commit_file(root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        101,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    // Only the fetch repository has the commit. It proves nothing about
    // the destination that the person reviewed and pushed to.
    git(
        root,
        &[
            "push",
            "-q",
            &format!("file://{}", remote.bare.display()),
            "HEAD:refs/heads/main",
        ],
    );
    let found = check(&remote.fixture, 101).await;
    assert!(found.remote_checked);
    assert_ne!(
        found.remote_oid,
        preview.source.as_ref().map(|source| source.oid.clone())
    );
    assert_ne!(found.receipt.state, GitReceiptState::Completed);
}

#[tokio::test]
async fn multiple_push_urls_cannot_send_to_an_unreviewed_destination() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    let second = remote.fixture.base.join("second.git");
    git(
        &remote.fixture.base,
        &[
            "clone",
            "-q",
            "--bare",
            remote.bare.to_str().unwrap(),
            second.to_str().unwrap(),
        ],
    );
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            remote.bare.to_str().unwrap(),
        ],
    );
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            second.to_str().unwrap(),
        ],
    );
    let original = remote_ref(&remote, "main");
    commit_file(root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    assert!(preview
        .blocked_reason
        .as_deref()
        .unwrap()
        .contains("one push URL"));
    let receipt = push_now(&remote.fixture, 102, &preview).await;
    assert_eq!(code(&receipt), "refresh_required");
    assert_eq!(remote_ref(&remote, "main"), original);
    assert_eq!(rev(&second, "main"), original.unwrap());
}

#[tokio::test]
async fn changed_push_url_is_not_used_to_reconcile_an_old_push() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        103,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    git(
        &remote.fixture.root,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "/nonexistent/replacement.git",
        ],
    );
    let found = check(&remote.fixture, 103).await;
    assert!(!found.remote_checked);
    assert_eq!(found.receipt.state, GitReceiptState::Uncertain);
    assert!(found.message.contains("address changed"));
}

#[tokio::test]
async fn a_lost_remote_report_is_uncertain_even_when_git_exits_nonzero() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    let worktree = worktree::resolve(remote.fixture.path()).await.unwrap();
    let output = command::GitOutput {
        stdout: Vec::new(),
        stderr: b"fatal: the remote end hung up unexpectedly\n".to_vec(),
        code: Some(128),
        truncated: false,
    };
    let outcome = push::outcome(
        &push_of(&preview),
        &worktree,
        &before_push(&preview),
        &output,
    )
    .await;
    assert_eq!(outcome.state, GitReceiptState::Uncertain);
    assert_eq!(outcome.outcome.code, "push_unconfirmed");
}

fn remote_ref(remote: &Remote, branch: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .current_dir(&remote.bare)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn hook(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `pre-push` hook that counts how often Git really pushed.
fn count_pushes(remote: &Remote) -> std::path::PathBuf {
    let counter = remote.fixture.base.join("pushes");
    hook(
        &remote.fixture.root.join(".git/hooks"),
        "pre-push",
        &format!("echo push >> '{}'", counter.display()),
    );
    counter
}

fn pushes(counter: &Path) -> usize {
    fs::read_to_string(counter).map_or(0, |text| text.lines().count())
}

async fn check(fixture: &Fixture, request: u32) -> PushCheck {
    check_push(&home(fixture), Some("phone-a"), &id(request))
        .await
        .expect("checked")
}

type PushCheck = super::mutation::PushCheck;

// MARK: - Preview

#[tokio::test]
async fn the_preview_reviews_the_upstream_without_contacting_the_remote() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    git(root, &["remote", "set-head", "origin", "main"]);
    commit_file(root, "A.md", "a\n", "First outgoing");
    commit_file(root, "B.md", "b\n", "Second outgoing");
    let fetched_before = read(&remote.fixture).await.last_fetched_at;
    // A teammate moves the remote; the preview must not notice.
    push_upstream(&remote, "THEIRS.md", "t\n", "Theirs");
    let tracking_before = rev(root, "origin/main");

    let preview = review(&remote.fixture).await;
    let source = preview.source.clone().unwrap();
    let target = preview.target.clone().unwrap();
    assert_eq!(source.branch, "main");
    assert_eq!(source.oid, rev(root, "HEAD"));
    assert_eq!(target.remote, "origin");
    assert_eq!(target.destination, "main");
    assert_eq!(target.destination_ref, "refs/heads/main");
    assert_eq!(
        target.tracking_ref.as_deref(),
        Some("refs/remotes/origin/main")
    );
    assert_eq!(
        target.destination_oid.as_deref(),
        Some(tracking_before.as_str())
    );
    assert_eq!(target.selected_by, GitPushSelectedBy::Upstream);
    assert!(!target.new_branch);
    assert!(target.is_default_branch, "{target:?}");
    assert_eq!(target.default_branch.as_deref(), Some("main"));
    assert_eq!(
        target.remote_url.as_deref(),
        Some(remote.bare.to_str().unwrap())
    );
    assert!(!preview.selection_required);
    assert_eq!(preview.ahead, Some(2));
    assert_eq!(preview.behind, Some(0));
    assert!(preview.blocked_reason.is_none());
    assert!(preview.rejection_likely_reason.is_none());
    let subjects = preview
        .outgoing
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect::<Vec<_>>();
    assert_eq!(subjects, ["Second outgoing", "First outgoing"]);
    assert!(preview.next_cursor.is_none());

    // Nothing was fetched or moved by reviewing.
    assert_eq!(rev(root, "origin/main"), tracking_before);
    assert_eq!(read(&remote.fixture).await.last_fetched_at, fetched_before);
    assert!(serde_json::to_vec(&preview).unwrap().len() < 512 * 1024);
}

#[tokio::test]
async fn a_branch_without_an_upstream_needs_an_explicit_destination() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    git(root, &["switch", "-q", "-c", "topic"]);
    commit_file(root, "T.md", "t\n", "Topic work");

    let preview = review(&remote.fixture).await;
    assert!(preview.selection_required, "{preview:?}");
    assert!(preview.target.is_none());
    assert!(preview.outgoing.is_empty());
    assert!(preview.selection_reason.unwrap().contains("no upstream"));
    let suggestion = preview.suggestion.unwrap();
    assert_eq!(
        (suggestion.remote.as_str(), suggestion.destination.as_str()),
        ("origin", "topic")
    );

    // Only once the phone names both is there something to push.
    let chosen = review_to(&remote.fixture, "origin", "topic").await;
    let target = chosen.target.clone().unwrap();
    assert_eq!(target.selected_by, GitPushSelectedBy::Request);
    assert!(target.new_branch);
    assert!(target.destination_oid.is_none());
    assert!(!target.is_default_branch);
    assert_eq!(chosen.ahead, Some(1));
    assert_eq!(chosen.outgoing.len(), 1);
    assert_eq!(chosen.outgoing[0].subject, "Topic work");

    let unknown = review_to(&remote.fixture, "upstream", "topic").await;
    assert!(unknown.target.is_none());
    assert!(unknown
        .selection_reason
        .unwrap()
        .contains("no remote named upstream"));
}

#[tokio::test]
async fn the_preview_explains_what_cannot_be_pushed() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    // Up to date with the remote as last fetched: nothing to send.
    let current = review(&remote.fixture).await;
    assert_eq!(current.ahead, Some(0));
    assert!(current
        .blocked_reason
        .unwrap()
        .contains("already has every commit"));

    // The remote has a commit this branch lacks: allowed, but flagged.
    push_upstream(&remote, "THEIRS.md", "t\n", "Theirs");
    fetch_now(&remote.fixture, 1).await;
    commit_file(root, "MINE.md", "m\n", "Mine");
    let diverged = review(&remote.fixture).await;
    assert_eq!((diverged.ahead, diverged.behind), (Some(1), Some(1)));
    assert!(diverged.blocked_reason.is_none());
    assert!(diverged
        .rejection_likely_reason
        .unwrap()
        .contains("will probably refuse"));

    git(root, &["switch", "-q", "--detach", "HEAD"]);
    let detached = review(&remote.fixture).await;
    assert!(detached.source.is_none());
    assert!(detached.blocked_reason.unwrap().contains("detached"));

    let lonely = repository();
    let alone = review(&lonely).await;
    assert!(alone.blocked_reason.unwrap().contains("no remotes"));
    assert!(!alone.selection_required);
}

#[tokio::test]
async fn outgoing_commits_page_and_a_cursor_is_refused_once_the_source_moves() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    for n in 0..55 {
        commit_file(root, "N.md", &format!("{n}\n"), &format!("Commit {n}"));
    }
    let first = review(&remote.fixture).await;
    assert_eq!(first.ahead, Some(55));
    assert_eq!(first.outgoing.len(), 50);
    let cursor = first.next_cursor.clone().unwrap();
    let second = push_preview(remote.fixture.path(), None, Some(&cursor))
        .await
        .unwrap();
    assert_eq!(second.outgoing.len(), 5);
    assert!(second.next_cursor.is_none());
    let mut seen = first
        .outgoing
        .iter()
        .chain(&second.outgoing)
        .map(|commit| commit.oid.clone())
        .collect::<Vec<_>>();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 55, "each outgoing commit exactly once");

    commit_file(root, "N.md", "more\n", "One more");
    assert_eq!(
        push_preview(remote.fixture.path(), None, Some(&cursor))
            .await
            .unwrap_err(),
        GitError::StaleCursor
    );
}

// MARK: - Push

#[tokio::test]
async fn a_reviewed_push_sends_exactly_the_reviewed_commit() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    let counter = count_pushes(&remote);
    let preview = review(&remote.fixture).await;
    let source = preview.source.clone().unwrap().oid;

    let done = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(done.state, GitReceiptState::Completed, "{done:?}");
    assert_eq!(code(&done), "pushed");
    assert!(
        message(&done).contains("Pushed 1 commit to origin/main"),
        "{}",
        message(&done)
    );
    assert_eq!(
        done.outcome.as_ref().unwrap().commit.as_deref(),
        Some(source.as_str())
    );
    assert_eq!(
        remote_ref(&remote, "main").as_deref(),
        Some(source.as_str())
    );
    // Git recorded the remote's answer in the tracking ref.
    assert_eq!(rev(root, "origin/main"), source);
    assert_eq!(pushes(&counter), 1);
}

#[tokio::test]
async fn a_new_branch_is_created_and_tags_never_ride_along() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    git(root, &["config", "push.followTags", "true"]);
    git(root, &["switch", "-q", "-c", "topic"]);
    commit_file(root, "T.md", "t\n", "Topic");
    git(root, &["tag", "-a", "-m", "release", "v1"]);

    let preview = review_to(&remote.fixture, "origin", "topic").await;
    let done = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(done.state, GitReceiptState::Completed, "{done:?}");
    assert_eq!(code(&done), "pushed");
    assert!(message(&done).contains("Created topic on origin"));
    assert_eq!(remote_ref(&remote, "topic"), Some(rev(root, "HEAD")));
    let tags = git(&remote.bare, &["tag", "--list"]);
    assert!(tags.trim().is_empty(), "tags were pushed: {tags}");
    // No upstream was configured behind the user's back.
    let config = fs::read_to_string(root.join(".git/config")).unwrap();
    assert!(!config.contains("[branch \"topic\"]"), "{config}");
}

#[tokio::test]
async fn a_push_to_a_non_default_branch_uses_its_upstream() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    git(root, &["remote", "set-head", "origin", "main"]);
    git(root, &["switch", "-q", "-c", "feature"]);
    commit_file(root, "F.md", "f\n", "Feature");
    git(root, &["push", "-q", "-u", "origin", "feature"]);
    commit_file(root, "F.md", "g\n", "More feature");

    let preview = review(&remote.fixture).await;
    let target = preview.target.clone().unwrap();
    assert_eq!(target.destination, "feature");
    assert!(!target.is_default_branch);
    assert_eq!(target.default_branch.as_deref(), Some("main"));
    let done = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(code(&done), "pushed", "{done:?}");
    assert_eq!(remote_ref(&remote, "feature"), Some(rev(root, "HEAD")));
    assert_ne!(remote_ref(&remote, "main"), Some(rev(root, "HEAD")));
}

#[tokio::test]
async fn a_changed_local_preview_requires_review_again_and_pushes_nothing() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    let original = remote_ref(&remote, "main");

    // The branch moved after review.
    let preview = review(&remote.fixture).await;
    commit_file(root, "B.md", "b\n", "Later");
    let moved = run(
        &remote.fixture,
        1,
        &read(&remote.fixture).await.preconditions,
        push_of(&preview),
    )
    .await;
    assert_eq!(moved.state, GitReceiptState::Rejected);
    assert_eq!(code(&moved), "refresh_required", "{moved:?}");

    // The destination was fetched (moved) after review.
    let preview = review(&remote.fixture).await;
    push_upstream(&remote, "THEIRS.md", "t\n", "Theirs");
    fetch_now(&remote.fixture, 2).await;
    let fetched = push_now(&remote.fixture, 3, &preview).await;
    assert_eq!(code(&fetched), "refresh_required", "{fetched:?}");
    assert!(message(&fetched).contains("origin/main changed"));

    // The push address changed after review.
    let preview = review(&remote.fixture).await;
    let elsewhere = remote.fixture.base.join("elsewhere.git");
    git(
        &remote.fixture.base,
        &["init", "-q", "--bare", elsewhere.to_str().unwrap()],
    );
    git(
        root,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            elsewhere.to_str().unwrap(),
        ],
    );
    let readdressed = push_now(&remote.fixture, 4, &preview).await;
    assert_eq!(code(&readdressed), "refresh_required", "{readdressed:?}");

    assert_ne!(remote_ref(&remote, "main"), Some(rev(root, "HEAD")));
    assert!(git(&elsewhere, &["for-each-ref"]).trim().is_empty());
    let _ = original;
}

#[tokio::test]
async fn a_remote_that_moved_after_review_refuses_and_nothing_is_forced() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "MINE.md", "m\n", "Mine");
    let preview = review(&remote.fixture).await;
    // A teammate pushes after the review; this Mac never fetched it.
    push_upstream(&remote, "THEIRS.md", "t\n", "Theirs");
    let theirs = remote_ref(&remote, "main");

    let refused = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert_eq!(code(&refused), "non_fast_forward");
    assert!(message(&refused).contains("Fetch and choose how to update"));
    assert!(refused
        .outcome
        .unwrap()
        .git_output
        .unwrap()
        .contains("rejected"));
    assert_eq!(
        remote_ref(&remote, "main"),
        theirs,
        "the remote was not overwritten"
    );
}

#[tokio::test]
async fn the_remotes_own_refusal_is_returned() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    hook(
        &remote.bare.join("hooks"),
        "pre-receive",
        "echo 'main is protected' >&2; exit 1",
    );
    let before = remote_ref(&remote, "main");
    let preview = review(&remote.fixture).await;
    let refused = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert_eq!(code(&refused), "remote_rejected");
    assert!(message(&refused).contains("pre-receive hook declined"));
    assert!(refused
        .outcome
        .unwrap()
        .git_output
        .unwrap()
        .contains("main is protected"));
    assert_eq!(remote_ref(&remote, "main"), before);
}

#[tokio::test]
async fn a_pre_push_hook_failure_is_reported_and_nothing_is_sent() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    hook(
        &remote.fixture.root.join(".git/hooks"),
        "pre-push",
        "echo 'tests failed' >&2; exit 1",
    );
    let before = remote_ref(&remote, "main");
    let preview = review(&remote.fixture).await;
    let refused = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(refused.state, GitReceiptState::Uncertain, "{refused:?}");
    assert_eq!(code(&refused), "hook_failed");
    assert!(refused
        .outcome
        .unwrap()
        .git_output
        .unwrap()
        .contains("tests failed"));
    assert_eq!(remote_ref(&remote, "main"), before);
}

/// Credentials come only from the Mac. A transport that asks for them fails
/// at once with an actionable code instead of waiting for an answer.
#[tokio::test]
async fn missing_credentials_fail_promptly_without_prompting() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@example.invalid/repo.git",
        ],
    );
    git(root, &["config", "ssh.variant", "ssh"]);
    // A fake ssh that refuses the key, as a Mac without one would see.
    git(
        root,
        &[
            "config",
            "core.sshCommand",
            "echo 'git@example.invalid: Permission denied (publickey).' >&2; exit 255;:",
        ],
    );
    let preview = review(&remote.fixture).await;
    let started = std::time::Instant::now();
    let refused = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert_eq!(code(&refused), "auth_failed");
    assert!(message(&refused).contains("Set them up on the Mac"));
    assert!(started.elapsed() < Duration::from_secs(15));

    // A credential prompt on an HTTP remote cannot hang either.
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            "http://user@127.0.0.1:9/repo.git",
        ],
    );
    let preview = review(&remote.fixture).await;
    let started = std::time::Instant::now();
    let refused = push_now(&remote.fixture, 2, &preview).await;
    assert_eq!(refused.state, GitReceiptState::Rejected, "{refused:?}");
    assert!(
        matches!(code(&refused), "network_failed" | "auth_failed"),
        "{refused:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(15));
}

/// A push that hangs answers early with progress, is stopped at its
/// deadline with everything it started, and is uncertain, never "not
/// pushed". Stopping waiting is not cancelling.
#[tokio::test]
async fn a_hanging_push_times_out_uncertain_and_is_not_repeated() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://example.invalid/repo.git",
        ],
    );
    git(root, &["config", "ssh.variant", "ssh"]);
    git(
        root,
        &[
            "config",
            "core.sshCommand",
            "printf 'Writing objects:  50%% (1/2)\\r' >&2; sleep 60;:",
        ],
    );
    let preview = review(&remote.fixture).await;
    let answered = submit_now(
        &remote.fixture,
        1,
        &preview.preconditions,
        push_of(&preview),
    )
    .await;
    let Submitted::Pending(pending) = answered else {
        panic!("expected an early pending answer, got {answered:?}");
    };
    assert_eq!(
        pending.operation,
        crate::cli::serve::contract::GitMutationKind::Push
    );
    let done = settled(&remote.fixture, 1).await;
    assert_eq!(done.state, GitReceiptState::Uncertain, "{done:?}");
    assert_eq!(code(&done), "timed_out");
    assert!(message(&done).contains("Check the remote"));

    // Repeating the same request returns the same receipt; Git is not run
    // again.
    let again = run(
        &remote.fixture,
        1,
        &preview.preconditions,
        push_of(&preview),
    )
    .await;
    assert_eq!(again, done);
    // The process group was killed at the deadline.
    assert!(!receipts::git_may_be_running(
        &home(&remote.fixture),
        Some("phone-a"),
        &id(1)
    ));
}

#[tokio::test]
async fn a_repeated_push_returns_the_first_receipt_and_pushes_once() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let counter = count_pushes(&remote);
    let preview = review(&remote.fixture).await;
    let first = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(code(&first), "pushed");
    let again = push_now(&remote.fixture, 1, &preview).await;
    assert_eq!(again, first);
    assert_eq!(pushes(&counter), 1);

    // The same id for a different push is refused before anything runs.
    let mut other = push_of(&preview);
    if let Mutation::Push { destination, .. } = &mut other {
        *destination = "elsewhere".to_owned();
    }
    let conflict = submit(
        &home(&remote.fixture),
        Some("phone-a".to_owned()),
        &id(1),
        remote.fixture.path(),
        preview.preconditions.clone(),
        other,
    )
    .await
    .unwrap_err();
    assert_eq!(conflict, MutationError::RequestIdConflict);
    assert_eq!(pushes(&counter), 1);
    assert!(remote_ref(&remote, "elsewhere").is_none());
}

// MARK: - Lost responses and restarts

fn before_push(preview: &GitPushPreview) -> Before {
    Before {
        head: preview.preconditions.head.clone(),
        branch: preview.preconditions.branch.clone(),
        source: preview.source.as_ref().map(|source| source.oid.clone()),
        destination: preview
            .target
            .as_ref()
            .and_then(|target| target.destination_oid.clone()),
        ..Before::default()
    }
}

#[tokio::test]
async fn a_push_lost_before_git_started_is_not_applied() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(&remote.fixture, 1, push_of(&preview), None);
    let found = settled(&remote.fixture, 1).await;
    assert_eq!(found.state, GitReceiptState::Rejected, "{found:?}");
    assert_eq!(code(&found), "not_applied");
    // A check after that leaves it alone and contacts nothing.
    let checked = check(&remote.fixture, 1).await;
    assert!(!checked.remote_checked);
    assert_eq!(checked.receipt, found);
}

#[tokio::test]
async fn a_tracking_ref_alone_never_settles_an_interrupted_push() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    // The push went through and Git recorded it before the gateway stopped.
    git(root, &["push", "-q", "origin", "main"]);
    let found = settled(&remote.fixture, 1).await;
    assert_eq!(found.state, GitReceiptState::Uncertain, "{found:?}");
    assert_eq!(code(&found), "push_unconfirmed");
    assert!(found.outcome.unwrap().reconciled);
}

/// The remote took the push but the Mac never recorded it: the local state
/// cannot tell, so the receipt stays uncertain, and only asking the remote
/// settles it. Nothing is pushed a second time.
#[tokio::test]
async fn a_push_lost_after_the_remote_accepted_it_is_confirmed_by_asking_the_remote() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    let counter = count_pushes(&remote);
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    // Accepted by the remote without moving the tracking ref.
    git(
        root,
        &[
            "push",
            "-q",
            "--no-verify",
            // Not origin's spelling of the address, so Git does not move
            // origin's tracking ref.
            &format!("file://{}", remote.bare.display()),
            &format!("{}:refs/heads/main", rev(root, "HEAD")),
        ],
    );
    let found = settled(&remote.fixture, 1).await;
    assert_eq!(found.state, GitReceiptState::Uncertain, "{found:?}");
    assert_eq!(code(&found), "push_unconfirmed");

    let checked = check(&remote.fixture, 1).await;
    assert!(checked.remote_checked);
    assert_eq!(checked.remote_oid, Some(rev(root, "HEAD")));
    assert_eq!(checked.receipt.state, GitReceiptState::Completed);
    assert_eq!(code(&checked.receipt), "pushed");
    assert!(checked.receipt.outcome.as_ref().unwrap().reconciled);
    // The receipt now says so for good.
    assert_eq!(settled(&remote.fixture, 1).await, checked.receipt);
    assert_eq!(pushes(&counter), 0, "Latch never pushed again");
}

#[tokio::test]
async fn an_unchanged_remote_after_git_stopped_does_not_prove_the_request_never_ran() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    let found = settled(&remote.fixture, 1).await;
    assert_eq!(found.state, GitReceiptState::Uncertain, "{found:?}");

    let checked = check(&remote.fixture, 1).await;
    assert!(checked.remote_checked);
    assert_eq!(checked.receipt.state, GitReceiptState::Uncertain);
    assert_eq!(code(&checked.receipt), "push_unconfirmed");
    assert!(
        checked.message.contains("remains unconfirmed"),
        "{}",
        checked.message
    );
}

#[tokio::test]
async fn a_missing_process_record_does_not_prove_git_stopped() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        104,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    fs::remove_file(
        home(&remote.fixture)
            .root()
            .join("git/receipts/device-phone-a")
            .join(format!("{}.pid", id(104))),
    )
    .unwrap();
    let checked = check(&remote.fixture, 104).await;
    assert_eq!(checked.receipt.state, GitReceiptState::Uncertain);
    assert!(checked.message.contains("may still be pushing"));
}

/// While the Git a previous gateway started might still be running, an
/// unchanged remote proves nothing.
#[tokio::test]
async fn a_push_whose_git_may_still_run_stays_uncertain() {
    use std::os::unix::process::CommandExt;
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    let mut survivor = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    receipts::record_pid(
        &home(&remote.fixture),
        Some("phone-a"),
        &id(1),
        survivor.id(),
    );

    let checked = check(&remote.fixture, 1).await;
    assert_eq!(
        checked.receipt.state,
        GitReceiptState::Uncertain,
        "{checked:?}"
    );
    assert!(
        checked.message.contains("may still be pushing"),
        "{}",
        checked.message
    );
    survivor.kill().unwrap();
    survivor.wait().unwrap();

    let checked = check(&remote.fixture, 1).await;
    assert_eq!(code(&checked.receipt), "push_unconfirmed");
}

#[tokio::test]
async fn a_remote_without_the_source_stays_uncertain_even_after_fetching() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    push_upstream(&remote, "THEIRS.md", "t\n", "Theirs");

    let checked = check(&remote.fixture, 1).await;
    assert!(checked.remote_checked);
    assert_eq!(
        checked.receipt.state,
        GitReceiptState::Uncertain,
        "{checked:?}"
    );
    assert!(
        checked.message.contains("remains unconfirmed"),
        "{}",
        checked.message
    );

    fetch_now(&remote.fixture, 2).await;
    let checked = check(&remote.fixture, 1).await;
    assert_eq!(
        checked.receipt.state,
        GitReceiptState::Uncertain,
        "{checked:?}"
    );
    assert_eq!(code(&checked.receipt), "push_unconfirmed");
}

#[tokio::test]
async fn an_unreachable_remote_leaves_an_uncertain_push_unchanged() {
    let remote = with_remote();
    let root = &remote.fixture.root;
    commit_file(root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    let found = settled(&remote.fixture, 1).await;
    git(
        root,
        &["remote", "set-url", "origin", "http://127.0.0.1:9/repo.git"],
    );
    let checked = check(&remote.fixture, 1).await;
    assert!(!checked.remote_checked);
    assert_eq!(checked.receipt, found);
    assert!(
        checked.message.contains("still unconfirmed"),
        "{}",
        checked.message
    );
}

#[tokio::test]
async fn push_checks_are_scoped_to_the_device_and_to_pushes() {
    let remote = with_remote();
    commit_file(&remote.fixture.root, "A.md", "a\n", "Outgoing");
    let preview = review(&remote.fixture).await;
    abandoned(
        &remote.fixture,
        1,
        push_of(&preview),
        Some(before_push(&preview)),
    );
    let home = home(&remote.fixture);
    assert_eq!(
        check_push(&home, Some("phone-b"), &id(1))
            .await
            .unwrap_err(),
        PushCheckError::NotFound
    );
    assert_eq!(
        check_push(&home, None, &id(1)).await.unwrap_err(),
        PushCheckError::NotFound
    );
    assert_eq!(
        check_push(&home, Some("phone-a"), &id(9))
            .await
            .unwrap_err(),
        PushCheckError::NotFound
    );
    fetch_now(&remote.fixture, 2).await;
    assert_eq!(
        check_push(&home, Some("phone-a"), &id(2))
            .await
            .unwrap_err(),
        PushCheckError::NotAPush
    );
}

#[test]
fn porcelain_ref_lines_are_read_for_the_reviewed_destination_only() {
    let stdout = b"To ../remote.git\n=\tabc:refs/heads/other\t[up to date]\n!\tdef:refs/heads/main\t[rejected] (fetch first)\nDone\n";
    assert_eq!(
        super::push::ref_status(stdout, "main"),
        Some(('!', "[rejected] (fetch first)".to_owned()))
    );
    assert_eq!(
        super::push::ref_status(stdout, "other"),
        Some(('=', "[up to date]".to_owned()))
    );
    assert_eq!(super::push::ref_status(stdout, "missing"), None);
    let cursor = super::push::cursor("aaa", Some("bbb"), 50);
    assert_eq!(
        super::push::parse_cursor(&cursor, "aaa", Some("bbb")),
        Some(50)
    );
    assert_eq!(super::push::parse_cursor(&cursor, "aaa", None), None);
    assert_eq!(super::push::parse_cursor(&cursor, "ccc", Some("bbb")), None);
}

#[test]
fn destinations_that_could_be_options_or_refspecs_are_refused() {
    for name in [
        "-f",
        "--force",
        "main:other",
        "a..b",
        "HEAD",
        "@{-1}",
        "x y",
        "",
    ] {
        assert!(!is_plausible_destination(name), "{name:?}");
    }
    let _ = configure;
}
