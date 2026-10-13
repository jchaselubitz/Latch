//! Resolving a folder to the Git working tree that contains it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::command::{self, args};
use super::GitError;
use crate::cli::serve::contract::{GitRepository, GitWorktreeKind};
use crate::cli::serve::directory::{self, BrowseError};

/// One resolved working tree. Every Git route builds this afresh from the
/// phone's folder, so a repository that moved, was deleted, or became bare
/// since the phone last looked is refused rather than acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Worktree {
    /// Canonical top level of the working tree, valid UTF-8.
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub kind: GitWorktreeKind,
}

impl Worktree {
    pub fn root_str(&self) -> &str {
        // `resolve` refuses a root that is not UTF-8.
        self.root.to_str().unwrap_or_default()
    }

    pub fn name(&self) -> String {
        display_name(&self.root)
    }

    pub fn repository(&self) -> GitRepository {
        GitRepository {
            root: self.root_str().to_owned(),
            name: self.name(),
            kind: self.kind,
        }
    }
}

pub(crate) fn display_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// Resolves a caller-supplied absolute folder to its working tree.
pub(crate) async fn resolve(path: &str) -> Result<Worktree, GitError> {
    let folder = directory::canonical_directory_from_str(path).map_err(|error| match error {
        BrowseError::UnavailablePath => GitError::UnavailablePath,
        BrowseError::UnreadableDirectory => GitError::UnreadableDirectory,
        _ => GitError::InvalidPath,
    })?;
    resolve_canonical(&folder).await
}

/// Resolves an already-canonical directory.
pub(crate) async fn resolve_canonical(folder: &Path) -> Result<Worktree, GitError> {
    // First the questions Git can answer anywhere inside a repository, so a
    // bare repository is named as one instead of failing as "not a work tree".
    let probe = command::run(
        folder,
        &args(["rev-parse", "--is-bare-repository", "--is-inside-work-tree"]),
        4096,
    )
    .await?;
    if !probe.succeeded() {
        return Err(refusal(&probe.stderr_text()));
    }
    let answers = String::from_utf8_lossy(&probe.stdout);
    let mut answers = answers.lines();
    if answers.next() == Some("true") {
        return Err(GitError::BareRepository);
    }
    if answers.next() != Some("true") {
        // Inside `.git` itself: a repository, but not a working tree.
        return Err(GitError::NotARepository);
    }

    let located = command::run(
        folder,
        &args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
            "--show-superproject-working-tree",
        ]),
        4 * directory::MAX_PATH_BYTES + 64,
    )
    .await?;
    if !located.succeeded() || located.truncated {
        return Err(refusal(&located.stderr_text()));
    }
    // Each answer is one line; a path containing a newline would make this
    // ambiguous, so the count must be exact.
    let text = located
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(&located.stdout);
    let lines = text.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if !(3..=4).contains(&lines.len()) {
        return Err(GitError::InvalidPath);
    }
    let path_of = |bytes: &[u8]| {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    };
    let root = fs::canonicalize(path_of(lines[0])).map_err(|_| GitError::UnavailablePath)?;
    if root.to_str().is_none() || root.as_os_str().len() > directory::MAX_PATH_BYTES {
        return Err(GitError::InvalidPath);
    }
    let git_dir = fs::canonicalize(path_of(lines[1])).map_err(|_| GitError::UnavailablePath)?;
    let common_dir = fs::canonicalize(path_of(lines[2])).map_err(|_| GitError::UnavailablePath)?;
    let superproject = lines.get(3).is_some_and(|line| !line.is_empty());
    let kind = if superproject {
        GitWorktreeKind::Submodule
    } else if git_dir != common_dir {
        GitWorktreeKind::LinkedWorktree
    } else {
        GitWorktreeKind::Repository
    };
    Ok(Worktree {
        root,
        git_dir,
        common_dir,
        kind,
    })
}

/// What Git's refusal to treat a folder as a repository means to the phone.
fn refusal(stderr: &str) -> GitError {
    if stderr.contains("dubious ownership") {
        GitError::UnsafeRepository
    } else {
        GitError::NotARepository
    }
}
