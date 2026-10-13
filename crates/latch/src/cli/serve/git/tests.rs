//! Every repository here is a throwaway under a temporary directory. Setup
//! runs Git with no system or global configuration so the user's own setup
//! cannot shape a fixture; the code under test runs Git exactly as the
//! gateway does.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::cli::serve::contract::{
    GitChangeKind, GitDiffLineKind, GitOperationKind, GitWorktreeKind,
};

pub(super) fn git(dir: &Path, words: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(words)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Latch Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Latch Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        output.status.success()
            || words.first() == Some(&"merge")
            || words.first() == Some(&"rebase"),
        "git {words:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(super) struct Fixture {
    _temp: tempfile::TempDir,
    pub(super) base: PathBuf,
    pub(super) root: PathBuf,
}

impl Fixture {
    pub(super) fn path(&self) -> &str {
        self.root.to_str().unwrap()
    }
}

/// A temporary directory that no repository contains, so "not a repository"
/// means what it says. A `TMPDIR` inside a checkout (as a supervising tool
/// may set) would otherwise make every fixture part of that checkout.
fn outside_any_repository() -> tempfile::TempDir {
    let preferred = std::env::temp_dir();
    let inside = fs::canonicalize(&preferred)
        .unwrap_or_else(|_| preferred.clone())
        .ancestors()
        .any(|dir| dir.join(".git").exists());
    let parent = if inside {
        PathBuf::from("/tmp")
    } else {
        preferred
    };
    tempfile::Builder::new()
        .prefix("latch-git-test-")
        .tempdir_in(parent)
        .unwrap()
}

/// A repository on `main` with one committed file.
pub(super) fn repository() -> Fixture {
    let temp = outside_any_repository();
    let base = fs::canonicalize(temp.path()).unwrap();
    let root = base.join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("README.md"), "hello\n").unwrap();
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-q", "-m", "initial"]);
    Fixture {
        _temp: temp,
        base,
        root,
    }
}

pub(super) fn entry<'a>(
    status: &'a GitStatus,
    path: &str,
    area: GitChangeArea,
) -> &'a crate::cli::serve::contract::GitStatusEntry {
    status
        .entries
        .iter()
        .find(|entry| entry.path == path && entry.area == area)
        .unwrap_or_else(|| panic!("no {area:?} entry for {path:?} in {:#?}", status.entries))
}

pub(super) fn home(fixture: &Fixture) -> LatchHome {
    LatchHome::new(fixture.base.join("latch-home"))
}

// MARK: - Worktree resolution

