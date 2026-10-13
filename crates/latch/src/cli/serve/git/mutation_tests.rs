//! Git mutations against throwaway repositories: staging, commits, branches,
//! stale preconditions, request-id replay and receipt recovery.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::receipts::{self, Before, Stored};
use super::tests::{entry, git, home, repository, Fixture};
use super::*;
use crate::cli::serve::contract::{
    GitMutationKind, GitMutationReceipt, GitPreconditions, GitReceiptState, GitStatusEntry,
};

/// A repository whose own config carries everything a commit needs, since
/// tests run Git without any global configuration.
fn workspace() -> Fixture {
    let fixture = repository();
    for (key, value) in [
        ("user.name", "Latch Test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        git(&fixture.root, &["config", key, value]);
    }
    fixture
}

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

async fn read(fixture: &Fixture) -> GitStatus {
    status(&home(fixture), fixture.path(), &[]).await.unwrap()
}

fn target(entry: &GitStatusEntry) -> FileTarget {
    assert!(!entry.fingerprint.is_empty(), "entry has a fingerprint");
    FileTarget {
        path_key: entry.path_key.clone(),
        area: entry.area,
        fingerprint: entry.fingerprint.clone(),
    }
}

async fn run_as(
    fixture: &Fixture,
    device: Option<&str>,
    request: u32,
    expected: &GitPreconditions,
    mutation: Mutation,
) -> GitMutationReceipt {
    match submit(
        &home(fixture),
        device.map(str::to_owned),
        &id(request),
        fixture.path(),
        expected.clone(),
        mutation,
    )
    .await
    .expect("accepted")
    {
        Submitted::Settled(receipt) => receipt,
        Submitted::Pending(receipt) => panic!("still pending: {receipt:?}"),
    }
}

async fn run(
    fixture: &Fixture,
    request: u32,
    expected: &GitPreconditions,
    mutation: Mutation,
) -> GitMutationReceipt {
    run_as(fixture, Some("phone-a"), request, expected, mutation).await
}

fn code(receipt: &GitMutationReceipt) -> &str {
    receipt
        .outcome
        .as_ref()
        .map_or("", |outcome| outcome.code.as_str())
}

fn stage(files: Vec<FileTarget>) -> Mutation {
    Mutation::Stage {
        selection: Selection::Files(files),
        confirm_conflict_markers: false,
    }
}

fn unstage(files: Vec<FileTarget>) -> Mutation {
    Mutation::Unstage {
        selection: Selection::Files(files),
    }
}

fn commit(summary: &str) -> Mutation {
    Mutation::Commit {
        summary: summary.to_owned(),
        description: None,
    }
}

fn head(fixture: &Fixture) -> String {
    git(&fixture.root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn hook(fixture: &Fixture, name: &str, script: &str) {
    let path = fixture.root.join(".git/hooks").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

// MARK: - Stage and unstage

#[tokio::test]
async fn whole_files_stage_and_unstage_including_deletions_and_untracked() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "hello\nworld\n").unwrap();
    fs::write(fixture.root.join("doomed.txt"), "x\n").unwrap();
    git(&fixture.root, &["add", "doomed.txt"]);
    git(&fixture.root, &["commit", "-q", "-m", "doomed"]);
    fs::remove_file(fixture.root.join("doomed.txt")).unwrap();
    fs::create_dir(fixture.root.join("notes")).unwrap();
    fs::write(fixture.root.join("notes/a b.txt"), "a\n").unwrap();

    let before = read(&fixture).await;
    let files = vec![
        target(entry(&before, "README.md", GitChangeArea::Unstaged)),
        target(entry(&before, "doomed.txt", GitChangeArea::Unstaged)),
        target(entry(&before, "notes/", GitChangeArea::Untracked)),
    ];
    let receipt = run(&fixture, 1, &before.preconditions, stage(files)).await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    assert_eq!(code(&receipt), "staged");
    assert_eq!(receipt.operation, GitMutationKind::Stage);

    let after = read(&fixture).await;
    entry(&after, "README.md", GitChangeArea::Staged);
    assert_eq!(
        entry(&after, "doomed.txt", GitChangeArea::Staged).change,
        crate::cli::serve::contract::GitChangeKind::Deleted
    );
    entry(&after, "notes/a b.txt", GitChangeArea::Staged);
    assert!(after
        .entries
        .iter()
        .all(|e| e.area == GitChangeArea::Staged));

    let files = after.entries.iter().map(target).collect();
    let receipt = run(&fixture, 2, &after.preconditions, unstage(files)).await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let restored = read(&fixture).await;
    assert!(restored
        .entries
        .iter()
        .all(|e| e.area != GitChangeArea::Staged));
    // Unstaging never touches the files themselves.
    assert_eq!(
        fs::read_to_string(fixture.root.join("README.md")).unwrap(),
        "hello\nworld\n"
    );
    assert!(!fixture.root.join("doomed.txt").exists());
}

#[tokio::test]
async fn unusual_names_are_staged_exactly_and_never_as_patterns_or_options() {
    let fixture = workspace();
    let names: Vec<&[u8]> = vec![
        b"-rf",
        b"--all",
        b"line\nbreak.txt",
        b"*.md",
        b":(top)magic",
        b"quote\"d 'name'.txt",
        "caf\u{e9}.txt".as_bytes(),
    ];
    for name in &names {
        fs::write(fixture.root.join(path_of(name)), "x\n").unwrap();
    }
    // A decoy the glob `*.md` would match if it were a pattern.
    fs::write(fixture.root.join("decoy.md"), "decoy\n").unwrap();

    let before = read(&fixture).await;
    let files = names
        .iter()
        .map(|name| {
            let key = status::hex(name);
            target(
                before
                    .entries
                    .iter()
                    .find(|e| e.path_key == key)
                    .unwrap_or_else(|| panic!("{name:?} listed")),
            )
        })
        .collect();
    let receipt = run(&fixture, 1, &before.preconditions, stage(files)).await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");

    let after = read(&fixture).await;
    for name in &names {
        let key = status::hex(name);
        assert!(
            after
                .entries
                .iter()
                .any(|e| e.path_key == key && e.area == GitChangeArea::Staged),
            "{name:?} staged"
        );
    }
    entry(&after, "decoy.md", GitChangeArea::Untracked);
}

#[tokio::test]
async fn a_non_utf8_name_stages_when_the_filesystem_allows_it() {
    let fixture = workspace();
    let name: &[u8] = b"bad\xffname.txt";
    if fs::write(fixture.root.join(path_of(name)), "x\n").is_err() {
        // APFS refuses names that are not UTF-8; nothing to test there.
        return;
    }
    let before = read(&fixture).await;
    let key = status::hex(name);
    let file = before.entries.iter().find(|e| e.path_key == key).unwrap();
    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        stage(vec![target(file)]),
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
}

#[tokio::test]
async fn unstaging_a_staged_rename_restores_both_paths() {
    let fixture = workspace();
    git(&fixture.root, &["mv", "README.md", "renamed file.md"]);
    let before = read(&fixture).await;
    let renamed = entry(&before, "renamed file.md", GitChangeArea::Staged);
    assert_eq!(renamed.orig_path.as_deref(), Some("README.md"));

    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        unstage(vec![target(renamed)]),
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let after = read(&fixture).await;
    assert!(after
        .entries
        .iter()
        .all(|e| e.area != GitChangeArea::Staged));
    entry(&after, "README.md", GitChangeArea::Unstaged);
    entry(&after, "renamed file.md", GitChangeArea::Untracked);
}

#[tokio::test]
async fn stage_all_and_unstage_all_cover_what_was_listed() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "changed\n").unwrap();
    fs::write(fixture.root.join("new.txt"), "new\n").unwrap();
    let before = read(&fixture).await;

    // Only the Changes section: the untracked file stays untracked.
    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        Mutation::Stage {
            selection: Selection::All(vec![GitChangeArea::Unstaged]),
            confirm_conflict_markers: false,
        },
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let middle = read(&fixture).await;
    entry(&middle, "README.md", GitChangeArea::Staged);
    entry(&middle, "new.txt", GitChangeArea::Untracked);

    let receipt = run(
        &fixture,
        2,
        &middle.preconditions,
        Mutation::Stage {
            selection: Selection::All(vec![GitChangeArea::Untracked]),
            confirm_conflict_markers: false,
        },
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let staged = read(&fixture).await;
    assert_eq!(staged.entries.len(), 2);

    let receipt = run(
        &fixture,
        3,
        &staged.preconditions,
        Mutation::Unstage {
            selection: Selection::All(vec![GitChangeArea::Staged]),
        },
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let after = read(&fixture).await;
    assert!(after
        .entries
        .iter()
        .all(|e| e.area != GitChangeArea::Staged));
}

#[tokio::test]
async fn unstaging_on_an_unborn_branch_keeps_the_file() {
    let fixture = workspace();
    git(&fixture.root, &["checkout", "-q", "--orphan", "fresh"]);
    let before = read(&fixture).await;
    assert_eq!(before.preconditions.head, None);
    let staged = entry(&before, "README.md", GitChangeArea::Staged);
    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        unstage(vec![target(staged)]),
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let after = read(&fixture).await;
    entry(&after, "README.md", GitChangeArea::Untracked);
    assert!(fixture.root.join("README.md").exists());
}

// MARK: - Stale preconditions

#[tokio::test]
async fn a_file_edited_after_review_is_not_staged() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "reviewed\n").unwrap();
    let before = read(&fixture).await;
    let file = target(entry(&before, "README.md", GitChangeArea::Unstaged));
    // An agent writes again: same porcelain record, different file.
    fs::write(fixture.root.join("README.md"), "rewritten by an agent\n").unwrap();

    let receipt = run(&fixture, 1, &before.preconditions, stage(vec![file])).await;
    assert_eq!(receipt.state, GitReceiptState::Rejected);
    assert_eq!(code(&receipt), "refresh_required");
    let after = read(&fixture).await;
    assert!(after
        .entries
        .iter()
        .all(|e| e.area != GitChangeArea::Staged));
}

