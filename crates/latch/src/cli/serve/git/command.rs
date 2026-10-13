//! The one way the gateway runs Git.
//!
//! Every invocation is a fixed argument array chosen here, run directly (no
//! shell) in an explicit working directory with an environment that cannot
//! prompt, page, open an editor, or follow a `GIT_DIR` inherited from whoever
//! started the gateway. Output is read to a bound and the process is killed at
//! a deadline, so a phone request can neither fill memory nor hang on Git.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::GitError;

/// No read-only Git call the phone can cause should take longer than this.
/// A status in a very large repository is the slowest; past this the phone
/// is told Git did not answer rather than left waiting.
pub(super) const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Git's own complaints are short; anything past this is not read.
const STDERR_LIMIT: u64 = 16 * 1024;

/// Options set on every invocation, ahead of the subcommand.
///
/// `core.fsmonitor=false` keeps a repository-configured monitor command from
/// running on a phone's read; `core.quotePath=false` keeps paths as bytes in
/// the few places `-z` does not; colour and pagers are off so output is data.
const GLOBAL_ARGS: &[&str] = &[
    "--no-pager",
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.quotePath=false",
    "-c",
    "color.ui=false",
    "-c",
    "credential.interactive=never",
];

#[derive(Debug)]
pub(super) struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Exit code, or `None` when Git was killed by a signal (including after
    /// its output was cut off at the limit).
    pub code: Option<i32>,
    /// Stdout exceeded `limit` and was cut there.
    pub truncated: bool,
}

impl GitOutput {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// The Git executable: the first `git` on the gateway's `PATH`, so a Mac
/// with Homebrew Git uses the same one its terminal does, then the system's.
pub(super) fn git_executable() -> &'static Path {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(|| {
        std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("git"))
                    .find(|candidate| is_executable(candidate))
            })
            .unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Runs `git <GLOBAL_ARGS> <args>` in `cwd`, reading at most `limit` bytes of
/// stdout. A non-zero exit is returned, not raised: callers decide what Git's
/// refusal means.
pub(super) async fn run(
    cwd: &Path,
    args: &[OsString],
    limit: usize,
) -> Result<GitOutput, GitError> {
    run_with_timeout(cwd, args, limit, GIT_TIMEOUT).await
}

pub(super) async fn run_with_timeout(
    cwd: &Path,
    args: &[OsString],
    limit: usize,
    timeout: Duration,
) -> Result<GitOutput, GitError> {
    run_with_input(cwd, args, None, limit, timeout).await
}

/// Receives each progress line Git writes to stderr (`\r` or `\n`
/// terminated), as text.
pub(crate) type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// How a long-running mutation is run, beyond the plain invocation.
#[derive(Clone, Default)]
pub(super) struct Extras {
    pub index_file: Option<PathBuf>,
    pub commit_review: Option<Arc<super::commit_review::Review>>,
    /// Accept the message Git prepared (Continue) instead of failing on the
    /// editor that `GIT_EDITOR=/usr/bin/false` makes unavailable.
    pub accept_default_message: bool,
    /// Called with every progress line.
    pub progress: Option<ProgressSink>,
    /// Called with Git's process id (also its process group) once started.
    pub on_spawn: Option<Arc<dyn Fn(u32) + Send + Sync>>,
}

impl std::fmt::Debug for Extras {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Extras")
            .field("accept_default_message", &self.accept_default_message)
            .field("progress", &self.progress.is_some())
            .field("on_spawn", &self.on_spawn.is_some())
            .field("commit_review", &self.commit_review.is_some())
            .finish()
    }
}

/// Like [`run_with_timeout`], with `input` written to Git's stdin (a NUL
/// separated pathspec list, never a script) and then closed.
///
/// Git runs as the leader of its own session (and so its own process group,
/// with no controlling terminal for `ssh` or a helper to prompt on), and the
/// whole group is killed at the deadline, so a hook or helper Git started
/// cannot outlive the request that caused it.
pub(super) async fn run_with_input(
    cwd: &Path,
    args: &[OsString],
    input: Option<Vec<u8>>,
    limit: usize,
    timeout: Duration,
) -> Result<GitOutput, GitError> {
    run_with_extras(cwd, args, input, limit, timeout, Extras::default()).await
}

