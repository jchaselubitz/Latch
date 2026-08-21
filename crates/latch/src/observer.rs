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
const OBSERVER_VERSION: u32 = 1;
const CODEX_SOURCE_ENV: &str = "LATCH_CODEX_CONVERSATION_SOURCE";

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
    if crate::session::meta::harness_kind(&manifest.launch.argv) != Some("claude") {
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

/// Captures one bounded Claude hook payload in the hosted session's raw sidecar.
pub fn capture_claude_hook(home: &LatchHome, reader: impl Read) -> anyhow::Result<()> {
    capture_hook(home, reader, "claude")
}

/// Captures a Codex hook/sidecar payload.  Integrations invoke this hidden
/// command when Codex reports a source binding or incremental transcript
/// record; the connector never searches a working directory for it.
pub fn capture_codex_hook(home: &LatchHome, reader: impl Read) -> anyhow::Result<()> {
    capture_hook(home, reader, "codex")
}

/// Persists the optional source path supplied by the launching integration.
/// The environment value is private launch material and is deliberately not
/// copied into display metadata. Relative paths are rejected: a binding must
/// name the agent's exact source, not be interpreted against an arbitrary cwd.
pub fn record_launch_source_binding(
    paths: &SessionPaths,
    manifest: &LaunchManifest,
) -> anyhow::Result<()> {
    if crate::session::meta::harness_kind(&manifest.launch.argv) != Some("codex") {
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

fn capture_hook(home: &LatchHome, reader: impl Read, connector: &str) -> anyhow::Result<()> {
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
    let command = format!("{} __conversation-hook", shell_quote(&executable));
    let plugin = serde_json::to_vec_pretty(&serde_json::json!({
        "name": CLAUDE_PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Captures raw Claude source bindings and observations for Latch."
    }))?;
    let hook =
        serde_json::json!({"matcher": ".*", "hooks": [{"type": "command", "command": command}]});
    let hooks = serde_json::to_vec_pretty(&serde_json::json!({
        "hooks": {
            "SessionStart": [hook.clone()],
            "PermissionRequest": [hook],
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
            },
            display: DisplayMetadata::default(),
        };
        assert!(record_launch_source_binding(&paths, &manifest).is_err());
    }
}
