//! Launch material accepted by `latch create`.
//!
//! The document is bounded and validated before a session is created. Secrets
//! are held in memory and passed to the child through a private FIFO.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Manifest schema version this build writes and accepts.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// Largest accepted launch manifest.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// A terminal size in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl TerminalSize {
    /// Creates a terminal size.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// What to run, where, and with what environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// Program and arguments. Never persisted by Latch.
    pub argv: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Environment entries to set.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whether the child inherits the launcher environment.
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    /// Initial terminal size.
    pub size: TerminalSize,
    /// Requested terminal type. Latch normalizes this to its pinned value.
    #[serde(default = "default_term")]
    pub term: String,
    /// Hosted agent this launch starts, declared by the launcher.
    ///
    /// When present, `argv` is the agent's own program and arguments —
    /// `argv[0]` is the agent executable, never a shell that runs it — so
    /// Latch can record the session's identity and prepare its conversation
    /// observer without interpreting a command string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// Starts `argv` through the owner's login shell rather than directly.
    ///
    /// Latch builds the wrapper itself, after it has prepared the agent argv,
    /// so the process still gets the PATH and setup a terminal would give it
    /// while `argv` stays structured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_shell: Option<LoginShell>,
}

/// Hosted agents Latch can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Claude Code.
    Claude,
    /// OpenAI Codex CLI.
    Codex,
}

impl AgentKind {
    /// The harness marker persisted for sessions running this agent, which is
    /// also the executable name `argv[0]` must carry.
    pub const fn harness(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// A login shell that starts the launch argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginShell {
    /// Absolute path of the shell.
    pub path: PathBuf,
    /// Launcher-authored shell text run before the program is exec'd, such
    /// as exports or setup commands. Latch never interprets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prelude: Option<String>,
}

impl LaunchSpec {
    /// Replaces a `login_shell` request with the argv that carries it out:
    /// `[shell, -ilc, "<prelude>\nexec \"$@\"", latch, argv...]`.
    ///
    /// The program and its arguments reach the shell as positional
    /// parameters, never as text, so nothing in `argv` is re-parsed. Called
    /// once, after identity and observer preparation have read `argv` and the
    /// session metadata has persisted the identity; the declaration is
    /// dropped with the wrap, because `argv[0]` is no longer the agent.
    pub fn apply_login_shell(&mut self) {
        let Some(shell) = self.login_shell.take() else {
            return;
        };
        self.agent = None;
        let script = match shell.prelude.as_deref().map(str::trim) {
            Some(prelude) if !prelude.is_empty() => format!("{prelude}\nexec \"$@\""),
            _ => "exec \"$@\"".to_owned(),
        };
        let program = std::mem::take(&mut self.argv);
        self.argv = [
            shell.path.display().to_string(),
            // Interactive as well as login, for the same reason a shell
            // session is: `.zshrc` is where PATH additions usually live.
            "-ilc".to_owned(),
            script,
            // `$0` for the script; the program is `$1`.
            "latch".to_owned(),
        ]
        .into_iter()
        .chain(program)
        .collect();
    }
}

/// Bounded metadata safe to persist and display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DisplayMetadata {
    /// Short display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Human title.
    #[serde(default)]
    pub title: Option<String>,
    /// Redacted command label.
    #[serde(default)]
    pub command_label: Option<String>,
    /// Session provenance.
    #[serde(default)]
    pub source: SourceInfo,
}

/// Session provenance supplied by an integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Broad source kind.
    pub kind: String,
    /// Opaque external correlation id.
    #[serde(default)]
    pub external_run_id: Option<String>,
}

impl Default for SourceInfo {
    fn default() -> Self {
        Self {
            kind: "cli".to_owned(),
            external_run_id: None,
        }
    }
}

/// Everything needed to launch a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchManifest {
    /// Schema version.
    pub format_version: u32,
    /// Child launch details.
    pub launch: LaunchSpec,
    /// Display-only metadata.
    #[serde(default)]
    pub display: DisplayMetadata,
}

/// Named arguments for constructing a manifest.
pub struct LaunchRequest {
    /// Program and arguments.
    pub argv: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Initial terminal size.
    pub size: TerminalSize,
}

impl LaunchManifest {
    /// Creates a manifest with safe defaults.
    pub fn new(request: LaunchRequest) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            launch: LaunchSpec {
                argv: request.argv,
                cwd: request.cwd,
                env: BTreeMap::new(),
                inherit_env: true,
                size: request.size,
                term: default_term(),
                agent: None,
                login_shell: None,
            },
            display: DisplayMetadata::default(),
        }
    }
}

/// Errors produced while reading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Input could not be read.
    #[error("cannot read launch manifest: {0}")]
    Io(#[from] std::io::Error),
    /// JSON was malformed.
    #[error("launch manifest is malformed: {detail}")]
    Malformed {
        /// Parser detail.
        detail: String,
    },
    /// Schema version is unsupported.
    #[error("launch manifest format version {found}; this build reads {supported}")]
    UnsupportedVersion {
        /// Version supplied.
        found: u32,
        /// Version accepted.
        supported: u32,
    },
    /// A field cannot be acted on.
    #[error("launch manifest field `{field}` is unusable: {detail}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Validation detail.
        detail: String,
    },
    /// Input exceeded the hard limit.
    #[error("launch manifest exceeds {limit} bytes")]
    TooLarge {
        /// Maximum accepted bytes.
        limit: usize,
    },
}