#[tokio::test]
async fn stage_all_refuses_when_anything_new_appeared() {
    let fixture = workspace();
    fs::write(fixture.root.join("seen.txt"), "seen\n").unwrap();
    let before = read(&fixture).await;
    fs::write(fixture.root.join("unseen.txt"), "unseen\n").unwrap();
    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        Mutation::Stage {
            selection: Selection::All(vec![GitChangeArea::Untracked]),
            confirm_conflict_markers: false,
        },
    )
    .await;
    assert_eq!(code(&receipt), "refresh_required");
    let after = read(&fixture).await;
    assert!(after
        .entries
        .iter()
        .all(|e| e.area == GitChangeArea::Untracked));
}

#[tokio::test]
async fn a_partially_fingerprinted_untracked_folder_cannot_be_staged() {
    let fixture = workspace();
    fs::create_dir(fixture.root.join("large")).unwrap();
    for n in 0..4097 {
        fs::write(fixture.root.join(format!("large/{n:04}.txt")), "").unwrap();
    }
    let before = read(&fixture).await;
    let folder = entry(&before, "large/", GitChangeArea::Untracked);
    for (request, mutation) in [
        (2001, stage(vec![target(folder)])),
        (
            2002,
            Mutation::Stage {
                selection: Selection::All(vec![GitChangeArea::Untracked]),
                confirm_conflict_markers: false,
            },
        ),
    ] {
        let result = run(&fixture, request, &before.preconditions, mutation).await;
        assert_eq!(code(&result), "too_many_changes", "{result:?}");
    }
    assert!(git(&fixture.root, &["diff", "--cached", "--name-only"]).is_empty());
}

