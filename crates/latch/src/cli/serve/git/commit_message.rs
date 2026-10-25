//! Read-only staged context for the commit-message endpoint.
//!
//! No provider calls, receipts, mutation guards or hooks belong here. The
//! private index is dropped before returning; a caller must revalidate before
//! disclosure AND after the network call, then return the original identity.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::command::{self, args, Extras};
use super::{snapshot, status, worktree, GitError, Worktree};
use crate::cli::serve::contract::{
    GitDraftCoverage, GitDraftExpected, GitDraftFailureCode, GitDraftOmission,
    GitDraftOmissionReason, GitDraftSource, GitPreconditions,
};

const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILES: usize = 128;
const MANIFEST_BYTES: usize = 16_000;
const PATCH_BYTES: usize = 12_000;
const FILE_READ_BYTES: usize = 256 * 1024;

/// Deliberately not Debug: source and prompt content must not reach logs.
pub(crate) struct Context {
    pub source: GitDraftSource,
    pub coverage: GitDraftCoverage,
    /// The ONLY payload intended for the provider. No host root or branch.
    pub cloud_context: String,
    worktree: Worktree,
}

impl Context {
    /// Use immediately before disclosure and again before accepting a draft.
    /// This takes no repository mutation lock and does not refresh identity.
    pub async fn revalidate(&self) -> Result<(), GitDraftFailureCode> {
        let current = worktree::resolve(self.worktree.root_str())
            .await
            .map_err(failure)?;
        if current != self.worktree {
            return Err(GitDraftFailureCode::StaleSource);
        }
        let expected = GitDraftExpected {
            branch: self.source.branch.clone(),
            head: self.source.head.clone(),
            index: self.source.index.clone(),
        };
        check(&current, &expected).await?;
        if checksum(&read_index(&current)?) != self.source.captured_index {
            return Err(GitDraftFailureCode::StaleSource);
        }
        Ok(())
    }
}

pub(crate) async fn collect(
    path: &str,
    expected: &GitDraftExpected,
) -> Result<Context, GitDraftFailureCode> {
    // A bounded number of bounded Git calls, plus an overall capture deadline.
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        capture(path, expected, || {}),
    )
    .await
    .map_err(|_| GitDraftFailureCode::TimedOut)?
}

