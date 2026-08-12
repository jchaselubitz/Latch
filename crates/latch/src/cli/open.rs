//! Open a local viewer attached to an existing session.
//!
//! Viewer opening is deliberately separate from session creation: a successful
//! worker spawn is the durable launch boundary, while a GUI viewer is optional
//! presentation that may be closed and reopened without affecting the process.

use anyhow::bail;
#[cfg(target_os = "macos")]
use anyhow::Context;

use crate::cli::json::OpenReport;
use crate::cli::manage;
use crate::worker::paths::LatchHome;

/// A request to open one session in a local terminal viewer.
#[derive(Debug, Clone)]
pub struct OpenRequest {
    /// Where sessions live for this invocation.
    pub home: LatchHome,
    /// Existing session id or display name.
    pub session: String,
    /// Viewer identifier. The initial local integration supports `iterm`.
    pub viewer: String,
}

/// Opens a terminal viewer attached to a session without changing that
/// session's lifecycle.
pub fn open(request: OpenRequest) -> anyhow::Result<OpenReport> {
    let viewer = request.viewer.to_ascii_lowercase();
    if viewer != "iterm" {
        bail!(
            "unsupported viewer `{}`; supported viewers: iterm",
            request.viewer
        );
    }
    let id = manage::resolve_existing(&request.home, &request.session)?;

    open_iterm(id.as_str())?;
    Ok(OpenReport {
        id: id.to_string(),
        viewer,
        opened: true,
    })
}

#[cfg(target_os = "macos")]
fn open_iterm(session_id: &str) -> anyhow::Result<()> {
    use std::process::Command;

    let shell = std::env::var_os("SHELL")
        .filter(|value| std::path::Path::new(value).is_absolute())
        .unwrap_or_else(|| "/bin/zsh".into());
    let command = host_attach_command(shell, session_id);
    let output = Command::new("osascript")
        .arg("-e")
        .arg("on run argv\ntell application \"iTerm\"\nactivate\ncreate window with default profile command (item 1 of argv)\nend tell\nend run")
        // Passing the command as an argv item avoids interpolating session ids
        // or executable paths into AppleScript source.
        .arg(&command)
        .output()
        .context("cannot invoke osascript to open iTerm")?;
    if !output.status.success() {
        bail!(
            "iTerm did not open the Latch session: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn host_attach_command(shell: impl AsRef<std::ffi::OsStr>, session_id: &str) -> String {
    let attach = format!("exec latch attach {}", shell_quote(session_id));
    // iTerm's AppleScript `command` replaces the profile's login command; it
    // is not itself a shell script. Put the real executable first so iTerm's
    // launcher can exec it directly. The login shell interprets the inner
    // command, where `exec` is useful because it leaves `latch attach` as the
    // terminal's foreground process.
    format!("{} -lc {}", shell_quote(shell), shell_quote(attach))
}

#[cfg(not(target_os = "macos"))]
fn open_iterm(_session_id: &str) -> anyhow::Result<()> {
    bail!("opening iTerm is only supported on macOS")
}

fn shell_quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    use std::os::unix::ffi::OsStrExt;

    let text = String::from_utf8_lossy(value.as_ref().as_bytes());
    format!("'{}'", text.replace('\'', "'\\\"'\\\"'"))
}

#[cfg(test)]
mod tests {
    use super::{host_attach_command, shell_quote};

    #[test]
    fn shell_quote_preserves_spaces_and_single_quotes() {
        assert_eq!(
            shell_quote("/Applications/Latch's App/latch"),
            "'/Applications/Latch'\\\"'\\\"'s App/latch'"
        );
    }

    #[test]
    fn iterm_attach_resolves_latch_from_the_host_login_shell() {
        let command = host_attach_command("/bin/zsh", "ses_01JTEST");

        assert!(command.starts_with("'/bin/zsh' -lc "));
        assert!(!command.starts_with("exec "));
        assert!(command.contains("exec latch attach"));
        assert!(!command.contains(std::env::current_exe().unwrap().to_string_lossy().as_ref()));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_builds_report_that_iterm_cannot_be_opened() {
        let error = super::open_iterm("ses_01JTEST").expect_err("iTerm is macOS-only");
        assert!(error.to_string().contains("only supported on macOS"));
    }
}