#[tokio::test]
async fn a_commit_cannot_include_files_omitted_from_the_phone_review() {
    let fixture = workspace();
    for n in 0..2001 {
        fs::write(fixture.root.join(format!("file-{n:04}.txt")), "review\n").unwrap();
    }
    git(&fixture.root, &["add", "--all"]);
    let before = read(&fixture).await;
    assert!(before.entries_truncated);
    let old_head = head(&fixture);
    let result = run(
        &fixture,
        2003,
        &before.preconditions,
        commit("Must not commit hidden files"),
    )
    .await;
    assert_eq!(code(&result), "too_many_changes", "{result:?}");
    assert_eq!(head(&fixture), old_head);
}

#[tokio::test]
async fn an_external_commit_or_branch_switch_makes_every_action_stale() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "changed\n").unwrap();
    let before = read(&fixture).await;
    let file = target(entry(&before, "README.md", GitChangeArea::Unstaged));
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "from a terminal"],
    );

    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        stage(vec![file.clone()]),
    )
    .await;
    assert_eq!(code(&receipt), "refresh_required");

    let fresh = read(&fixture).await;
    git(&fixture.root, &["switch", "-q", "-c", "elsewhere"]);
    let receipt = run(&fixture, 2, &fresh.preconditions, stage(vec![file])).await;
    assert_eq!(code(&receipt), "refresh_required");
}

// MARK: - Commit

#[tokio::test]
async fn a_commit_contains_exactly_the_reviewed_index_and_leaves_other_work() {
    let fixture = workspace();
    fs::write(fixture.root.join("staged.txt"), "staged\n").unwrap();
    fs::write(fixture.root.join("README.md"), "unstaged edit\n").unwrap();
    git(&fixture.root, &["add", "staged.txt"]);
    let parent = head(&fixture);
    let before = read(&fixture).await;

    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        Mutation::Commit {
            summary: "Add staged file".to_owned(),
            description: Some("Body line one.\n\n# not a comment".to_owned()),
        },
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Completed, "{receipt:#?}");
    let outcome = receipt.outcome.unwrap();
    assert_eq!(outcome.code, "committed");
    assert_eq!(outcome.commit.as_deref(), Some(head(&fixture).as_str()));
    assert_eq!(outcome.branch.as_deref(), Some("main"));

    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD^"]).trim(), parent);
    let changed = git(&fixture.root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(changed.trim(), "staged.txt");
    let message = git(&fixture.root, &["log", "-1", "--format=%B"]);
    assert!(
        message.starts_with("Add staged file\n\nBody line one."),
        "{message}"
    );
    assert!(message.contains("# not a comment"), "{message}");
    // The unstaged edit is still there, still unstaged.
    assert_eq!(
        fs::read_to_string(fixture.root.join("README.md")).unwrap(),
        "unstaged edit\n"
    );
    let after = read(&fixture).await;
    entry(&after, "README.md", GitChangeArea::Unstaged);
}