async fn capture(
    path: &str,
    expected: &GitDraftExpected,
    after_copy: impl FnOnce(),
) -> Result<Context, GitDraftFailureCode> {
    if expected.index.len() != 64
        || !expected.index.bytes().all(|b| b.is_ascii_hexdigit())
        || expected.head.as_ref().is_some_and(|oid| {
            !(40..=64).contains(&oid.len()) || !oid.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(GitDraftFailureCode::InvalidRequest);
    }
    let worktree = worktree::resolve(path).await.map_err(failure)?;
    check(&worktree, expected).await?;
    let bytes = read_index(&worktree)?;
    if bytes.is_empty() {
        return Err(GitDraftFailureCode::NoStagedChanges);
    }
    let captured_index = checksum(&bytes);
    let private = CapturedIndex::new(&worktree, &bytes)?;
    // Exercise an external writer at this boundary in deterministic fixtures.
    after_copy();
    // Match the captured copy to the SAME status preconditions used by commit.
    // Reading against the private index also detects malformed/unmerged data.
    let captured = observe(&worktree, Some(&private.index)).await?;
    matches(expected, &captured)?;
    if checksum(&read_index(&worktree)?) != captured_index {
        return Err(GitDraftFailureCode::StaleSource);
    }
    check(&worktree, expected).await?;

    // Pin even an unborn base explicitly. hash-object without -w computes
    // the repository-format empty-tree id without writing an object.
    let base = match &expected.head {
        Some(head) => head.clone(),
        None => {
            let output = command::run_with_extras(
                &worktree.root,
                &args(["hash-object", "-t", "tree", "--stdin"]),
                Some(Vec::new()),
                128,
                command::GIT_TIMEOUT,
                Extras::default(),
            )
            .await
            .map_err(failure)?;
            let oid = String::from_utf8(output.stdout.clone())
                .map_err(|_| GitDraftFailureCode::UnusableContext)?;
            let oid = oid.trim();
            if !output.succeeded()
                || output.truncated
                || !(40..=64).contains(&oid.len())
                || !oid.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(GitDraftFailureCode::UnusableContext);
            }
            oid.to_owned()
        }
    };
    let mut words = diff_args(&base, "--raw");
    words.extend(args(["--no-abbrev", "-z"]));
    let raw = run(&worktree, &private.index, &words, MANIFEST_BYTES * 4).await?;
    if raw.truncated {
        return Err(GitDraftFailureCode::ContextTooLarge);
    }
    if !raw.succeeded() {
        return Err(GitDraftFailureCode::UnusableContext);
    }
    let files = parse_manifest(&raw.stdout)?;
    if files.is_empty() {
        return Err(GitDraftFailureCode::NoStagedChanges);
    }
    let total_files = files.len();
    let mut safe = Vec::new();
    let mut omissions = Vec::new();
    for file in files {
        let paths = std::iter::once(&file.path).chain(file.old_path.iter());
        if paths.clone().any(|p| sensitive_path(p)) {
            omissions.push(omission(None, GitDraftOmissionReason::SensitivePath));
        } else if paths.clone().any(|p| credentials(p)) {
            omissions.push(omission(None, GitDraftOmissionReason::Credential));
        } else if paths.clone().any(|p| p.chars().any(char::is_control)) {
            omissions.push(omission(None, GitDraftOmissionReason::InvalidText));
        } else {
            safe.push(file);
        }
    }
    if safe.is_empty() {
        return Err(GitDraftFailureCode::UnusableContext);
    }
    let manifest =
        serde_json::to_string(&safe).map_err(|_| GitDraftFailureCode::UnusableContext)?;
    if manifest.len() > MANIFEST_BYTES {
        return Err(GitDraftFailureCode::ContextTooLarge);
    }
    // Reserve an equal share for EVERY file, so one huge early file cannot
    // consume all patch coverage. Unused shares intentionally stay unused.
    let share = PATCH_BYTES / safe.len();
    let mut patches = Vec::new();
    let mut included = Vec::new();
    let mut patch_bytes = 0;
    for file in safe {
        if file.old_mode == "160000" || file.new_mode == "160000" {
            omissions.push(omission(
                Some(&file.path),
                GitDraftOmissionReason::Submodule,
            ));
            included.push(file);
            continue;
        }
        let mut words = diff_args(&base, "--patch");
        words.extend(args([
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--line-prefix=",
            "--",
        ]));
        if let Some(old) = &file.old_path {
            words.push(old.into());
        }
        words.push(file.path.clone().into());
        let output = run(&worktree, &private.index, &words, FILE_READ_BYTES).await?;
        if !output.succeeded() && !output.truncated {
            return Err(GitDraftFailureCode::UnusableContext);
        }
        // Never transmit a prefix that has not been scanned in full. Oversize
        // files contribute safe metadata only, rather than an unscanned tail.
        if output.truncated {
            omissions.push(omission(
                Some(&file.path),
                GitDraftOmissionReason::PatchLimit,
            ));
            included.push(file);
            continue;
        }
        let Ok(text) = std::str::from_utf8(&output.stdout) else {
            omissions.push(omission(
                Some(&file.path),
                GitDraftOmissionReason::InvalidText,
            ));
            included.push(file);
            continue;
        };
        if credentials(text) {
            // Drop even the metadata for credential-bearing files. The cloud
            // gets only the anonymous reason and count, never their path.
            omissions.push(omission(None, GitDraftOmissionReason::Credential));
            continue;
        }
        if text.lines().any(|line| line.starts_with("Binary files ")) {
            omissions.push(omission(Some(&file.path), GitDraftOmissionReason::Binary));
        } else {
            let text = text.find("\n@@").map(|at| &text[at + 1..]).unwrap_or("");
            let end = utf8_end(text, share);
            if end < text.len() {
                omissions.push(omission(
                    Some(&file.path),
                    GitDraftOmissionReason::PatchLimit,
                ));
            }
            if end > 0 {
                patches.push(Patch {
                    path: file.path.clone(),
                    text: text[..end].to_owned(),
                });
                patch_bytes += end;
            }
        }
        included.push(file);
    }
    if included.is_empty() {
        return Err(GitDraftFailureCode::UnusableContext);
    }
    let coverage = GitDraftCoverage {
        total_files: total_files as u64,
        included_files: included.len() as u64,
        patch_files: patches.len() as u64,
        patch_bytes: patch_bytes as u64,
        omissions,
    };
    // JSON escaping may expand the byte count, but each input and the total
    // are bounded independently. Source paths are relative Git paths only.
    let cloud_context = serde_json::to_string(&serde_json::json!({
        "files": included, "patches": patches, "coverage": coverage,
    }))
    .map_err(|_| GitDraftFailureCode::UnusableContext)?;
    if cloud_context.len() > 64 * 1024 {
        return Err(GitDraftFailureCode::ContextTooLarge);
    }
    let context = Context {
        source: GitDraftSource {
            root: worktree.root_str().to_owned(),
            branch: expected.branch.clone(),
            head: expected.head.clone(),
            index: expected.index.clone(),
            captured_index,
        },
        coverage,
        cloud_context,
        worktree,
    };
    context.revalidate().await?;
    // RAII removes the index on success, refusal, timeout or cancellation.
    drop(private);
    Ok(context)
}