/// Reads and validates a bounded manifest.
pub fn read(reader: impl Read) -> Result<LaunchManifest, ManifestError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge {
            limit: MAX_MANIFEST_BYTES,
        });
    }
    let manifest = serde_json::from_slice(&bytes).map_err(|error| ManifestError::Malformed {
        detail: error.to_string(),
    })?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Writes a validated manifest.
pub fn write(writer: impl Write, manifest: &LaunchManifest) -> Result<(), ManifestError> {
    validate(manifest)?;
    serde_json::to_writer(writer, manifest).map_err(|error| ManifestError::Malformed {
        detail: error.to_string(),
    })
}

fn validate(manifest: &LaunchManifest) -> Result<(), ManifestError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(ManifestError::UnsupportedVersion {
            found: manifest.format_version,
            supported: MANIFEST_FORMAT_VERSION,
        });
    }
    if manifest.launch.argv.first().is_none_or(String::is_empty) {
        return Err(ManifestError::InvalidField {
            field: "launch.argv",
            detail: "must contain a program".to_owned(),
        });
    }
    if let Some(agent) = manifest.launch.agent {
        // The identity names the program that actually runs. A shell or
        // interpreter in argv[0] would get the agent's observer arguments and
        // leave the agent itself unobserved.
        let program = std::path::Path::new(&manifest.launch.argv[0])
            .file_name()
            .and_then(|name| name.to_str());
        if program != Some(agent.harness()) {
            return Err(ManifestError::InvalidField {
                field: "launch.agent",
                detail: format!(
                    "declares {} but launch.argv[0] is not the {} executable; \
                     pass the agent argv and use launch.login_shell for shell setup",
                    agent.harness(),
                    agent.harness()
                ),
            });
        }
    }
    if let Some(shell) = &manifest.launch.login_shell {
        if !shell.path.is_absolute() {
            return Err(ManifestError::InvalidField {
                field: "launch.login_shell.path",
                detail: "must be absolute".to_owned(),
            });
        }
    }
    if !manifest.launch.cwd.is_absolute() {
        return Err(ManifestError::InvalidField {
            field: "launch.cwd",
            detail: "must be absolute".to_owned(),
        });
    }
    if manifest.launch.size.cols == 0 || manifest.launch.size.rows == 0 {
        return Err(ManifestError::InvalidField {
            field: "launch.size",
            detail: "dimensions must be non-zero".to_owned(),
        });
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

fn default_term() -> String {
    crate::engine::DEFAULT_TERMINAL.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(argv: &[&str], agent: Option<AgentKind>) -> LaunchManifest {
        let mut manifest = LaunchManifest::new(LaunchRequest {
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
            cwd: PathBuf::from("/tmp"),
            size: TerminalSize::new(80, 24),
        });
        manifest.launch.agent = agent;
        manifest
    }

    fn reads(manifest: &LaunchManifest) -> Result<LaunchManifest, ManifestError> {
        read(serde_json::to_vec(manifest).unwrap().as_slice())
    }

    #[test]
    fn a_declared_agent_round_trips() {
        let mut declared = manifest(&["/opt/bin/codex"], Some(AgentKind::Codex));
        declared.launch.login_shell = Some(LoginShell {
            path: PathBuf::from("/bin/zsh"),
            prelude: None,
        });
        let json = serde_json::to_value(&declared).unwrap();
        assert_eq!(json["launch"]["agent"], "codex");
        assert_eq!(json["launch"]["login_shell"]["path"], "/bin/zsh");
        assert_eq!(reads(&declared).unwrap(), declared);
    }

    #[test]
    fn a_manifest_without_the_new_fields_still_reads() {
        let plain = manifest(&["/bin/zsh", "-il"], None);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json["launch"].get("agent").is_none());
        assert!(json["launch"].get("login_shell").is_none());
        assert_eq!(reads(&plain).unwrap(), plain);
    }

    #[test]
    fn a_declared_agent_must_be_the_program_that_runs() {
        let wrapped = manifest(&["/bin/zsh", "-lc", "claude"], Some(AgentKind::Claude));
        assert!(matches!(
            reads(&wrapped),
            Err(ManifestError::InvalidField {
                field: "launch.agent",
                ..
            })
        ));
        let mismatched = manifest(&["codex"], Some(AgentKind::Claude));
        assert!(reads(&mismatched).is_err());
    }

    #[test]
    fn a_login_shell_must_be_absolute() {
        let mut relative = manifest(&["claude"], Some(AgentKind::Claude));
        relative.launch.login_shell = Some(LoginShell {
            path: PathBuf::from("zsh"),
            prelude: None,
        });
        assert!(matches!(
            reads(&relative),
            Err(ManifestError::InvalidField {
                field: "launch.login_shell.path",
                ..
            })
        ));
    }

    #[test]
    fn a_login_shell_receives_the_program_as_arguments() {
        let mut launch = manifest(&["claude", "it's \"quoted\"; rm -rf /"], None).launch;
        launch.login_shell = Some(LoginShell {
            path: PathBuf::from("/bin/zsh"),
            prelude: Some("  ".to_owned()),
        });
        launch.apply_login_shell();
        assert_eq!(
            launch.argv,
            [
                "/bin/zsh",
                "-ilc",
                "exec \"$@\"",
                "latch",
                "claude",
                "it's \"quoted\"; rm -rf /"
            ]
        );
        // Applying twice is a no-op: the request was consumed.
        let once = launch.argv.clone();
        launch.apply_login_shell();
        assert_eq!(launch.argv, once);
    }
}