pub(super) async fn run_with_extras(
    cwd: &Path,
    args: &[OsString],
    input: Option<Vec<u8>>,
    limit: usize,
    timeout: Duration,
    extras: Extras,
) -> Result<GitOutput, GitError> {
    let mut command = Command::new(git_executable());
    command
        .current_dir(cwd)
        .args(GLOBAL_ARGS)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(review) = &extras.commit_review {
        command.arg("-c").arg(format!(
            "core.hooksPath={}",
            review.directory.join("hooks").display()
        ));
    }
    command.args(args);
    // SAFETY: setsid is async-signal-safe and touches only the child. A
    // freshly forked child is never a group leader, so it cannot fail here.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    apply_environment(&mut command);
    if let Some(path) = &extras.index_file {
        command.env("GIT_INDEX_FILE", path);
    }
    // Keep the private index and hook forwarding files alive until Git
    // has finished, including timeout and disconnected-client paths.
    let _commit_review = extras.commit_review;
    if extras.accept_default_message {
        command.env("GIT_EDITOR", "/usr/bin/true");
    }
    let progress = extras.progress;

    let mut child = command.spawn().map_err(|_| GitError::GitUnavailable)?;
    if let (Some(on_spawn), Some(pid)) = (&extras.on_spawn, child.id()) {
        on_spawn(pid);
    }
    let group = ProcessGroup(child.id());
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or(GitError::GitUnavailable)?;
    let stderr = child.stderr.take().ok_or(GitError::GitUnavailable)?;
    let work = async move {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            use tokio::io::AsyncWriteExt;
            // Git may exit before reading everything (a refusal); that is
            // its answer, not a failure to deliver the list.
            let _ = stdin.write_all(&input).await;
        }
        // Each reader owns its pipe and drops it when its bound is reached,
        // so a Git writing past the limit gets EPIPE and exits rather than
        // blocking on a full pipe until the deadline.
        let read_stdout = async move {
            let mut buffer = Vec::new();
            stdout
                .take(limit as u64 + 1)
                .read_to_end(&mut buffer)
                .await
                .map(|_| buffer)
        };
        let read_stderr = async move {
            match progress {
                None => {
                    let mut buffer = Vec::new();
                    stderr
                        .take(STDERR_LIMIT)
                        .read_to_end(&mut buffer)
                        .await
                        .map(|_| buffer)
                }
                Some(sink) => read_progress(stderr, &sink).await,
            }
        };
        let (stdout, stderr) = tokio::join!(read_stdout, read_stderr);
        let mut stdout = stdout.map_err(|_| GitError::Failed)?;
        let stderr = stderr.unwrap_or_default();
        let truncated = stdout.len() > limit;
        if truncated {
            stdout.truncate(limit);
            let _ = child.start_kill();
        }
        let status = child.wait().await.map_err(|_| GitError::Failed)?;
        group.disarm();
        Ok(GitOutput {
            stdout,
            stderr,
            code: status.code(),
            truncated,
        })
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        // Dropping the future drops the child, and `kill_on_drop` ends it.
        Err(_) => Err(GitError::TimedOut),
    }
}

/// Reads stderr to its end, passing each progress line to `sink` and keeping
/// what a terminal would show: a line ended by `\r` is replaced by the next.
/// Only the last [`STDERR_LIMIT`] bytes are kept, because Git's refusal is
/// what it writes last.
async fn read_progress(
    mut stderr: tokio::process::ChildStderr,
    sink: &ProgressSink,
) -> std::io::Result<Vec<u8>> {
    let mut kept: Vec<u8> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stderr.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        for &byte in &chunk[..read] {
            match byte {
                b'\r' | b'\n' => {
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim();
                    if !text.is_empty() {
                        sink(text);
                    }
                    if byte == b'\n' {
                        kept.extend_from_slice(&line);
                        kept.push(b'\n');
                    }
                    line.clear();
                }
                _ if line.len() < 4096 => line.push(byte),
                _ => {}
            }
        }
        let limit = STDERR_LIMIT as usize;
        if kept.len() > limit * 2 {
            kept.drain(..kept.len() - limit);
        }
    }
    kept.extend_from_slice(&line);
    let limit = STDERR_LIMIT as usize;
    if kept.len() > limit {
        kept.drain(..kept.len() - limit);
    }
    Ok(kept)
}

/// The percentage in a progress line such as `Receiving objects:  45%
/// (9/20)`, if it has one.
pub(crate) fn percent(line: &str) -> Option<u8> {
    let at = line.find('%')?;
    let digits = line[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let value = digits
        .chars()
        .rev()
        .collect::<String>()
        .parse::<u8>()
        .ok()?;
    (value <= 100).then_some(value)
}

/// Kills a Git process group unless Git was reaped normally first.
struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: killpg only sends a signal; the group is the one this
            // gateway created for Git and has not yet reaped.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
}

/// Removes everything Git reads from the environment that could point it at
/// another repository or make it interactive, then pins the rest.
fn apply_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(&key);
        }
    }
    for key in ["SSH_ASKPASS", "PAGER", "EDITOR", "VISUAL"] {
        command.env_remove(key);
    }
    command
        // Git Credential Manager has its own switch for prompting.
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/usr/bin/false")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "/usr/bin/false")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Pathspecs are always literal: a file named `*.rs` or `:(top)` is
        // that file, never a pattern.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C");
    // Tests run Git as the gateway does, minus the developer's own global
    // and system configuration, so a signing key or hook path on the
    // machine running the tests cannot change what a fixture does.
    #[cfg(test)]
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

/// Builds an argument list from fixed words and caller-checked paths.
pub(super) fn args<I, S>(words: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    words
        .into_iter()
        .map(|word| word.as_ref().to_owned())
        .collect()
}