fn diff_args(base: &str, format: &str) -> Vec<std::ffi::OsString> {
    let mut words = args([
        "diff",
        "--cached",
        format,
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-relative",
        "--ignore-submodules=dirty",
        "--submodule=short",
        "-M",
    ]);
    words.push(base.into());
    words
}

async fn run(
    worktree: &Worktree,
    index: &Path,
    words: &[std::ffi::OsString],
    limit: usize,
) -> Result<command::GitOutput, GitDraftFailureCode> {
    command::run_with_extras(
        &worktree.root,
        words,
        None,
        limit,
        command::GIT_TIMEOUT,
        Extras {
            index_file: Some(index.to_owned()),
            ..Extras::default()
        },
    )
    .await
    .map_err(failure)
}

async fn observe(
    worktree: &Worktree,
    index: Option<&Path>,
) -> Result<GitPreconditions, GitDraftFailureCode> {
    let words = args([
        "status",
        "--porcelain=v2",
        "-z",
        "--branch",
        "--untracked-files=normal",
        "--find-renames",
    ]);
    let out = match index {
        Some(index) => run(worktree, index, &words, super::STATUS_READ_LIMIT).await?,
        None => command::run(&worktree.root, &words, super::STATUS_READ_LIMIT)
            .await
            .map_err(failure)?,
    };
    if out.truncated {
        return Err(GitDraftFailureCode::ContextTooLarge);
    }
    if !out.succeeded() {
        return Err(GitDraftFailureCode::UnusableContext);
    }
    let porcelain = status::parse(&out.stdout, true);
    let operation = status::operation(&worktree.git_dir);
    Ok(snapshot::take(&worktree.root, &porcelain, operation.as_ref()).preconditions)
}

async fn check(
    worktree: &Worktree,
    expected: &GitDraftExpected,
) -> Result<(), GitDraftFailureCode> {
    matches(expected, &observe(worktree, None).await?)
}

fn matches(
    expected: &GitDraftExpected,
    current: &GitPreconditions,
) -> Result<(), GitDraftFailureCode> {
    if expected.branch != current.branch
        || expected.head != current.head
        || expected.index != current.index
    {
        Err(GitDraftFailureCode::StaleSource)
    } else {
        Ok(())
    }
}

fn failure(error: GitError) -> GitDraftFailureCode {
    match error {
        GitError::TimedOut => GitDraftFailureCode::TimedOut,
        GitError::GitUnavailable => GitDraftFailureCode::GitUnavailable,
        _ => GitDraftFailureCode::UnusableContext,
    }
}

fn read_index(worktree: &Worktree) -> Result<Vec<u8>, GitDraftFailureCode> {
    let path = worktree.git_dir.join("index");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(GitDraftFailureCode::UnusableContext),
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(GitDraftFailureCode::UnusableContext);
    }
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| GitDraftFailureCode::UnusableContext)?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(GitDraftFailureCode::ContextTooLarge);
    }
    Ok(bytes)
}

fn checksum(bytes: &[u8]) -> String {
    status::hex(&Sha256::digest(bytes))
}