#[tokio::test]
async fn a_commit_refuses_an_index_that_changed_after_review() {
    let fixture = workspace();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    fs::write(fixture.root.join("b.txt"), "b\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let before = read(&fixture).await;
    let parent = head(&fixture);
    // Another terminal stages more before the phone's commit runs.
    git(&fixture.root, &["add", "b.txt"]);

    let receipt = run(&fixture, 1, &before.preconditions, commit("Only a")).await;
    assert_eq!(receipt.state, GitReceiptState::Rejected);
    assert_eq!(code(&receipt), "refresh_required");
    assert_eq!(head(&fixture), parent);

    // Restaging the reviewed file with new content is also a change.
    git(&fixture.root, &["reset", "-q", "b.txt"]);
    let reviewed = read(&fixture).await;
    fs::write(fixture.root.join("a.txt"), "a, edited\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let receipt = run(&fixture, 2, &reviewed.preconditions, commit("Only a")).await;
    assert_eq!(code(&receipt), "refresh_required");
    assert_eq!(head(&fixture), parent);
}

#[tokio::test]
async fn nothing_staged_is_gits_refusal_not_a_success() {
    let fixture = workspace();
    let before = read(&fixture).await;
    let receipt = run(&fixture, 1, &before.preconditions, commit("Empty")).await;
    assert_eq!(receipt.state, GitReceiptState::Rejected);
    assert_eq!(code(&receipt), "nothing_to_commit");
}

#[tokio::test]
async fn missing_identity_failed_signing_and_hooks_are_reported_as_refusals() {
    let fixture = workspace();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let parent = head(&fixture);

    git(&fixture.root, &["config", "--unset", "user.name"]);
    git(&fixture.root, &["config", "--unset", "user.email"]);
    git(&fixture.root, &["config", "user.useConfigOnly", "true"]);
    let before = read(&fixture).await;
    let receipt = run(&fixture, 1, &before.preconditions, commit("No identity")).await;
    assert_eq!(receipt.state, GitReceiptState::Rejected, "{receipt:#?}");
    assert_eq!(code(&receipt), "identity_missing");
    assert!(receipt.outcome.unwrap().git_output.is_some());
    assert_eq!(head(&fixture), parent);

    git(&fixture.root, &["config", "user.name", "Latch Test"]);
    git(
        &fixture.root,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&fixture.root, &["config", "commit.gpgsign", "true"]);
    git(&fixture.root, &["config", "gpg.program", "/usr/bin/false"]);
    let receipt = run(&fixture, 2, &before.preconditions, commit("Unsigned")).await;
    assert_eq!(code(&receipt), "signing_failed", "{receipt:#?}");
    assert_eq!(head(&fixture), parent);

    git(&fixture.root, &["config", "commit.gpgsign", "false"]);
    hook(
        &fixture,
        "pre-commit",
        "#!/bin/sh\necho 'lint: 3 problems' >&2\nexit 1\n",
    );
    let receipt = run(&fixture, 3, &before.preconditions, commit("Hooked")).await;
    assert_eq!(code(&receipt), "hook_failed", "{receipt:#?}");
    let output = receipt.outcome.unwrap().git_output.unwrap();
    assert!(output.contains("lint: 3 problems"), "{output}");
    assert_eq!(head(&fixture), parent);

    // The hook runs, and a passing one lets the commit through.
    hook(
        &fixture,
        "pre-commit",
        "#!/bin/sh\ntouch hook-ran\nexit 0\n",
    );
    let receipt = run(&fixture, 4, &before.preconditions, commit("Hooked")).await;
    assert_eq!(code(&receipt), "committed", "{receipt:#?}");
    assert!(fixture.root.join("hook-ran").exists());
}

#[tokio::test]
async fn commit_hooks_cannot_add_unreviewed_files_to_the_commit_or_real_index() {
    for name in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        let fixture = workspace();
        fs::write(fixture.root.join("reviewed.txt"), "reviewed\n").unwrap();
        fs::write(fixture.root.join("extra.txt"), "not reviewed\n").unwrap();
        git(&fixture.root, &["add", "reviewed.txt"]);
        let before = head(&fixture);
        let index = git(&fixture.root, &["write-tree"]);
        hook(&fixture, name, "git add -- extra.txt");
        let snapshot = read(&fixture).await;
        let result = run(
            &fixture,
            1001,
            &snapshot.preconditions,
            commit("Reviewed only"),
        )
        .await;
        assert_eq!(code(&result), "refresh_required", "{name}: {result:?}");
        assert_eq!(
            head(&fixture),
            before,
            "{name} must not commit unreviewed staging"
        );
        assert_eq!(
            git(&fixture.root, &["write-tree"]),
            index,
            "the real index is preserved"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("extra.txt")).unwrap(),
            "not reviewed\n"
        );
    }
}

