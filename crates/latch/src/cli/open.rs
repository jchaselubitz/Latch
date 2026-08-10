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

    let latch = std::env::current_exe().context("cannot locate the latch executable")?;
    let command = format!(
        "exec {} attach {}",
        shell_quote(latch.as_os_str()),
        shell_quote(session_id)
    );
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

#[cfg(not(target_os = "macos"))]
fn open_iterm(_session_id: &str) -> anyhow::Result<()> {
    bail!("opening iTerm is only supported on macOS")
}

#[cfg(target_os = "macos")]
fn shell_quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    use std::os::unix::ffi::OsStrExt;

    let text = String::from_utf8_lossy(value.as_ref().as_bytes());
    format!("'{}'", text.replace('\'', "'\\\"'\\\"'"))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::shell_quote;

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_quote_preserves_spaces_and_single_quotes() {
        assert_eq!(
            shell_quote("/Applications/Latch's App/latch"),
            "'/Applications/Latch'\\\"'\\\"'s App/latch'"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_builds_report_that_iterm_cannot_be_opened() {
        let error = super::open_iterm("ses_01JTEST").expect_err("iTerm is macOS-only");
        assert!(error.to_string().contains("only supported on macOS"));
    }
}