struct CapturedIndex {
    directory: PathBuf,
    index: PathBuf,
}
impl CapturedIndex {
    fn new(worktree: &Worktree, bytes: &[u8]) -> Result<Self, GitDraftFailureCode> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = worktree.git_dir.join(format!(
            "latch-draft-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| GitDraftFailureCode::UnusableContext)?;
        let capture = Self {
            index: directory.join("index"),
            directory,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&capture.index)
            .map_err(|_| GitDraftFailureCode::UnusableContext)?;
        file.write_all(bytes)
            .map_err(|_| GitDraftFailureCode::UnusableContext)?;
        Ok(capture)
    }
}
impl Drop for CapturedIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct File {
    path: String,
    old_path: Option<String>,
    change: String,
    old_mode: String,
    new_mode: String,
    old_oid: String,
    new_oid: String,
}
#[derive(Serialize)]
struct Patch {
    path: String,
    text: String,
}

fn parse_manifest(raw: &[u8]) -> Result<Vec<File>, GitDraftFailureCode> {
    let mut parts = raw.split(|b| *b == 0).peekable();
    let mut files = Vec::new();
    while let Some(header) = parts.next() {
        if header.is_empty() && parts.peek().is_none() {
            break;
        }
        if files.len() >= MAX_FILES {
            return Err(GitDraftFailureCode::ContextTooLarge);
        }
        let header =
            std::str::from_utf8(header).map_err(|_| GitDraftFailureCode::UnusableContext)?;
        let fields = header
            .strip_prefix(':')
            .ok_or(GitDraftFailureCode::UnusableContext)?
            .split(' ')
            .collect::<Vec<_>>();
        if fields.len() != 5
            || !matches!(
                fields[4].as_bytes().first(),
                Some(b'A' | b'M' | b'D' | b'R' | b'C' | b'T')
            )
        {
            return Err(GitDraftFailureCode::UnusableContext);
        }
        let path = |bytes: Option<&[u8]>| -> Result<String, GitDraftFailureCode> {
            let text = std::str::from_utf8(bytes.ok_or(GitDraftFailureCode::UnusableContext)?)
                .map_err(|_| GitDraftFailureCode::UnusableContext)?;
            if text.is_empty()
                || Path::new(text).is_absolute()
                || Path::new(text)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(GitDraftFailureCode::UnusableContext);
            }
            Ok(text.to_owned())
        };
        let first = path(parts.next())?;
        let (old_path, path) = if fields[4].starts_with(['R', 'C']) {
            (Some(first), path(parts.next())?)
        } else {
            (None, first)
        };
        files.push(File {
            path,
            old_path,
            change: fields[4].to_owned(),
            old_mode: fields[0].to_owned(),
            new_mode: fields[1].to_owned(),
            old_oid: fields[2].to_owned(),
            new_oid: fields[3].to_owned(),
        });
    }
    Ok(files)
}