#[tokio::test]
async fn hooks_that_edit_working_files_without_staging_preserve_the_reviewed_commit() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "reviewed\n").unwrap();
    git(&fixture.root, &["add", "README.md"]);
    hook(&fixture, "pre-commit", "printf 'hook edit\\n' > README.md");
    let snapshot = read(&fixture).await;
    let result = run(
        &fixture,
        1002,
        &snapshot.preconditions,
        commit("Reviewed tree"),
    )
    .await;
    assert_eq!(code(&result), "committed", "{result:?}");
    assert_eq!(
        git(&fixture.root, &["show", "HEAD:README.md"]).trim(),
        "reviewed"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("README.md")).unwrap(),
        "hook edit\n"
    );
}

#[tokio::test]
async fn a_hung_hook_is_stopped_and_the_commit_reconciled() {
    let fixture = workspace();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let parent = head(&fixture);
    hook(&fixture, "pre-commit", "#!/bin/sh\nsleep 60\n");
    let before = read(&fixture).await;
    let started = std::time::Instant::now();
    let receipt = run(&fixture, 1, &before.preconditions, commit("Slow")).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    // Git was stopped before it committed, and the repository proves it.
    assert_eq!(receipt.state, GitReceiptState::Rejected, "{receipt:#?}");
    assert_eq!(code(&receipt), "not_applied");
    assert!(receipt.outcome.unwrap().reconciled);
    assert_eq!(head(&fixture), parent);
}

#[tokio::test]
async fn no_commit_while_a_merge_is_stopped() {
    let fixture = workspace();
    git(&fixture.root, &["switch", "-q", "-c", "side"]);
    fs::write(fixture.root.join("README.md"), "side\n").unwrap();
    git(&fixture.root, &["commit", "-q", "-am", "side"]);
    git(&fixture.root, &["switch", "-q", "main"]);
    fs::write(fixture.root.join("README.md"), "main\n").unwrap();
    git(&fixture.root, &["commit", "-q", "-am", "main"]);
    git(&fixture.root, &["merge", "-q", "side"]);
    let before = read(&fixture).await;
    assert!(before.operation.is_some());
    let receipt = run(&fixture, 1, &before.preconditions, commit("Conclude")).await;
    assert_eq!(code(&receipt), "operation_in_progress");
    assert!(fixture.root.join(".git/MERGE_HEAD").exists());
}

// MARK: - Request ids and receipts

#[tokio::test]
async fn a_repeated_request_id_returns_the_first_receipt_without_running_git_again() {
    let fixture = workspace();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let before = read(&fixture).await;
    let parent = head(&fixture);

    let first = run(&fixture, 7, &before.preconditions, commit("Once")).await;
    assert_eq!(code(&first), "committed");
    let committed = head(&fixture);
    // The phone lost the answer and sends the same request again.
    let second = run(&fixture, 7, &before.preconditions, commit("Once")).await;
    assert_eq!(second, first);
    assert_eq!(head(&fixture), committed);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD^"]).trim(), parent);

    // The same id with any other arguments is refused, and runs nothing.
    let reused = submit(
        &home(&fixture),
        Some("phone-a".to_owned()),
        &id(7),
        fixture.path(),
        before.preconditions.clone(),
        commit("Twice"),
    )
    .await;
    assert_eq!(reused.unwrap_err(), MutationError::RequestIdConflict);
    assert_eq!(head(&fixture), committed);

    // The receipt is there to look up afterwards.
    let looked_up = receipt(&home(&fixture), Some("phone-a"), &id(7))
        .await
        .unwrap();
    assert_eq!(looked_up, Some(first));
    // Uppercase is the same id.
    let upper = receipt(
        &home(&fixture),
        Some("phone-a"),
        &id(7).to_ascii_uppercase(),
    )
    .await
    .unwrap();
    assert!(upper.is_some());
}

#[tokio::test]
async fn receipts_belong_to_the_device_that_made_them() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "x\n").unwrap();
    let before = read(&fixture).await;
    let file = target(entry(&before, "README.md", GitChangeArea::Unstaged));
    let receipt_a = run_as(
        &fixture,
        Some("phone-a"),
        1,
        &before.preconditions,
        stage(vec![file]),
    )
    .await;
    assert_eq!(code(&receipt_a), "staged");

    let home = home(&fixture);
    assert_eq!(receipt(&home, Some("phone-b"), &id(1)).await.unwrap(), None);
    assert_eq!(receipt(&home, None, &id(1)).await.unwrap(), None);
    assert!(receipt(&home, Some("phone-a"), &id(1))
        .await
        .unwrap()
        .is_some());

    // Another device's use of the same id is its own request, not a replay
    // of phone A's and not a conflict that would reveal it.
    let fresh = read(&fixture).await;
    let staged = target(entry(&fresh, "README.md", GitChangeArea::Staged));
    let receipt_b = run_as(
        &fixture,
        Some("phone-b"),
        1,
        &fresh.preconditions,
        unstage(vec![staged]),
    )
    .await;
    assert_eq!(code(&receipt_b), "unstaged");
    assert_eq!(
        receipt(&home, Some("phone-a"), &id(1)).await.unwrap(),
        Some(receipt_a)
    );
}

