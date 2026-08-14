//! Open a local viewer attached to an existing session.
//!
//! Viewer opening is deliberately separate from session creation: a successful
//! worker spawn is the durable launch boundary, while a GUI viewer is optional
//! presentation that may be closed and reopened without affecting the process.

use anyhow::bail;
use anyhow::Context;

use crate::cli::json::OpenReport;
use crate::cli::manage;
use crate::session::paths::LatchHome;

/// The config key holding the default viewer-opening shape.
pub const OPEN_BEHAVIOR_KEY: &str = "open.behavior";

/// How the viewer should present the attachment.
///
/// The caller may state this per invocation; otherwise it comes from
/// `open.behavior` in `~/.latch/config.toml`. Overlord and Latch Desktop both
/// hold their own window-or-tab preference, and neither could reach this code
/// path while the AppleScript was fixed at `create window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenBehavior {
    /// A new viewer window.
    #[default]
    NewWindow,
    /// A new tab in the viewer's current window, falling back to a window when
    /// the viewer has none open.
    NewTab,
}

impl OpenBehavior {
    /// The canonical wire and config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewWindow => "new-window",
            Self::NewTab => "new-tab",
        }
    }

    /// Parses a caller- or config-supplied spelling.
    ///
    /// Both the bare noun and the `new-` form are accepted because the flag
    /// reads as `--as tab` while the stored key reads as `new-tab`.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "window" | "new-window" => Ok(Self::NewWindow),
            "tab" | "new-tab" => Ok(Self::NewTab),
            other => bail!("unsupported open behavior `{other}`; expected `window` or `tab`"),
        }
    }
}

/// A request to open one session in a local terminal viewer.
#[derive(Debug, Clone)]
pub struct OpenRequest {
    /// Where sessions live for this invocation.
    pub home: LatchHome,
    /// Existing session id or display name.
    pub session: String,
    /// Viewer identifier. The initial local integration supports `iterm`.
    pub viewer: String,
    /// Caller-stated shape; `None` defers to the stored preference.
    pub behavior: Option<OpenBehavior>,
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
    let behavior = match request.behavior {
        Some(behavior) => behavior,
        None => configured_behavior(&request.home)?,
    };
    let id = manage::resolve_existing(&request.home, &request.session)?;

    open_iterm(id.as_str(), behavior)?;
    Ok(OpenReport {
        id: id.to_string(),
        viewer,
        opened: true,
        behavior: behavior.as_str().to_owned(),
    })
}

/// Reads the stored default. A malformed value is an error rather than a silent
/// fallback, so a typo in the config surfaces where it can be fixed.
fn configured_behavior(home: &LatchHome) -> anyhow::Result<OpenBehavior> {
    match manage::config_value(home, OPEN_BEHAVIOR_KEY)? {
        Some(value) => OpenBehavior::parse(&value)
            .with_context(|| format!("`{OPEN_BEHAVIOR_KEY}` in {}", home.config_file().display())),
        None => Ok(OpenBehavior::default()),
    }
}

/// The AppleScript run for each shape.
///
/// iTerm can create a tab directly, but only in a window that exists; a first
/// launch has none, so the tab script opens a window in that case rather than
/// failing after the session is already running.
#[cfg(any(target_os = "macos", test))]
fn iterm_script(behavior: OpenBehavior) -> &'static str {
    match behavior {
        OpenBehavior::NewWindow => {
            "on run argv\n\
             tell application \"iTerm\"\n\
             activate\n\
             create window with default profile command (item 1 of argv)\n\
             end tell\n\
             end run"
        }
        OpenBehavior::NewTab => {
            "on run argv\n\
             tell application \"iTerm\"\n\
             activate\n\
             if (count of windows) is 0 then\n\
             create window with default profile command (item 1 of argv)\n\
             else\n\
             tell current window to create tab with default profile command (item 1 of argv)\n\
             end if\n\
             end tell\n\
             end run"
        }
    }
}

#[cfg(target_os = "macos")]
fn open_iterm(session_id: &str, behavior: OpenBehavior) -> anyhow::Result<()> {
    use std::process::Command;

    let shell = std::env::var_os("SHELL")
        .filter(|value| std::path::Path::new(value).is_absolute())
        .unwrap_or_else(|| "/bin/zsh".into());
    let command = host_attach_command(shell, session_id);
    let output = Command::new("osascript")
        .arg("-e")
        .arg(iterm_script(behavior))
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
fn open_iterm(_session_id: &str, _behavior: OpenBehavior) -> anyhow::Result<()> {
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
    use super::{host_attach_command, iterm_script, shell_quote, OpenBehavior};

    #[test]
    fn open_behavior_accepts_both_the_flag_and_config_spellings() {
        for value in ["window", "new-window", "New-Window", " new_window "] {
            assert_eq!(OpenBehavior::parse(value).unwrap(), OpenBehavior::NewWindow);
        }
        for value in ["tab", "new-tab", "NEW_TAB"] {
            assert_eq!(OpenBehavior::parse(value).unwrap(), OpenBehavior::NewTab);
        }
        assert_eq!(OpenBehavior::default(), OpenBehavior::NewWindow);
        assert_eq!(OpenBehavior::NewTab.as_str(), "new-tab");
        assert_eq!(
            OpenBehavior::parse(OpenBehavior::NewTab.as_str()).unwrap(),
            OpenBehavior::NewTab
        );
    }

    #[test]
    fn an_unknown_open_behavior_is_rejected_rather_than_silently_defaulted() {
        let error = OpenBehavior::parse("split").expect_err("`split` is not a shape");
        assert!(error.to_string().contains("split"), "{error}");
    }

    /// The tab script has to survive a first launch with no iTerm window, which
    /// `create tab` alone cannot: it targets `current window`.
    #[test]
    fn the_tab_script_still_opens_a_window_when_iterm_has_none() {
        let script = iterm_script(OpenBehavior::NewTab);

        assert!(
            script.contains("if (count of windows) is 0 then"),
            "{script}"
        );
        assert!(
            script.contains("tell current window to create tab with default profile command"),
            "{script}"
        );
        assert!(
            script.contains("create window with default profile command"),
            "{script}"
        );
    }

    #[test]
    fn the_window_script_never_creates_a_tab() {
        let script = iterm_script(OpenBehavior::NewWindow);

        assert!(!script.contains("create tab"), "{script}");
        assert!(
            script.contains("create window with default profile command"),
            "{script}"
        );
    }

    /// Both scripts read the attach command from `argv`, never from interpolated
    /// AppleScript source; a session id must not be able to become script text.
    #[test]
    fn neither_script_interpolates_the_attach_command() {
        for behavior in [OpenBehavior::NewWindow, OpenBehavior::NewTab] {
            let script = iterm_script(behavior);
            assert!(script.starts_with("on run argv\n"), "{script}");
            assert!(script.contains("(item 1 of argv)"), "{script}");
        }
    }

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
        let error = super::open_iterm("ses_01JTEST", super::OpenBehavior::NewWindow)
            .expect_err("iTerm is macOS-only");
        assert!(error.to_string().contains("only supported on macOS"));
    }
}