fn omission(path: Option<&str>, reason: GitDraftOmissionReason) -> GitDraftOmission {
    GitDraftOmission {
        path: path.map(str::to_owned),
        reason,
    }
}
fn utf8_end(text: &str, limit: usize) -> usize {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn sensitive_path(path: &str) -> bool {
    path.to_ascii_lowercase().split('/').any(|part| {
        part == ".env"
            || part.starts_with(".env.")
            || part.ends_with(".env")
            || [
                ".ssh",
                ".aws",
                ".gnupg",
                ".npmrc",
                ".pypirc",
                ".netrc",
                "credentials",
                "secrets",
                "id_rsa",
                "id_ed25519",
            ]
            .contains(&part)
            || [".pem", ".p12", ".pfx", ".key", ".keystore"]
                .iter()
                .any(|ext| part.ends_with(ext))
            || part.starts_with("credentials.")
            || part.starts_with("secrets.")
    })
}

/// Conservative, deliberately non-exhaustive defense in depth. Scan the FULL
/// bounded patch before slicing, including removed lines and context lines.
pub(crate) fn credentials(text: &str) -> bool {
    if [
        "PRIVATE KEY-----",
        "AKIA",
        "ASIA",
        "AIza",
        "ghp_",
        "github_pat_",
        "gho_",
        "ghs_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "eyJ",
    ]
    .iter()
    .any(|marker| text.contains(marker))
    {
        return true;
    }
    text.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        let assignment = lower.contains('=') || lower.contains(':');
        assignment
            && [
                "api_key",
                "apikey",
                "api-key",
                "access_key",
                "secret",
                "password",
                "passwd",
                "token",
                "authorization",
                "credential",
            ]
            .iter()
            .any(|key| lower.contains(key))
            || lower.contains("://") && lower.contains('@') && lower.matches(':').count() >= 2
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{git, repository};
    use super::*;

    async fn expected(path: &str) -> GitDraftExpected {
        let worktree = worktree::resolve(path).await.unwrap();
        let current = observe(&worktree, None).await.unwrap();
        GitDraftExpected {
            branch: current.branch,
            head: current.head,
            index: current.index,
        }
    }

    #[tokio::test]
    async fn draft_context_is_staged_only_in_a_linked_worktree_and_rejects_new_index() {
        let fixture = repository();
        let linked = fixture.base.join("linked");
        git(
            &fixture.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "draft",
                linked.to_str().unwrap(),
            ],
        );
        let path = linked.to_str().unwrap();
        fs::write(linked.join("README.md"), "hello\nstaged-marker\n").unwrap();
        git(&linked, &["add", "README.md"]);
        fs::write(
            linked.join("README.md"),
            "hello\nstaged-marker\nunstaged-marker\n",
        )
        .unwrap();
        fs::write(linked.join("untracked.txt"), "untracked-marker\n").unwrap();
        let reviewed = expected(path).await;
        let worktree = worktree::resolve(path).await.unwrap();
        let original = read_index(&worktree).unwrap();
        let context = collect(path, &reviewed).await.unwrap();
        assert!(context.cloud_context.contains("staged-marker"));
        assert!(!context.cloud_context.contains("unstaged-marker"));
        assert!(!context.cloud_context.contains("untracked"));
        assert!(!context.cloud_context.contains(path));
        assert_eq!(context.coverage.total_files, 1);
        assert_eq!(context.source.branch.as_deref(), Some("draft"));
        assert_eq!(original, read_index(&worktree).unwrap());
        assert!(!fs::read_dir(&worktree.git_dir).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("latch-draft-")));
        fs::write(linked.join("README.md"), "only-worktree-changed\n").unwrap();
        context.revalidate().await.unwrap();
        let private = CapturedIndex::new(&worktree, &original).unwrap();
        git(&linked, &["add", "README.md"]);
        let old_patch = run(
            &worktree,
            &private.index,
            &diff_args(reviewed.head.as_deref().unwrap(), "--patch"),
            FILE_READ_BYTES,
        )
        .await
        .unwrap();
        assert!(old_patch.succeeded());
        let old_patch = String::from_utf8(old_patch.stdout).unwrap();
        assert!(old_patch.contains("staged-marker"));
        assert!(!old_patch.contains("only-worktree-changed"));
        assert_eq!(
            context.revalidate().await,
            Err(GitDraftFailureCode::StaleSource)
        );
        assert!(matches!(
            collect(path, &reviewed).await,
            Err(GitDraftFailureCode::StaleSource)
        ));
    }

    #[tokio::test]
    async fn draft_context_supports_unborn_head_and_anonymizes_sensitive_omissions() {
        let fixture = repository();
        let root = fixture.base.join("unborn");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("safe.txt"), "safe-staged-marker\n").unwrap();
        fs::write(root.join(".env"), "PRIVATE_VALUE=never-send\n").unwrap();
        fs::write(root.join("config.txt"), "api_key = never-send-credential\n").unwrap();
        git(&root, &["add", "."]);
        let path = root.to_str().unwrap();
        let context = collect(path, &expected(path).await).await.unwrap();
        assert_eq!(context.source.head, None);
        assert_eq!(context.coverage.total_files, 3);
        assert_eq!(context.coverage.included_files, 1);
        assert!(context.cloud_context.contains("safe-staged-marker"));
        for absent in [".env", "config.txt", "never-send", "api_key"] {
            assert!(!context.cloud_context.contains(absent));
        }
        assert!(context.coverage.omissions.iter().all(|o| o.path.is_none()));
        git(&root, &["rm", "--cached", "safe.txt"]);
        assert!(matches!(
            collect(path, &expected(path).await).await,
            Err(GitDraftFailureCode::UnusableContext)
        ));
    }

    #[tokio::test]
    async fn draft_context_metadata_covers_rename_delete_mode_binary_and_submodule() {
        use std::os::unix::fs::PermissionsExt;
        let f = repository();
        fs::write(f.root.join("delete.txt"), "remove me\n").unwrap();
        fs::write(f.root.join("executable.sh"), "echo hello\n").unwrap();
        git(&f.root, &["add", "."]);
        git(&f.root, &["commit", "-qm", "base"]);
        git(&f.root, &["mv", "README.md", "renamed.md"]);
        git(&f.root, &["rm", "delete.txt"]);
        fs::set_permissions(
            f.root.join("executable.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(f.root.join("binary.dat"), [0, 1, 2, 3]).unwrap();
        git(&f.root, &["add", "."]);
        let oid = git(&f.root, &["rev-parse", "HEAD"]);
        git(
            &f.root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},module", oid.trim()),
            ],
        );
        let context = collect(f.path(), &expected(f.path()).await).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&context.cloud_context).unwrap();
        let files = value["files"].as_array().unwrap();
        assert!(files.iter().any(|v| v["path"] == "renamed.md"
            && v["oldPath"] == "README.md"
            && v["change"] == "R100"));
        assert!(files
            .iter()
            .any(|v| v["path"] == "delete.txt" && v["change"] == "D"));
        assert!(files
            .iter()
            .any(|v| v["path"] == "executable.sh" && v["newMode"] == "100755"));
        assert!(files
            .iter()
            .any(|v| v["path"] == "module" && v["newMode"] == "160000"));
        for reason in [
            GitDraftOmissionReason::Binary,
            GitDraftOmissionReason::Submodule,
        ] {
            assert!(context
                .coverage
                .omissions
                .iter()
                .any(|o| o.reason == reason));
        }
    }

    #[tokio::test]
    async fn draft_context_bounds_uneven_patches_and_refuses_large_manifests() {
        let f = repository();
        fs::write(
            f.root.join("a-large.txt"),
            "long line of harmless source data\n".repeat(2000),
        )
        .unwrap();
        fs::write(f.root.join("z-small.txt"), "small-file-marker\n").unwrap();
        git(&f.root, &["add", "."]);
        let c = collect(f.path(), &expected(f.path()).await).await.unwrap();
        assert!(c.cloud_context.contains("small-file-marker"));
        assert!(c.coverage.patch_bytes <= PATCH_BYTES as u64);
        assert!(c
            .coverage
            .omissions
            .iter()
            .any(|o| o.reason == GitDraftOmissionReason::PatchLimit));
        fs::write(
            f.root.join("a-large.txt"),
            "large harmless line\n".repeat(20000),
        )
        .unwrap();
        git(&f.root, &["add", "."]);
        let c = collect(f.path(), &expected(f.path()).await).await.unwrap();
        assert!(c.cloud_context.contains("small-file-marker"));
        assert_eq!(c.coverage.patch_files, 1);
        for i in 0..MAX_FILES {
            fs::write(f.root.join(format!("file-{i}")), "data\n").unwrap();
        }
        git(&f.root, &["add", "."]);
        assert!(matches!(
            collect(f.path(), &expected(f.path()).await).await,
            Err(GitDraftFailureCode::ContextTooLarge)
        ));
    }

    #[tokio::test]
    async fn draft_context_empty_intent_to_add_and_conflict_fail_closed() {
        let f = repository();
        assert!(matches!(
            collect(f.path(), &expected(f.path()).await).await,
            Err(GitDraftFailureCode::NoStagedChanges)
        ));
        fs::write(f.root.join("intent.txt"), "not staged\n").unwrap();
        git(&f.root, &["add", "-N", "intent.txt"]);
        assert!(matches!(
            collect(f.path(), &expected(f.path()).await).await,
            Err(GitDraftFailureCode::NoStagedChanges)
        ));
        let oid = git(&f.root, &["rev-parse", "HEAD:README.md"]);
        git(&f.root, &["update-index", "--force-remove", "README.md"]);
        let mut child = std::process::Command::new("git")
            .current_dir(&f.root)
            .args(["update-index", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.take().unwrap(),
            "100644 {} 1\tREADME.md\n100644 {} 2\tREADME.md",
            oid.trim(),
            oid.trim()
        )
        .unwrap();
        assert!(child.wait().unwrap().success());
        assert!(matches!(
            collect(f.path(), &expected(f.path()).await).await,
            Err(GitDraftFailureCode::UnusableContext)
        ));
    }

    #[tokio::test]
    async fn draft_context_runs_no_hooks_filters_or_external_diff_and_writes_no_git_objects() {
        use std::os::unix::fs::PermissionsExt;
        let f = repository();
        fs::write(f.root.join("README.md"), "safe change\n").unwrap();
        git(&f.root, &["add", "."]);
        let marker = f.base.join("executed");
        let script = f.base.join("hostile");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &f.root,
            &["config", "diff.external", script.to_str().unwrap()],
        );
        git(
            &f.root,
            &["config", "diff.hostile.textconv", script.to_str().unwrap()],
        );
        git(
            &f.root,
            &["config", "core.fsmonitor", script.to_str().unwrap()],
        );
        fs::write(f.root.join(".git/info/attributes"), "* diff=hostile\n").unwrap();
        fs::copy(&script, f.root.join(".git/hooks/pre-commit")).unwrap();
        fn tree(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
            let mut result = Vec::new();
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    result.extend(tree(&path));
                } else {
                    result.push((path.clone(), fs::read(path).unwrap()));
                }
            }
            result.sort();
            result
        }
        let reviewed = expected(f.path()).await;
        let before = tree(&f.root.join(".git"));
        let c = collect(f.path(), &reviewed).await.unwrap();
        c.revalidate().await.unwrap();
        assert_eq!(before, tree(&f.root.join(".git")));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn draft_context_split_index_and_sha256_are_supported() {
        let f = repository();
        git(&f.root, &["update-index", "--split-index"]);
        fs::write(f.root.join("README.md"), "split-index-marker\n").unwrap();
        git(&f.root, &["add", "."]);
        let c = collect(f.path(), &expected(f.path()).await).await.unwrap();
        assert!(c.cloud_context.contains("split-index-marker"));
        let root = f.base.join("sha256");
        fs::create_dir(&root).unwrap();
        git(
            &root,
            &["init", "-q", "--object-format=sha256", "-b", "main"],
        );
        fs::write(root.join("safe.txt"), "sha256-marker\n").unwrap();
        git(&root, &["add", "."]);
        let path = root.to_str().unwrap();
        assert!(collect(path, &expected(path).await)
            .await
            .unwrap()
            .cloud_context
            .contains("sha256-marker"));
    }

    #[tokio::test]
    async fn draft_context_rejects_branch_and_head_changes() {
        let f = repository();
        fs::write(f.root.join("README.md"), "staged change\n").unwrap();
        git(&f.root, &["add", "."]);
        let c = collect(f.path(), &expected(f.path()).await).await.unwrap();
        git(&f.root, &["checkout", "-qb", "other"]);
        assert_eq!(c.revalidate().await, Err(GitDraftFailureCode::StaleSource));
        let c = collect(f.path(), &expected(f.path()).await).await.unwrap();
        git(&f.root, &["commit", "--allow-empty", "-qm", "move HEAD"]);
        assert_eq!(c.revalidate().await, Err(GitDraftFailureCode::StaleSource));
    }

    #[tokio::test]
    async fn draft_context_detects_index_head_and_branch_races_during_capture() {
        for change in ["index", "head", "branch"] {
            let f = repository();
            fs::write(f.root.join("README.md"), "reviewed staged data\n").unwrap();
            git(&f.root, &["add", "."]);
            let reviewed = expected(f.path()).await;
            let result = capture(f.path(), &reviewed, || match change {
                "index" => {
                    fs::write(f.root.join("README.md"), "concurrent staged data\n").unwrap();
                    git(&f.root, &["add", "."]);
                }
                "head" => {
                    git(&f.root, &["commit", "-qm", "external commit"]);
                }
                _ => {
                    git(&f.root, &["checkout", "-qb", "external-branch"]);
                }
            })
            .await;
            assert!(
                matches!(result, Err(GitDraftFailureCode::StaleSource)),
                "{change}"
            );
            assert!(!fs::read_dir(f.root.join(".git")).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("latch-draft-")));
        }
    }

    #[tokio::test]
    async fn draft_context_reads_sparse_index_without_expanding_the_live_index() {
        let f = repository();
        for dir in ["included", "outside"] {
            fs::create_dir(f.root.join(dir)).unwrap();
            fs::write(f.root.join(dir).join("file.txt"), "base\n").unwrap();
        }
        git(&f.root, &["add", "."]);
        git(&f.root, &["commit", "-qm", "sparse base"]);
        git(
            &f.root,
            &["sparse-checkout", "init", "--cone", "--sparse-index"],
        );
        git(&f.root, &["sparse-checkout", "set", "included"]);
        fs::write(f.root.join("included/file.txt"), "sparse-staged-marker\n").unwrap();
        git(&f.root, &["add", "included/file.txt"]);
        let reviewed = expected(f.path()).await;
        let original = fs::read(f.root.join(".git/index")).unwrap();
        let context = collect(f.path(), &reviewed).await.unwrap();
        assert!(context.cloud_context.contains("sparse-staged-marker"));
        assert!(!context.cloud_context.contains("outside"));
        assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), original);
    }
}