#[tokio::test]
async fn a_repository_and_its_subfolders_resolve_to_the_canonical_root() {
    let fixture = repository();
    fs::create_dir_all(fixture.root.join("src/deep")).unwrap();
    let resolved = resolve(fixture.root.join("src/deep").to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(resolved.root, fixture.path());
    assert_eq!(resolved.name, "repo");
    assert_eq!(resolved.kind, GitWorktreeKind::Repository);

    // A symlinked folder resolves to the same canonical root, not the link.
    std::os::unix::fs::symlink(&fixture.root, fixture.base.join("alias")).unwrap();
    let through_link = resolve(fixture.base.join("alias/src").to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(through_link.root, fixture.path());

    // `..` is resolved before Git sees it, and still lands on the same root.
    let dotted = format!("{}/src/deep/../..", fixture.path());
    assert_eq!(resolve(&dotted).await.unwrap().root, fixture.path());
}

#[tokio::test]
async fn a_linked_worktree_with_a_git_file_is_its_own_root() {
    let fixture = repository();
    let linked = fixture.base.join("linked");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    assert!(
        linked.join(".git").is_file(),
        ".git is a file in a linked worktree"
    );
    let resolved = resolve(linked.to_str().unwrap()).await.unwrap();
    assert_eq!(resolved.root, linked.to_str().unwrap());
    assert_eq!(resolved.kind, GitWorktreeKind::LinkedWorktree);

    let status = status(&home(&fixture), linked.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(status.head.branch.as_deref(), Some("feature"));
    assert_eq!(status.kind, GitWorktreeKind::LinkedWorktree);
}

#[tokio::test]
async fn a_submodule_checkout_resolves_as_a_submodule() {
    let fixture = repository();
    let library = fixture.base.join("library");
    fs::create_dir(&library).unwrap();
    git(&library, &["init", "-q", "-b", "main"]);
    fs::write(library.join("lib.txt"), "one\n").unwrap();
    git(&library, &["add", "lib.txt"]);
    git(&library, &["commit", "-q", "-m", "lib"]);
    git(
        &fixture.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            library.to_str().unwrap(),
            "vendor/library",
        ],
    );
    git(&fixture.root, &["commit", "-q", "-m", "add submodule"]);
    let checkout = fixture.root.join("vendor/library");
    assert!(
        checkout.join(".git").is_file(),
        ".git is a file in a submodule"
    );

    let resolved = resolve(checkout.to_str().unwrap()).await.unwrap();
    assert_eq!(resolved.root, checkout.to_str().unwrap());
    assert_eq!(resolved.kind, GitWorktreeKind::Submodule);

    // A new commit inside the submodule is a submodule entry in the
    // superproject, and its diff explains itself instead of showing text.
    fs::write(checkout.join("lib.txt"), "two\n").unwrap();
    git(&checkout, &["commit", "-q", "-am", "bump"]);
    let parent = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    let row = entry(&parent, "vendor/library", GitChangeArea::Unstaged);
    assert!(row.submodule);
    let diff = file_diff(fixture.path(), &row.path_key, GitChangeArea::Unstaged, None)
        .await
        .unwrap();
    assert_eq!(diff.kind, GitDiffKind::Submodule);
    assert!(diff.hunks.is_empty());
}

#[tokio::test]
async fn bare_repositories_plain_folders_and_git_dirs_are_refused() {
    let fixture = repository();
    let bare = fixture.base.join("bare.git");
    fs::create_dir(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    assert_eq!(
        resolve(bare.to_str().unwrap()).await,
        Err(GitError::BareRepository)
    );

    let plain = fixture.base.join("plain");
    fs::create_dir(&plain).unwrap();
    assert_eq!(
        resolve(plain.to_str().unwrap()).await,
        Err(GitError::NotARepository)
    );
    assert_eq!(
        resolve(fixture.root.join(".git").to_str().unwrap()).await,
        Err(GitError::NotARepository)
    );
    assert_eq!(resolve("relative/path").await, Err(GitError::InvalidPath));
    assert_eq!(resolve("").await, Err(GitError::InvalidPath));
    let file = fixture.root.join("README.md");
    assert_eq!(
        resolve(file.to_str().unwrap()).await,
        Err(GitError::InvalidPath)
    );
}

#[tokio::test]
async fn a_repository_deleted_after_opening_is_refused_on_the_next_call() {
    let fixture = repository();
    let opened = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    fs::remove_dir_all(&fixture.root).unwrap();
    assert_eq!(
        status(&home(&fixture), &opened.root, &[]).await,
        Err(GitError::UnavailablePath)
    );
    assert_eq!(
        file_diff(&opened.root, "61", GitChangeArea::Unstaged, None).await,
        Err(GitError::UnavailablePath)
    );
}

// MARK: - Porcelain v2 parsing

#[test]
fn porcelain_keeps_renames_newlines_and_non_utf8_bytes_as_data() {
    let mut output = Vec::new();
    output.extend_from_slice(b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0");
    output.extend_from_slice(b"# branch.head main\0");
    output.extend_from_slice(b"# branch.upstream origin/main\0");
    output.extend_from_slice(b"# branch.ab +2 -1\0");
    output.extend_from_slice(
        b"1 .M N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa with space.txt\0",
    );
    output.extend_from_slice(
        b"2 R. N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa R100 new\nname.txt\0old name.txt\0",
    );
    output.extend_from_slice(
        b"1 MM N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bad\xffname\0",
    );
    output.extend_from_slice(
        b"u UU N... 100644 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -dash.txt\0",
    );
    output.extend_from_slice(b"? untracked dir/\0");
    output.extend_from_slice(b"! ignored.o\0");

    let parsed = status::parse(&output, true);
    assert_eq!(parsed.branch.head.as_deref(), Some("main"));
    assert_eq!(parsed.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!(parsed.branch.ahead_behind, Some((2, 1)));
    assert_eq!(parsed.records.len(), 6);
    assert_eq!(parsed.records[1].path, b"new\nname.txt");
    assert_eq!(
        parsed.records[1].orig_path.as_deref(),
        Some(&b"old name.txt"[..])
    );
    assert_eq!(parsed.records[2].path, b"bad\xffname");

    let entries = status::entries(&parsed.records, &HashMap::new(), &HashMap::new());
    let summary = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.area, entry.change))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (
                "with space.txt",
                GitChangeArea::Unstaged,
                GitChangeKind::Modified
            ),
            (
                "new\nname.txt",
                GitChangeArea::Staged,
                GitChangeKind::Renamed
            ),
            (
                "bad\u{fffd}name",
                GitChangeArea::Staged,
                GitChangeKind::Modified
            ),
            (
                "bad\u{fffd}name",
                GitChangeArea::Unstaged,
                GitChangeKind::Modified
            ),
            (
                "-dash.txt",
                GitChangeArea::Conflicted,
                GitChangeKind::Conflicted
            ),
            (
                "untracked dir/",
                GitChangeArea::Untracked,
                GitChangeKind::Untracked
            ),
        ]
    );
    assert_eq!(entries[1].orig_path.as_deref(), Some("old name.txt"));
    // The key round-trips the exact bytes, including the invalid one.
    assert_eq!(status::unhex(&entries[2].path_key).unwrap(), b"bad\xffname");
    assert_eq!(entries[4].conflict.as_deref(), Some("UU"));
}

#[test]
fn porcelain_reads_detached_and_unborn_heads() {
    let detached = status::parse(
        b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0# branch.head (detached)\0",
        true,
    );
    let head = status::head(&detached.branch);
    assert!(head.detached);
    assert_eq!(head.branch, None);
    assert_eq!(
        head.oid.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(status::upstream(&detached.branch), None);

    let unborn = status::parse(b"# branch.oid (initial)\0# branch.head main\0", true);
    assert_eq!(status::head(&unborn.branch).oid, None);
    assert!(!status::head(&unborn.branch).detached);

    // An upstream whose ref is gone has no counts rather than zeros.
    let gone = status::parse(b"# branch.head main\0# branch.upstream origin/gone\0", true);
    let upstream = status::upstream(&gone.branch).unwrap();
    assert_eq!((upstream.ahead, upstream.behind), (None, None));
}

#[test]
fn a_truncated_stream_drops_only_its_partial_last_record() {
    let full = b"? one.txt\0? two.txt\0? three";
    let parsed = status::parse(full, false);
    let paths = parsed
        .records
        .iter()
        .map(|r| r.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(paths, [b"one.txt".to_vec(), b"two.txt".to_vec()]);
    // A rename whose original path was cut off is dropped whole.
    let cut_rename = b"2 R. N... 100644 100644 100644 a a R100 new.txt\0old";
    assert!(status::parse(cut_rename, false).records.is_empty());
}

#[test]
fn numstat_counts_text_binary_and_renamed_paths() {
    let counts =
        status::parse_numstat(b"3\t1\tsrc/a.rs\0-\t-\timage.png\x002\t0\t\0old.txt\0new.txt\0");
    assert_eq!(counts[&b"src/a.rs".to_vec()].additions, Some(3));
    assert!(counts[&b"image.png".to_vec()].binary);
    assert_eq!(counts[&b"new.txt".to_vec()].additions, Some(2));
    assert!(!counts.contains_key(b"old.txt".as_slice()));
}

#[test]
fn file_keys_cannot_leave_the_worktree() {
    for bad in [
        &b"../outside"[..],
        b"/etc/passwd",
        b"a/../../b",
        b"./a",
        b"a//b",
        b"",
        b"a\0b",
    ] {
        assert!(!is_relative_path(bad), "{:?}", String::from_utf8_lossy(bad));
    }
    for good in [
        &b"a"[..],
        b"src/main.rs",
        b"dir/",
        b"-rf",
        b"..hidden",
        b"a..b",
    ] {
        assert!(
            is_relative_path(good),
            "{:?}",
            String::from_utf8_lossy(good)
        );
    }
    assert_eq!(status::unhex("zz"), None);
    assert_eq!(status::unhex("abc"), None);
    assert_eq!(status::unhex("2e2e2f"), Some(b"../".to_vec()));
}

// MARK: - Status against real repositories

#[tokio::test]
async fn status_lists_unusual_names_renames_and_counts() {
    let fixture = repository();
    let root = &fixture.root;
    for name in [
        "with space.txt",
        "-leading-dash.txt",
        "quote\"s.txt",
        "é-unicode.txt",
        "new\nline.txt",
    ] {
        fs::write(root.join(name), "one\n").unwrap();
    }
    fs::write(root.join("to-rename.txt"), "keep me\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "names"]);
    for name in [
        "with space.txt",
        "-leading-dash.txt",
        "quote\"s.txt",
        "é-unicode.txt",
        "new\nline.txt",
    ] {
        fs::write(root.join(name), "one\ntwo\n").unwrap();
    }
    git(root, &["mv", "to-rename.txt", "renamed.txt"]);
    git(root, &["add", "--", "quote\"s.txt"]);
    fs::write(root.join("untracked.txt"), "new\n").unwrap();
    fs::create_dir(root.join("fresh")).unwrap();
    fs::write(root.join("fresh/inside.txt"), "x\n").unwrap();

    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert_eq!(status.head.branch.as_deref(), Some("main"));
    assert!(status.head.oid.is_some());
    for name in [
        "with space.txt",
        "-leading-dash.txt",
        "é-unicode.txt",
        "new\nline.txt",
    ] {
        let row = entry(&status, name, GitChangeArea::Unstaged);
        assert_eq!(row.change, GitChangeKind::Modified);
        assert_eq!((row.additions, row.deletions), (Some(1), Some(0)), "{name}");
    }
    let staged = entry(&status, "quote\"s.txt", GitChangeArea::Staged);
    assert_eq!(staged.additions, Some(1));
    let renamed = entry(&status, "renamed.txt", GitChangeArea::Staged);
    assert_eq!(renamed.change, GitChangeKind::Renamed);
    assert_eq!(renamed.orig_path.as_deref(), Some("to-rename.txt"));
    entry(&status, "untracked.txt", GitChangeArea::Untracked);
    entry(&status, "fresh/", GitChangeArea::Untracked);
    assert!(!status.entries_truncated);
    assert_eq!(status.total_entries as usize, status.entries.len());

    // Each listed file opens its own diff through its key.
    let newline = entry(&status, "new\nline.txt", GitChangeArea::Unstaged);
    let diff = file_diff(
        fixture.path(),
        &newline.path_key,
        GitChangeArea::Unstaged,
        None,
    )
    .await
    .unwrap();
    assert_eq!((diff.additions, diff.deletions), (1, 0));
}

#[tokio::test]
async fn a_non_utf8_file_name_round_trips_when_the_filesystem_allows_it() {
    let fixture = repository();
    let name = std::ffi::OsStr::from_bytes(b"bad\xffname.txt");
    if fs::write(fixture.root.join(name), "x\n").is_err() {
        // APFS refuses names that are not UTF-8; the parser test above covers
        // the bytes, and this filesystem cannot hold them.
        return;
    }
    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    let row = status
        .entries
        .iter()
        .find(|entry| entry.area == GitChangeArea::Untracked && entry.path.contains('\u{fffd}'))
        .expect("untracked non-UTF-8 entry");
    assert_eq!(status::unhex(&row.path_key).unwrap(), b"bad\xffname.txt");
    let diff = file_diff(
        fixture.path(),
        &row.path_key,
        GitChangeArea::Untracked,
        None,
    )
    .await
    .unwrap();
    assert_eq!(diff.additions, 1);
}

#[tokio::test]
async fn ahead_behind_and_the_last_fetch_come_from_local_refs_only() {
    let fixture = repository();
    let remote = fixture.base.join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &fixture.root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&fixture.root, &["push", "-q", "-u", "origin", "main"]);

    let never_fetched = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert_eq!(never_fetched.last_fetched_at, None);
    let upstream = never_fetched.upstream.unwrap();
    assert_eq!(upstream.name, "origin/main");
    assert_eq!((upstream.ahead, upstream.behind), (Some(0), Some(0)));

    // Someone else pushes; until this clone fetches, nothing changes.
    let other = fixture.base.join("other");
    git(
        &fixture.base,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("theirs.txt"), "x\n").unwrap();
    git(&other, &["add", "theirs.txt"]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q", "origin", "main"]);
    fs::write(fixture.root.join("mine.txt"), "y\n").unwrap();
    git(&fixture.root, &["add", "mine.txt"]);
    git(&fixture.root, &["commit", "-q", "-m", "mine"]);

    let before_fetch = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    let upstream = before_fetch.upstream.unwrap();
    assert_eq!((upstream.ahead, upstream.behind), (Some(1), Some(0)));
    assert_eq!(before_fetch.last_fetched_at, None, "status never fetches");
    assert!(!fixture.root.join(".git/FETCH_HEAD").exists());

    git(&fixture.root, &["fetch", "-q", "origin"]);
    let after_fetch = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    let upstream = after_fetch.upstream.unwrap();
    assert_eq!((upstream.ahead, upstream.behind), (Some(1), Some(1)));
    assert!(after_fetch.last_fetched_at.is_some());
    let fetched = fs::metadata(fixture.root.join(".git/FETCH_HEAD"))
        .unwrap()
        .modified()
        .unwrap();
    let _ = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert_eq!(
        fs::metadata(fixture.root.join(".git/FETCH_HEAD"))
            .unwrap()
            .modified()
            .unwrap(),
        fetched,
        "reading status leaves FETCH_HEAD alone"
    );
}

#[tokio::test]
async fn a_detached_head_reports_its_commit() {
    let fixture = repository();
    git(&fixture.root, &["checkout", "-q", "--detach"]);
    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert!(status.head.detached);
    assert_eq!(status.head.branch, None);
    let sha = git(&fixture.root, &["rev-parse", "HEAD"]);
    assert_eq!(status.head.oid.as_deref(), Some(sha.trim()));
    assert_eq!(status.upstream, None);
}

fn conflicting_branches(fixture: &Fixture) {
    let root = &fixture.root;
    git(root, &["checkout", "-q", "-b", "theirs"]);
    fs::write(root.join("README.md"), "theirs\n").unwrap();
    git(root, &["commit", "-q", "-am", "theirs"]);
    git(root, &["checkout", "-q", "main"]);
    fs::write(root.join("README.md"), "ours\n").unwrap();
    git(root, &["commit", "-q", "-am", "ours"]);
}

#[tokio::test]
async fn a_stopped_merge_lists_its_conflict_and_shows_the_markers() {
    let fixture = repository();
    conflicting_branches(&fixture);
    git(&fixture.root, &["merge", "-q", "theirs"]);
    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert_eq!(
        status.operation.as_ref().map(|op| op.kind),
        Some(GitOperationKind::Merge)
    );
    let conflicted = entry(&status, "README.md", GitChangeArea::Conflicted);
    assert_eq!(conflicted.conflict.as_deref(), Some("UU"));

    let diff = file_diff(
        fixture.path(),
        &conflicted.path_key,
        GitChangeArea::Conflicted,
        None,
    )
    .await
    .unwrap();
    assert_eq!(diff.kind, GitDiffKind::Text);
    let texts = diff.hunks[0]
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>();
    assert!(
        texts.iter().any(|text| text.starts_with("<<<<<<<")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.starts_with(">>>>>>>")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn a_stopped_rebase_reports_its_step() {
    let fixture = repository();
    conflicting_branches(&fixture);
    git(&fixture.root, &["rebase", "-q", "theirs"]);
    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    let operation = status.operation.clone().expect("rebase in progress");
    assert_eq!(operation.kind, GitOperationKind::Rebase);
    assert_eq!((operation.step, operation.total), (Some(1), Some(1)));
    assert!(status.head.detached);
    entry(&status, "README.md", GitChangeArea::Conflicted);
}

// MARK: - Diffs

#[tokio::test]
async fn a_text_diff_has_numbered_lines_and_a_no_newline_note() {
    let fixture = repository();
    fs::write(fixture.root.join("README.md"), "hello\nworld").unwrap();
    let key = status::hex(b"README.md");
    let diff = file_diff(fixture.path(), &key, GitChangeArea::Unstaged, None)
        .await
        .unwrap();
    assert_eq!(diff.kind, GitDiffKind::Text);
    assert_eq!(diff.total_hunks, 1);
    assert_eq!(diff.next_cursor, None);
    let lines = &diff.hunks[0].lines;
    assert_eq!(lines[0].kind, GitDiffLineKind::Context);
    assert_eq!((lines[0].old_line, lines[0].new_line), (Some(1), Some(1)));
    assert_eq!(lines[1].kind, GitDiffLineKind::Added);
    assert_eq!((lines[1].old_line, lines[1].new_line), (None, Some(2)));
    assert_eq!(lines[1].text, "world");
    assert!(lines.iter().any(|line| line.kind == GitDiffLineKind::Note));
}

#[tokio::test]
async fn binary_oversized_and_folder_entries_explain_themselves() {
    let fixture = repository();
    let root = &fixture.root;
    fs::write(root.join("image.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    fs::write(root.join("big.txt"), "x\n").unwrap();
    git(root, &["add", "image.bin", "big.txt"]);
    git(root, &["commit", "-q", "-m", "assets"]);
    fs::write(root.join("image.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
    let huge = "a fairly long line of text that repeats\n".repeat(diff::MAX_DIFF_BYTES / 30);
    fs::write(root.join("big.txt"), &huge).unwrap();
    fs::write(root.join("huge-untracked.txt"), &huge).unwrap();
    fs::create_dir(root.join("folder")).unwrap();
    fs::write(root.join("folder/a.txt"), "a\n").unwrap();

    let status = status(&home(&fixture), fixture.path(), &[]).await.unwrap();
    assert!(entry(&status, "image.bin", GitChangeArea::Unstaged).binary);
    let check = |name: &'static str, area: GitChangeArea, kind: GitDiffKind| {
        let path = fixture.path().to_owned();
        async move {
            let diff = file_diff(&path, &status::hex(name.as_bytes()), area, None)
                .await
                .unwrap();
            assert_eq!(diff.kind, kind, "{name}");
            assert!(diff.hunks.is_empty(), "{name}");
        }
    };
    check("image.bin", GitChangeArea::Unstaged, GitDiffKind::Binary).await;
    check("big.txt", GitChangeArea::Unstaged, GitDiffKind::TooLarge).await;
    check(
        "huge-untracked.txt",
        GitChangeArea::Untracked,
        GitDiffKind::TooLarge,
    )
    .await;
    check("folder/", GitChangeArea::Untracked, GitDiffKind::Directory).await;
}

#[tokio::test]
async fn pages_return_every_hunk_once_and_stay_under_the_phone_limit() {
    let fixture = repository();
    let original = (0..6000)
        .map(|line| format!("line {line:05} {}\n", "-".repeat(60)))
        .collect::<String>();
    fs::write(fixture.root.join("long.txt"), &original).unwrap();
    git(&fixture.root, &["add", "long.txt"]);
    git(&fixture.root, &["commit", "-q", "-m", "long"]);
    let changed = original
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index % 10 == 0 {
                format!("{line} changed\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect::<String>();
    fs::write(fixture.root.join("long.txt"), changed).unwrap();
    let key = status::hex(b"long.txt");

    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = file_diff(
            fixture.path(),
            &key,
            GitChangeArea::Unstaged,
            cursor.as_deref(),
        )
        .await
        .unwrap();
        let size = serde_json::to_vec(&page).unwrap().len();
        assert!(size < 512 * 1024, "page of {size} bytes");
        assert!(!page.hunks.is_empty());
        assert_eq!(page.additions, 600);
        seen.extend(page.hunks.iter().map(|hunk| hunk.index));
        pages += 1;
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                assert_eq!(seen.len() as u32, page.total_hunks);
                break;
            }
        }
    }
    assert!(pages > 1, "a diff this long spans pages");
    let expected = (0..seen.len() as u32).collect::<Vec<_>>();
    assert_eq!(seen, expected, "every hunk exactly once, in order");

    // A cursor from before the file changed is refused, not misapplied.
    let first = file_diff(fixture.path(), &key, GitChangeArea::Unstaged, None)
        .await
        .unwrap();
    fs::write(fixture.root.join("long.txt"), "rewritten\n").unwrap();
    assert_eq!(
        file_diff(
            fixture.path(),
            &key,
            GitChangeArea::Unstaged,
            first.next_cursor.as_deref()
        )
        .await,
        Err(GitError::StaleCursor)
    );
    assert_eq!(
        file_diff(
            fixture.path(),
            &key,
            GitChangeArea::Unstaged,
            Some("v1-forged-1")
        )
        .await,
        Err(GitError::StaleCursor)
    );
}

#[test]
fn one_enormous_hunk_is_cut_to_a_page_and_says_how_much_is_missing() {
    let mut output = b"@@ -0,0 +1,20000 @@\n".to_vec();
    for line in 0..20000 {
        output.extend_from_slice(format!("+{} {line}\n", "x".repeat(40)).as_bytes());
    }
    let parsed = diff::parse(&output);
    assert_eq!(parsed.additions, 20000);
    let (page, next) = diff::page(&parsed.hunks, 0);
    assert_eq!(next, None);
    assert_eq!(page.len(), 1);
    assert!(page[0].omitted_lines > 0);
    assert_eq!(page[0].lines.len() as u32 + page[0].omitted_lines, 20000);
    assert!(serde_json::to_vec(&page).unwrap().len() <= diff::DIFF_PAGE_BUDGET);

    let long = format!("@@ -1 +1 @@\n-{}\n+short\n", "y".repeat(100_000));
    let parsed = diff::parse(long.as_bytes());
    let line = &parsed.hunks[0].lines[0];
    assert!(line.truncated);
    assert!(line.text.len() <= diff::MAX_LINE_BYTES + '…'.len_utf8());
}

#[test]
fn a_removed_line_that_looks_like_a_header_stays_a_removed_line() {
    let output =
        b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,1 @@\n--- not a header\n+++ nor this\n";
    let parsed = diff::parse(output);
    let kinds = parsed.hunks[0]
        .lines
        .iter()
        .map(|line| line.kind)
        .collect::<Vec<_>>();
    assert_eq!(kinds, [GitDiffLineKind::Removed, GitDiffLineKind::Added]);
    assert_eq!(parsed.hunks[0].lines[0].text, "-- not a header");
}

#[tokio::test]
async fn only_files_the_status_lists_in_that_area_can_be_diffed() {
    let fixture = repository();
    let root = &fixture.root;
    // A file literally named `*`: a pathspec would match every file.
    fs::write(root.join("*"), "star\n").unwrap();
    fs::write(root.join("other.txt"), "other\n").unwrap();
    git(root, &["add", "--", ":(literal)*", "other.txt"]);
    git(root, &["commit", "-q", "-m", "star"]);
    fs::write(root.join("*"), "star\nchanged\n").unwrap();
    fs::write(root.join("other.txt"), "other\nchanged too\n").unwrap();

    let star = file_diff(
        fixture.path(),
        &status::hex(b"*"),
        GitChangeArea::Unstaged,
        None,
    )
    .await
    .unwrap();
    assert_eq!(star.additions, 1, "only the file named *, not every file");

    for (key, area, expected) in [
        (
            status::hex(b"README.md"),
            GitChangeArea::Unstaged,
            GitError::FileNotChanged,
        ),
        (
            status::hex(b"*"),
            GitChangeArea::Staged,
            GitError::FileNotChanged,
        ),
        (
            status::hex(b"../repo/*"),
            GitChangeArea::Unstaged,
            GitError::InvalidRequest,
        ),
        (
            status::hex(b"/etc/hosts"),
            GitChangeArea::Untracked,
            GitError::InvalidRequest,
        ),
        (
            "not-hex".to_owned(),
            GitChangeArea::Unstaged,
            GitError::InvalidRequest,
        ),
    ] {
        assert_eq!(
            file_diff(fixture.path(), &key, area, None).await,
            Err(expected),
            "{key}"
        );
    }
}

// MARK: - Recent list and sessions

fn session(
    id: &str,
    cwd: &Path,
    state: &str,
    idle_ms: Option<u64>,
    phase: Option<&str>,
) -> SessionSummary {
    SessionSummary {
        id: id.to_owned(),
        name: id.to_owned(),
        title: None,
        state: state.to_owned(),
        cwd: cwd.to_owned(),
        command_label: "zsh".to_owned(),
        created_at: "2026-10-13T00:00:00Z".to_owned(),
        last_activity_at: None,
        idle_ms,
        connector: phase.map(|_| "claude".to_owned()),
        agent_phase: phase.map(str::to_owned),
    }
}

#[tokio::test]
async fn sessions_in_the_worktree_are_reported_with_their_activity() {
    let fixture = repository();
    let root = &fixture.root;
    fs::create_dir(root.join("src")).unwrap();
    let elsewhere = fixture.base.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let linked = fixture.base.join("linked");
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            linked.to_str().unwrap(),
        ],
    );
    let sessions = [
        session("ses_agent", root, "running", Some(60_000), Some("working")),
        session(
            "ses_shell",
            &root.join("src"),
            "running",
            Some(120_000),
            None,
        ),
        session("ses_busy", root, "running", Some(200), None),
        session("ses_gone", root, "exited", Some(0), None),
        session("ses_other", &elsewhere, "running", Some(0), None),
        session("ses_linked", &linked, "running", Some(0), None),
    ];
    let opened = status(&home(&fixture), fixture.path(), &sessions)
        .await
        .unwrap();
    let found = opened
        .sessions
        .iter()
        .map(|s| (s.id.as_str(), s.active))
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            ("ses_agent", true),
            ("ses_shell", false),
            ("ses_busy", true)
        ]
    );

    let list = repositories(&home(&fixture), &sessions).await;
    let roots = list
        .from_sessions
        .iter()
        .map(|repo| (repo.name.as_str(), repo.sessions.len()))
        .collect::<Vec<_>>();
    assert_eq!(roots, [("repo", 3), ("linked", 1)]);
    // Opened above, but already listed under sessions, so not repeated.
    assert!(list.recent.is_empty());
    assert!(list.from_sessions[0].last_opened_at.is_some());
}

#[tokio::test]
async fn the_mac_remembers_opened_repositories_newest_first() {
    let fixture = repository();
    let home = home(&fixture);
    let second = fixture.base.join("second");
    fs::create_dir(&second).unwrap();
    git(&second, &["init", "-q", "-b", "main"]);
    let gone = fixture.base.join("gone");
    fs::create_dir(&gone).unwrap();
    git(&gone, &["init", "-q", "-b", "main"]);

    status(&home, gone.to_str().unwrap(), &[]).await.unwrap();
    status(&home, fixture.path(), &[]).await.unwrap();
    status(&home, second.to_str().unwrap(), &[]).await.unwrap();
    // Opening from a subfolder records the root, once.
    fs::create_dir(fixture.root.join("sub")).unwrap();
    status(&home, fixture.root.join("sub").to_str().unwrap(), &[])
        .await
        .unwrap();
    fs::remove_dir_all(&gone).unwrap();

    let list = repositories(&home, &[]).await;
    let names = list
        .recent
        .iter()
        .map(|repo| repo.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["repo", "second"],
        "newest first; a deleted one drops out"
    );
    assert!(list.recent.iter().all(|repo| repo.last_opened_at.is_some()));
    // The deleted repository stays recorded in case its volume comes back.
    assert_eq!(recent::load(&home).len(), 3);

    let file = home.root().join("git/recent-repositories.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn the_recent_list_is_bounded() {
    let temp = outside_any_repository();
    let home = LatchHome::new(temp.path().join("home"));
    for index in 0..(recent::RECENT_LIMIT + 5) {
        recent::record(&home, &format!("/repo/{index}"), SystemTime::now()).unwrap();
    }
    let list = recent::load(&home);
    assert_eq!(list.len(), recent::RECENT_LIMIT);
    assert_eq!(list[0].root, format!("/repo/{}", recent::RECENT_LIMIT + 4));
}

// MARK: - Process discipline

#[tokio::test]
async fn git_runs_noninteractively_and_is_killed_at_its_deadline() {
    let fixture = repository();
    // A shell alias stands in for a Git that hangs: a hook, a credential
    // helper, a filesystem that stopped answering.
    let started = std::time::Instant::now();
    let hung = command::run_with_timeout(
        &fixture.root,
        &args(["-c", "alias.hang=!sleep 30", "hang"]),
        1024,
        std::time::Duration::from_millis(300),
    )
    .await;
    assert_eq!(hung.unwrap_err(), GitError::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    let env = command::run(&fixture.root, &args(["-c", "alias.e=!env", "e"]), 64 * 1024)
        .await
        .unwrap();
    let env = String::from_utf8_lossy(&env.stdout);
    for pinned in [
        "GIT_TERMINAL_PROMPT=0",
        "GIT_LITERAL_PATHSPECS=1",
        "GIT_OPTIONAL_LOCKS=0",
        "GIT_ASKPASS=/usr/bin/false",
    ] {
        assert!(env.lines().any(|line| line == pinned), "{pinned} missing");
    }

    // Output past the bound is cut, and the call still returns.
    let big = command::run(&fixture.root, &args(["-c", "alias.y=!yes", "y"]), 4096)
        .await
        .unwrap();
    assert!(big.truncated);
    assert_eq!(big.stdout.len(), 4096);
}