#[tokio::test]
async fn concurrent_commits_from_two_phones_run_one_after_the_other() {
    let fixture = workspace();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    let before = read(&fixture).await;
    let parent = head(&fixture);
    let home = home(&fixture);
    let (first, second) = (id(1), id(2));
    let (one, two) = tokio::join!(
        submit(
            &home,
            Some("phone-a".into()),
            &first,
            fixture.path(),
            before.preconditions.clone(),
            commit("From A")
        ),
        submit(
            &home,
            Some("phone-b".into()),
            &second,
            fixture.path(),
            before.preconditions.clone(),
            commit("From B")
        ),
    );
    let settled = |result: Result<Submitted, MutationError>| match result.unwrap() {
        Submitted::Settled(receipt) => receipt,
        Submitted::Pending(receipt) => panic!("pending {receipt:?}"),
    };
    let mut codes = [
        code(&settled(one)).to_owned(),
        code(&settled(two)).to_owned(),
    ];
    codes.sort();
    assert_eq!(codes, ["committed", "refresh_required"]);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD^"]).trim(), parent);
}

/// Writes a receipt the way a gateway that crashed mid-request leaves it.
fn abandoned(fixture: &Fixture, request: u32, mutation: Mutation, before: Option<Before>) {
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
async fn old_request_ids_remain_deduplicated_after_more_than_512_new_receipts() {
    let fixture = workspace();
    let expected = read(&fixture).await.preconditions;
    let first = run(&fixture, 9000, &expected, commit("Nothing staged")).await;
    let home = home(&fixture);
    let template = receipts::load(&home, Some("phone-a"), &id(9000))
        .unwrap()
        .unwrap();
    for n in 9001..9520 {
        let mut stored = template.clone();
        stored.receipt.request_id = id(n);
        assert!(matches!(
            receipts::claim(&home, &stored).unwrap(),
            receipts::Claim::Claimed
        ));
    }
    // A file staged much later must not be committed by an old retry.
    fs::write(fixture.root.join("later.txt"), "later\n").unwrap();
    git(&fixture.root, &["add", "later.txt"]);
    let replay = run(&fixture, 9000, &expected, commit("Nothing staged")).await;
    assert_eq!(replay, first);
    assert!(receipts::load(&home, Some("phone-a"), &id(9000))
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn receipts_left_pending_by_a_restart_are_reconciled_from_the_repository() {
    let fixture = workspace();
    let home = home(&fixture);
    let parent = head(&fixture);
    let at = |head: &str| {
        Some(Before {
            head: Some(head.to_owned()),
            branch: Some("main".to_owned()),
            targets: Vec::new(),
            ..Before::default()
        })
    };

    // Accepted, but the gateway stopped before Git started.
    abandoned(&fixture, 1, commit("Never ran"), None);
    let settled = receipt(&home, Some("phone-a"), &id(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Rejected);
    assert_eq!(code(&settled), "not_applied");

    // Git started; the branch did not move, so no commit was made.
    abandoned(&fixture, 2, commit("Did not finish"), at(&parent));
    let settled = receipt(&home, Some("phone-a"), &id(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code(&settled), "not_applied");
    assert!(settled.outcome.as_ref().unwrap().reconciled);

    // Git started and the commit it was making is there.
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    git(&fixture.root, &["add", "a.txt"]);
    git(&fixture.root, &["config", "user.name", "Latch Test"]);
    abandoned(&fixture, 3, commit("Made it"), at(&parent));
    git(&fixture.root, &["commit", "-q", "-m", "Made it"]);
    let settled = receipt(&home, Some("phone-a"), &id(3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Completed, "{settled:#?}");
    assert_eq!(
        settled.outcome.as_ref().unwrap().commit.as_deref(),
        Some(head(&fixture).as_str())
    );

    // The branch moved, but not in a way the request explains.
    abandoned(&fixture, 4, commit("Something else"), at(&parent));
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "unrelated"],
    );
    let settled = receipt(&home, Some("phone-a"), &id(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Uncertain);

    // A Git still holding the index lock settles nothing.
    abandoned(&fixture, 5, commit("Locked"), at(&head(&fixture)));
    fs::write(fixture.root.join(".git/index.lock"), "").unwrap();
    let settled = receipt(&home, Some("phone-a"), &id(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Uncertain);
    fs::remove_file(fixture.root.join(".git/index.lock")).unwrap();

    // Once settled, the receipt stays as decided.
    let again = receipt(&home, Some("phone-a"), &id(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again, settled_after(&home, 4).await);
}

async fn settled_after(home: &LatchHome, request: u32) -> GitMutationReceipt {
    receipts::load(home, Some("phone-a"), &id(request))
        .unwrap()
        .unwrap()
        .receipt
}

#[tokio::test]
async fn an_interrupted_stage_is_completed_or_uncertain_never_not_applied() {
    let fixture = workspace();
    fs::write(fixture.root.join("README.md"), "x\n").unwrap();
    let before = read(&fixture).await;
    let file = target(entry(&before, "README.md", GitChangeArea::Unstaged));
    let state = Some(Before {
        head: before.preconditions.head.clone(),
        branch: before.preconditions.branch.clone(),
        targets: vec![file.path_key.clone()],
        ..Before::default()
    });
    abandoned(&fixture, 1, stage(vec![file.clone()]), state.clone());
    let settled = receipt(&home(&fixture), Some("phone-a"), &id(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Uncertain);

    git(&fixture.root, &["add", "README.md"]);
    abandoned(&fixture, 2, stage(vec![file]), state);
    let settled = receipt(&home(&fixture), Some("phone-a"), &id(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, GitReceiptState::Completed);
}

#[tokio::test]
async fn a_folder_that_is_not_a_repository_is_refused_before_anything_is_accepted() {
    let fixture = workspace();
    let plain = fixture.base.join("plain");
    fs::create_dir(&plain).unwrap();
    let before = read(&fixture).await;
    let result = submit(
        &home(&fixture),
        Some("phone-a".into()),
        &id(1),
        plain.to_str().unwrap(),
        before.preconditions,
        commit("x"),
    )
    .await;
    assert_eq!(
        result.unwrap_err(),
        MutationError::Git(GitError::NotARepository)
    );
    assert!(receipt(&home(&fixture), Some("phone-a"), &id(1))
        .await
        .unwrap()
        .is_none());
}

// MARK: - Branches

#[tokio::test]
async fn branches_list_the_current_one_first_with_upstreams_and_other_worktrees() {
    let fixture = workspace();
    git(&fixture.root, &["branch", "feature"]);
    git(
        &fixture.root,
        &["branch", "--set-upstream-to=feature", "main"],
    );
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "ahead"],
    );
    let other = fixture.base.join("other-tree");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "parked",
            other.to_str().unwrap(),
        ],
    );

    let list = branches(fixture.path()).await.unwrap();
    assert_eq!(list.branches[0].name, "main");
    assert!(list.branches[0].current);
    assert_eq!(list.branches[0].upstream.as_deref(), Some("feature"));
    assert_eq!(
        (list.branches[0].ahead, list.branches[0].behind),
        (Some(1), Some(0))
    );
    assert_eq!(list.branches[0].subject, "ahead");
    let parked = list.branches.iter().find(|b| b.name == "parked").unwrap();
    assert!(parked.checked_out_elsewhere);
    let feature = list.branches.iter().find(|b| b.name == "feature").unwrap();
    assert!(!feature.checked_out_elsewhere);
    assert_eq!(feature.upstream, None);
    assert_eq!(list.preconditions, read(&fixture).await.preconditions);
    assert!(serde_json::to_vec(&list).unwrap().len() < 512 * 1024);
}

#[tokio::test]
async fn a_branch_is_created_at_the_reviewed_commit_and_optionally_switched_to() {
    let fixture = workspace();
    let before = read(&fixture).await;
    let oid = before.preconditions.head.clone().unwrap();
    let create = |name: &str, start: &str, start_oid: &str, switch: bool| Mutation::CreateBranch {
        name: name.to_owned(),
        start_point: start.to_owned(),
        start_oid: start_oid.to_owned(),
        switch,
    };

    let receipt = run(
        &fixture,
        1,
        &before.preconditions,
        create("topic", "main", &oid, false),
    )
    .await;
    assert_eq!(code(&receipt), "branch_created", "{receipt:#?}");
    assert_eq!(git(&fixture.root, &["rev-parse", "topic"]).trim(), oid);
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "main"
    );

    let receipt = run(
        &fixture,
        2,
        &before.preconditions,
        create("topic", "HEAD", &oid, false),
    )
    .await;
    assert_eq!(code(&receipt), "branch_exists", "{receipt:#?}");

    let receipt = run(
        &fixture,
        3,
        &before.preconditions,
        create("bad..name", "HEAD", &oid, false),
    )
    .await;
    assert_eq!(code(&receipt), "invalid_branch_name");

    let receipt = run(
        &fixture,
        4,
        &before.preconditions,
        create("work/next", "HEAD", &oid, true),
    )
    .await;
    assert_eq!(code(&receipt), "branch_created", "{receipt:#?}");
    assert_eq!(
        receipt.outcome.unwrap().branch.as_deref(),
        Some("work/next")
    );

    // The start point moved after review: nothing is created.
    let fresh = read(&fixture).await;
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "moved"],
    );
    let moved = read(&fixture).await;
    let receipt = run(
        &fixture,
        5,
        &moved.preconditions,
        create("late", "work/next", &oid, false),
    )
    .await;
    assert_eq!(code(&receipt), "refresh_required");
    let receipt = run(
        &fixture,
        6,
        &fresh.preconditions,
        create("late", "HEAD", &oid, false),
    )
    .await;
    assert_eq!(code(&receipt), "refresh_required");
    assert!(git(&fixture.root, &["branch", "--list", "late"])
        .trim()
        .is_empty());
}

#[tokio::test]
async fn a_switch_that_would_overwrite_work_returns_gits_refusal_and_keeps_the_work() {
    let fixture = workspace();
    git(&fixture.root, &["switch", "-q", "-c", "other"]);
    fs::write(fixture.root.join("README.md"), "other\n").unwrap();
    git(&fixture.root, &["commit", "-q", "-am", "other"]);
    git(&fixture.root, &["switch", "-q", "main"]);
    fs::write(fixture.root.join("README.md"), "precious local work\n").unwrap();

    let list = branches(fixture.path()).await.unwrap();
    let other = list.branches.iter().find(|b| b.name == "other").unwrap();
    let switch = Mutation::SwitchBranch {
        name: "other".to_owned(),
        oid: other.oid.clone(),
    };
    let receipt = run(&fixture, 1, &list.preconditions, switch.clone()).await;
    assert_eq!(receipt.state, GitReceiptState::Rejected);
    let outcome = receipt.outcome.unwrap();
    assert_eq!(outcome.code, "would_overwrite");
    assert_eq!(outcome.files, ["README.md"]);
    assert!(outcome.git_output.unwrap().contains("would be overwritten"));
    // Nothing was stashed, discarded or reset.
    assert_eq!(
        fs::read_to_string(fixture.root.join("README.md")).unwrap(),
        "precious local work\n"
    );
    assert!(git(&fixture.root, &["stash", "list"]).trim().is_empty());
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "main"
    );

    // With the work committed the switch goes through.
    git(&fixture.root, &["commit", "-q", "-am", "keep"]);
    let list = branches(fixture.path()).await.unwrap();
    let receipt = run(&fixture, 2, &list.preconditions, switch).await;
    assert_eq!(code(&receipt), "switched", "{receipt:#?}");
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "other"
    );
}

