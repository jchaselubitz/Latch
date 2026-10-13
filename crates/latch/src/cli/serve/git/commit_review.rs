//! Commit the reviewed index, including when normal Mac hooks stage files.
//! Hooks run against a private index copy. Fixed forwarding hooks check its
//! tree after every pre-commit/message hook and refuse additional staging.
//! Hook edits to working files remain on disk for the next review.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::command::{self, args, Extras};
use super::{GitError, Worktree};

#[derive(Debug)]
pub(crate) struct Review {
    pub directory: PathBuf,
    pub index: PathBuf,
}

impl Drop for Review {
    fn drop(&mut self) {
        // This uniquely named private folder was created by this request.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) async fn prepare(worktree: &Worktree) -> Result<Arc<Review>, GitError> {
    let directory = worktree.git_dir.join(format!(
        "latch-commit-review-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| GitError::Failed)?;
    let review = Arc::new(Review {
        index: directory.join("index"),
        directory,
    });
    let original_index = worktree.git_dir.join("index");
    if original_index.exists() {
        fs::copy(original_index, &review.index).map_err(|_| GitError::Failed)?;
        fs::set_permissions(&review.index, fs::Permissions::from_mode(0o600))
            .map_err(|_| GitError::Failed)?;
    }
    let extras = Extras {
        index_file: Some(review.index.clone()),
        ..Extras::default()
    };
    let tree = command::run_with_extras(
        &worktree.root,
        &args(["write-tree"]),
        None,
        128,
        command::GIT_TIMEOUT,
        extras,
    )
    .await?;
    if !tree.succeeded() || tree.truncated {
        return Err(GitError::Failed);
    }
    let tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();
    if !(40..=64).contains(&tree.len()) || !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GitError::Failed);
    }
    let hooks = command::run(
        &worktree.root,
        &args(["rev-parse", "--path-format=absolute", "--git-path", "hooks"]),
        8192,
    )
    .await?;
    if !hooks.succeeded() || hooks.truncated {
        return Err(GitError::Failed);
    }
    let hook_path = std::str::from_utf8(&hooks.stdout)
        .map_err(|_| GitError::InvalidPath)?
        .trim_end_matches('\n');
    let hooks = PathBuf::from(hook_path);
    let hooks = if hooks.is_absolute() {
        hooks
    } else {
        worktree.root.join(hooks)
    };
    let protected = ["pre-commit", "prepare-commit-msg", "commit-msg"];
    let target = review.directory.join("hooks");
    fs::create_dir(&target).map_err(|_| GitError::Failed)?;
    // Keep reference-transaction, post-commit and other configured hooks.
    if let Ok(entries) = fs::read_dir(&hooks) {
        for entry in entries {
            let entry = entry.map_err(|_| GitError::Failed)?;
            if !protected.iter().any(|name| entry.file_name() == *name) {
                std::os::unix::fs::symlink(entry.path(), target.join(entry.file_name()))
                    .map_err(|_| GitError::Failed)?;
            }
        }
    }
    let git = command::git_executable()
        .to_str()
        .ok_or(GitError::InvalidPath)?;
    for name in protected {
        let original = hooks.join(name);
        let original = original.to_str().ok_or(GitError::InvalidPath)?;
        // Every word here is fixed or a Mac-derived path/hex tree quoted
        // literally. No phone argument or commit message becomes shell text.
        let script = format!("#!/bin/sh\nif [ -x {hook} ]; then\n  {hook} \"$@\"\n  result=$?\n  [ \"$result\" -eq 0 ] || exit \"$result\"\nfi\nactual=$({git} --no-pager -c core.fsmonitor=false write-tree) || exit $?\nif [ \"$actual\" != {tree} ]; then\n  printf '%s\\n' '[Latch] commit hook changed the reviewed index; review the files on your Mac.' >&2\n  exit 1\nfi\n",
            hook=quote(original), git=quote(git), tree=quote(&tree));
        let path = target.join(name);
        fs::write(&path, script).map_err(|_| GitError::Failed)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| GitError::Failed)?;
    }
    Ok(review)
}
