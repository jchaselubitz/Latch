//! Raw, bounded agent hook capture used by future conversation connectors.
//!
//! This module deliberately performs no normalization and exposes no client
//! event contract. Agent-owned source data remains authoritative.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context};
use serde_json::Value;

use crate::engine;
use crate::session::manifest::LaunchManifest;
use crate::session::paths::{LatchHome, SessionId, SessionPaths, DIR_MODE, FILE_MODE};

const MAX_HOOK_BYTES: usize = 1024 * 1024;
const CLAUDE_PLUGIN_NAME: &str = "latch-conversation-observer";
const OBSERVER_VERSION: u32 = 2;
const CODEX_SOURCE_ENV: &str = "LATCH_CODEX_CONVERSATION_SOURCE";

/// The observer version that first registers a `Stop` hook. A session whose
/// Claude process was launched with an older plugin directory never emits
/// `Stop`, so the connector must not treat its silence as a turn boundary.
/// Compare a hook record's stamped `latch_observer_version` against this
/// constant rather than assuming the currently running `latch` binary's
/// compiled `OBSERVER_VERSION` describes every already-running session: the
/// binary can be upgraded in place while an older Claude process keeps
/// running against the plugin directory (and hook set) it was launched with.
pub const STOP_HOOK_MIN_OBSERVER_VERSION: u32 = 2;

struct PrivateWrite<'a> {
    path: &'a Path,
    contents: &'a [u8],
    mode: u32,
}

/// Injects Latch's private raw-source observer into a directly launched Claude process.
pub fn prepare_claude_launch(
    home: &LatchHome,
    manifest: &mut LaunchManifest,
) -> anyhow::Result<()> {
    if crate::session::meta::launch_harness(&manifest.launch) != Some("claude") {
        return Ok(());
    }
    let plugin = ensure_claude_plugin(home)?;
    if manifest
        .launch
        .argv
        .windows(2)
        .any(|pair| pair[0] == "--plugin-dir" && Path::new(&pair[1]) == plugin)
    {
        return Ok(());
    }
    manifest.launch.argv.splice(
        1..1,
        ["--plugin-dir".to_owned(), plugin.display().to_string()],
    );
    Ok(())
}

/// Install a session-scoped Codex SessionStart hook. Codex supplies the exact
/// transcript path and thread id to this hook; Latch never searches rollouts.
pub fn prepare_codex_launch(manifest: &mut LaunchManifest) -> anyhow::Result<()> {
    if crate::session::meta::launch_harness(&manifest.launch) != Some("codex") {
        return Ok(());
    }
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let command = format!("{} __codex-conversation-hook", shell_quote(&executable));
    let hook = format!(
        "hooks.SessionStart=[{{hooks=[{{type=\"command\",command={}}}]}}]",
        serde_json::to_string(&command)?
    );
    manifest.launch.argv.splice(
        1..1,
        [
            // Codex otherwise pauses at a terminal-only hook review prompt
            // before its first message. This flag also affects other hooks
            // enabled for this one Codex process; keep the observer command
            // fixed and scoped to this launch.
            "--dangerously-bypass-hook-trust".to_owned(),
            "-c".to_owned(),
            hook,
        ],
    );
    Ok(())
}

/// Captures one bounded Claude hook payload in the hosted session's raw
/// sidecar. `observer_version` is the version baked into the invoking hook
/// command at plugin-generation time (see `ensure_claude_plugin`), not the
/// running binary's compiled `OBSERVER_VERSION` — it tells the connector
/// which hook set this specific Claude launch actually has.
pub fn capture_claude_hook(
    home: &LatchHome,
    reader: impl Read,
    observer_version: u32,
) -> anyhow::Result<()> {
    capture_hook(home, reader, "claude", Some(observer_version))
}

/// Captures a Codex hook/sidecar payload.  Integrations invoke this hidden
/// command when Codex reports a source binding or incremental transcript
/// record; the connector never searches a working directory for it.
pub fn capture_codex_hook(home: &LatchHome, reader: impl Read) -> anyhow::Result<()> {
    capture_hook(home, reader, "codex", None)
}