#[tokio::test]
async fn switching_refuses_a_moved_missing_or_borrowed_branch() {
    let fixture = workspace();
    git(&fixture.root, &["branch", "target"]);
    let other = fixture.base.join("borrowed-tree");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "borrowed",
            other.to_str().unwrap(),
        ],
    );
    let list = branches(fixture.path()).await.unwrap();
    let oid = |name: &str| {
        list.branches
            .iter()
            .find(|b| b.name == name)
            .unwrap()
            .oid
            .clone()
    };
    let switch = |name: &str, oid: String| Mutation::SwitchBranch {
        name: name.to_owned(),
        oid,
    };

    let stale = "0".repeat(40);
    let receipt = run(&fixture, 1, &list.preconditions, switch("target", stale)).await;
    assert_eq!(code(&receipt), "refresh_required");
    let receipt = run(
        &fixture,
        2,
        &list.preconditions,
        switch("missing", oid("target")),
    )
    .await;
    assert_eq!(code(&receipt), "not_found");
    let receipt = run(
        &fixture,
        3,
        &list.preconditions,
        switch("borrowed", oid("borrowed")),
    )
    .await;
    assert_eq!(receipt.state, GitReceiptState::Rejected, "{receipt:#?}");
    assert_eq!(code(&receipt), "git_refused");
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "main"
    );
}

