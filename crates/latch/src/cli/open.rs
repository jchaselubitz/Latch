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
use crate::session::paths::LatchHome;

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

#[cfg(any(target_os = "macos", test))]
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

/// Wraps a value as a single POSIX shell word.
///
/// The escape for an embedded single quote is `'"'"'` — close the quoted run,
/// emit one double-quoted apostrophe, reopen. It has to be exactly that, with
/// no backslashes: `open` quotes the session id, then quotes the whole attach
/// command again, so any error here is applied twice and reaches `latch attach`
/// as extra characters in the session id rather than as a syntax error.
#[cfg(any(target_os = "macos", test))]
fn shell_quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    use std::os::unix::ffi::OsStrExt;

    let text = String::from_utf8_lossy(value.as_ref().as_bytes());
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{host_attach_command, shell_quote};

    #[test]
    fn shell_quote_preserves_spaces_and_single_quotes() {
        assert_eq!(
            shell_quote("/Applications/Latch's App/latch"),
            "'/Applications/Latch'\"'\"'s App/latch'"
        );
    }

    /// Splits a command string into words the way a POSIX shell does.
    ///
    /// The construction bug this file guards against produced a command whose
    /// *substrings* all looked right while its *words* were wrong, so the tests
    /// have to tokenize rather than match text.
    fn split_shell_words(input: &str) -> Vec<String> {
        #[derive(PartialEq)]
        enum Quote {
            None,
            Single,
            Double,
        }

        let mut words = Vec::new();
        let mut current = String::new();
        let mut started = false;
        let mut quote = Quote::None;
        let mut escaped = false;

        for character in input.chars() {
            if escaped {
                current.push(character);
                started = true;
                escaped = false;
                continue;
            }
            match (&quote, character) {
                (Quote::None, '\\') | (Quote::Double, '\\') => escaped = true,
                (Quote::None, '\'') => {
                    quote = Quote::Single;
                    started = true;
                }
                (Quote::Single, '\'') => quote = Quote::None,
                (Quote::None, '"') => {
                    quote = Quote::Double;
                    started = true;
                }
                (Quote::Double, '"') => quote = Quote::None,
                (Quote::None, character) if character.is_ascii_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                (_, character) => {
                    current.push(character);
                    started = true;
                }
            }
        }
        assert!(quote == Quote::None, "unbalanced quotes in `{input}`");
        assert!(!escaped, "trailing backslash in `{input}`");
        if started {
            words.push(current);
        }
        words
    }

    #[test]
    fn the_session_id_survives_iterm_and_the_login_shell_unaltered() {
        // Two rounds of parsing stand between this string and `latch attach`:
        // iTerm splits the AppleScript `command` into argv, then the login
        // shell parses argv[2] as a script. A quoting error in either round
        // reaches `latch attach` as a corrupted session id, which resolves to
        // no session and exits — an immediately-closing window, not an error
        // anyone can read.
        let argv = split_shell_words(&host_attach_command("/bin/zsh", "ses_01JTEST"));

        assert_eq!(argv.len(), 3, "iTerm should see exactly `shell -lc script`");
        assert_eq!(argv[0], "/bin/zsh");
        assert_eq!(argv[1], "-lc");
        assert_eq!(
            split_shell_words(&argv[2]),
            ["exec", "latch", "attach", "ses_01JTEST"]
        );
    }

    #[test]
    fn a_shell_path_with_a_space_and_an_apostrophe_stays_one_word() {
        let argv = split_shell_words(&host_attach_command("/opt/jake's tools/zsh", "ses_01JTEST"));

        assert_eq!(argv[0], "/opt/jake's tools/zsh");
        assert_eq!(
            split_shell_words(&argv[2]),
            ["exec", "latch", "attach", "ses_01JTEST"]
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