/// Persists the optional source path supplied by the launching integration.
/// The environment value is private launch material and is deliberately not
/// copied into display metadata. Relative paths are rejected: a binding must
/// name the agent's exact source, not be interpreted against an arbitrary cwd.
pub fn record_launch_source_binding(
    paths: &SessionPaths,
    manifest: &LaunchManifest,
) -> anyhow::Result<()> {
    if crate::session::meta::launch_harness(&manifest.launch) != Some("codex") {
        return Ok(());
    }
    let Some(source) = manifest.launch.env.get(CODEX_SOURCE_ENV) else {
        return Ok(());
    };
    let source = PathBuf::from(source);
    if !source.is_absolute() {
        bail!("{CODEX_SOURCE_ENV} must be an absolute agent-supplied source path");
    }
    write_private(PrivateWrite {
        path: &paths.conversation_source_binding(),
        contents: &serde_json::to_vec(&serde_json::json!({
            "connector": "codex",
            "source": source,
        }))?,
        mode: FILE_MODE,
    })
}

fn capture_hook(
    home: &LatchHome,
    reader: impl Read,
    connector: &str,
    observer_version: Option<u32>,
) -> anyhow::Result<()> {
    let raw = read_bounded_hook(reader)?;
    let latch_id = std::env::var(crate::session::paths::SESSION_ID_ENV)
        .context("conversation hook did not inherit LATCH_SESSION_ID")?;
    let id = SessionId::parse(&latch_id)?;
    let mut record: Value =
        serde_json::from_slice(&raw).context("conversation hook payload is not JSON")?;
    let object = record
        .as_object_mut()
        .context("conversation hook payload must be an object")?;
    object
        .entry("timestamp")
        .or_insert_with(|| Value::String(engine::format_rfc3339(SystemTime::now())));
    object
        .entry("connector")
        .or_insert_with(|| Value::String(connector.to_owned()));
    if let Some(observer_version) = observer_version {
        object.insert(
            "latch_observer_version".to_owned(),
            Value::from(observer_version),
        );
    }
    let paths = home.session(&id);
    if !paths.meta().is_file() {
        bail!("conversation hook belongs to unknown Latch session {id}");
    }
    if let Some(source) = binding_from_record(object) {
        let agent_session_id = object
            .get("session_id")
            .or_else(|| object.get("sessionId"))
            .and_then(Value::as_str);
        write_private(PrivateWrite {
            path: &paths.conversation_source_binding(),
            contents: &serde_json::to_vec(&serde_json::json!({
                "connector": connector,
                "source": source,
                "agentSessionId": agent_session_id,
            }))?,
            mode: FILE_MODE,
        })?;
    }
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(paths.conversation_source_hooks())
        .context("cannot append the conversation source sidecar")?;
    lock_exclusive(&file)?;
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

fn binding_from_record(object: &serde_json::Map<String, Value>) -> Option<PathBuf> {
    [
        "transcript_path",
        "thread_path",
        "rollout_path",
        "source_path",
    ]
    .into_iter()
    .find_map(|key| object.get(key).and_then(Value::as_str))
    .map(PathBuf::from)
    .filter(|path| path.is_absolute())
}

fn ensure_claude_plugin(home: &LatchHome) -> anyhow::Result<PathBuf> {
    let root = home
        .root()
        .join("observers")
        .join(format!("{CLAUDE_PLUGIN_NAME}-v{OBSERVER_VERSION}"));
    let metadata_dir = root.join(".claude-plugin");
    let hooks_dir = root.join("hooks");
    for directory in [&root, &metadata_dir, &hooks_dir] {
        fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        fs::set_permissions(directory, fs::Permissions::from_mode(DIR_MODE))?;
    }
    let executable =
        fs::canonicalize(std::env::current_exe().context("cannot locate the latch executable")?)?;
    // The version is baked into this specific plugin directory's command line
    // at generation time. It stays immutable even if the `latch` binary at
    // this path is later upgraded in place, so a hook fired by an
    // already-running Claude process still truthfully reports which hook set
    // (and thus which turn-boundary guarantees) that launch actually has.
    let command = format!(
        "{} __conversation-hook --observer-version {OBSERVER_VERSION}",
        shell_quote(&executable)
    );
    let plugin = serde_json::to_vec_pretty(&serde_json::json!({
        "name": CLAUDE_PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Captures raw Claude source bindings, permission requests, and turn completions for Latch."
    }))?;
    let hook =
        serde_json::json!({"matcher": ".*", "hooks": [{"type": "command", "command": command}]});
    let hooks = serde_json::to_vec_pretty(&serde_json::json!({
        "hooks": {
            "SessionStart": [hook.clone()],
            "PermissionRequest": [hook.clone()],
            "Stop": [hook],
        }
    }))?;
    write_private(PrivateWrite {
        path: &metadata_dir.join("plugin.json"),
        contents: &plugin,
        mode: FILE_MODE,
    })?;
    write_private(PrivateWrite {
        path: &hooks_dir.join("hooks.json"),
        contents: &hooks,
        mode: FILE_MODE,
    })?;
    Ok(root)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

fn write_private(request: PrivateWrite<'_>) -> anyhow::Result<()> {
    if fs::read(request.path).ok().as_deref() == Some(request.contents) {
        fs::set_permissions(request.path, fs::Permissions::from_mode(request.mode))?;
        return Ok(());
    }
    let parent = request
        .path
        .parent()
        .context("private file has no parent")?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".tmp-{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(request.mode)
        .open(&temp)
        .with_context(|| format!("cannot write {}", temp.display()))?;
    file.write_all(request.contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, request.path)?;
    fs::set_permissions(request.path, fs::Permissions::from_mode(request.mode))?;
    Ok(())
}

fn read_bounded_hook(reader: impl Read) -> anyhow::Result<Vec<u8>> {
    let mut raw = Vec::new();
    reader
        .take((MAX_HOOK_BYTES + 1) as u64)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_HOOK_BYTES {
        bail!("Claude hook payload exceeds {MAX_HOOK_BYTES} bytes");
    }
    Ok(raw)
}

fn lock_exclusive(file: &fs::File) -> anyhow::Result<()> {
    // SAFETY: `file` remains alive for the write guarded by this advisory lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("cannot lock the Claude source sidecar")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::session::manifest::{DisplayMetadata, LaunchSpec, TerminalSize};

    #[test]
    fn codex_launch_binding_is_explicit_and_absolute() {
        let temp = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(temp.path().join("session"));
        paths.ensure().unwrap();
        let mut env = BTreeMap::new();
        env.insert(
            CODEX_SOURCE_ENV.to_owned(),
            "/private/codex/session.jsonl".to_owned(),
        );
        let manifest = LaunchManifest {
            format_version: 1,
            launch: LaunchSpec {
                argv: vec!["codex".to_owned()],
                cwd: PathBuf::from("/private/workspace"),
                env,
                inherit_env: true,
                size: TerminalSize::new(80, 24),
                term: "xterm-256color".to_owned(),
                agent: None,
                login_shell: None,
            },
            display: DisplayMetadata::default(),
        };
        record_launch_source_binding(&paths, &manifest).unwrap();
        let binding: Value =
            serde_json::from_slice(&fs::read(paths.conversation_source_binding()).unwrap())
                .unwrap();
        assert_eq!(binding["connector"], "codex");
        assert_eq!(binding["source"], "/private/codex/session.jsonl");
    }

    #[test]
    fn codex_launch_installs_a_session_start_hook_on_the_agent_argv() {
        let mut manifest = LaunchManifest {
            format_version: 1,
            launch: LaunchSpec {
                argv: vec!["codex".to_owned()],
                cwd: PathBuf::from("/private/workspace"),
                env: BTreeMap::new(),
                inherit_env: true,
                size: TerminalSize::new(80, 24),
                term: "xterm-256color".to_owned(),
                agent: Some(crate::session::manifest::AgentKind::Codex),
                login_shell: None,
            },
            display: DisplayMetadata::default(),
        };
        prepare_codex_launch(&mut manifest).unwrap();
        assert_eq!(manifest.launch.argv[0], "codex");
        assert_eq!(manifest.launch.argv[1], "--dangerously-bypass-hook-trust");
        assert_eq!(manifest.launch.argv[2], "-c");
        assert!(manifest.launch.argv[3].contains("hooks.SessionStart="));
        assert!(manifest.launch.argv[3].contains("__codex-conversation-hook"));
    }

    /// A declared agent started through the owner's login shell still gets
    /// its observer on the agent argv, and the shell only ever receives that
    /// argv as positional parameters.
    #[test]
    fn a_declared_claude_launch_is_observed_through_its_login_shell() {
        let temp = tempfile::tempdir().unwrap();
        let home = LatchHome::new(temp.path());
        let mut manifest = LaunchManifest {
            format_version: 1,
            launch: LaunchSpec {
                argv: vec!["claude".to_owned(), "--model".to_owned(), "opus".to_owned()],
                cwd: PathBuf::from("/private/workspace"),
                env: BTreeMap::new(),
                inherit_env: true,
                size: TerminalSize::new(80, 24),
                term: "xterm-256color".to_owned(),
                agent: Some(crate::session::manifest::AgentKind::Claude),
                login_shell: Some(crate::session::manifest::LoginShell {
                    path: PathBuf::from("/bin/zsh"),
                    prelude: Some("export TASK=1".to_owned()),
                }),
            },
            display: DisplayMetadata::default(),
        };
        prepare_claude_launch(&home, &mut manifest).unwrap();
        let plugin = ensure_claude_plugin(&home).unwrap();
        manifest.launch.apply_login_shell();
        assert_eq!(
            manifest.launch.argv,
            vec![
                "/bin/zsh".to_owned(),
                "-ilc".to_owned(),
                "export TASK=1\nexec \"$@\"".to_owned(),
                "latch".to_owned(),
                "claude".to_owned(),
                "--plugin-dir".to_owned(),
                plugin.display().to_string(),
                "--model".to_owned(),
                "opus".to_owned(),
            ]
        );
        assert_eq!(manifest.launch.login_shell, None);
        assert_eq!(
            manifest.launch.agent, None,
            "the wrapped argv must still validate"
        );
        crate::session::manifest::write(Vec::new(), &manifest).unwrap();
    }

    #[test]
    fn claude_plugin_registers_a_versioned_stop_hook() {
        let temp = tempfile::tempdir().unwrap();
        let home = LatchHome::new(temp.path());
        let plugin = ensure_claude_plugin(&home).unwrap();
        assert!(
            plugin
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(&format!("-v{OBSERVER_VERSION}")),
            "the plugin directory name is stamped with the observer version so an \
             upgrade never rewrites hooks an already-running Claude process relies on"
        );
        let hooks: Value =
            serde_json::from_slice(&fs::read(plugin.join("hooks").join("hooks.json")).unwrap())
                .unwrap();
        for event in ["SessionStart", "PermissionRequest", "Stop"] {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook is registered"));
            assert!(
                command.contains(&format!("--observer-version {OBSERVER_VERSION}")),
                "{event} command does not bake in the observer version: {command}"
            );
        }
        assert_eq!(OBSERVER_VERSION, STOP_HOOK_MIN_OBSERVER_VERSION);
    }

    #[test]
    fn codex_launch_binding_rejects_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(temp.path().join("session"));
        paths.ensure().unwrap();
        let manifest = LaunchManifest {
            format_version: 1,
            launch: LaunchSpec {
                argv: vec!["codex".to_owned()],
                cwd: PathBuf::from("/private/workspace"),
                env: [(CODEX_SOURCE_ENV.to_owned(), "session.jsonl".to_owned())]
                    .into_iter()
                    .collect(),
                inherit_env: true,
                size: TerminalSize::new(80, 24),
                term: "xterm-256color".to_owned(),
                agent: None,
                login_shell: None,
            },
            display: DisplayMetadata::default(),
        };
        assert!(record_launch_source_binding(&paths, &manifest).is_err());
    }
}