#[test]
fn refusal_file_lists_are_read_from_gits_message() {
    let text = "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt\n\tdir/b c.txt\nPlease commit your changes or stash them before you switch branches.\nerror: The following untracked working tree files would be overwritten by checkout:\n\tnew.txt\nAborting\n";
    assert_eq!(
        ops::overwritten_files(text),
        ["a.txt", "dir/b c.txt", "new.txt"]
    );
}

#[test]
fn branch_names_that_read_as_options_or_revisions_are_refused() {
    for bad in [
        "",
        "-f",
        "--force",
        "HEAD",
        "@",
        "a@{1}",
        "has space",
        "tab\tname",
        "new\nline",
        "main~3",
        "main^",
        "a:b",
        "*",
        "a..b",
        ".hidden",
        "x/.y",
        "x.lock",
        "x/",
        "/x",
        "a//b",
        "a\\b",
    ] {
        assert!(!is_plausible_branch_name(bad), "{bad:?}");
    }
    for good in ["main", "feature/x", "fix-123", "caf\u{e9}"] {
        assert!(is_plausible_branch_name(good), "{good:?}");
    }
}

#[test]
fn path_of_round_trips_bytes() {
    let bytes = b"a\xffb";
    assert_eq!(path_of(bytes).as_os_str().as_encoded_bytes(), bytes);
    let _ = Path::new("unused");
}
