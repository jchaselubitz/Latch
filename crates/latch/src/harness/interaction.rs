//! Capability-gated input for hosted harness sessions.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::time::SystemTime;

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;

use super::permission;
use super::{CanSend, InteractionCapabilities};
use crate::cli::manage;
use crate::engine::{self, SessionState};
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId, SessionPaths, FILE_MODE};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const UNKNOWN_HARNESS_REASON: &str =
    "this session is not a known Claude Code harness; use --keys to send input";

/// Session-scoped interaction query.
pub struct InteractionOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or unique display name.
    pub session: String,
}

/// One operation accepted by `latch send`.
#[derive(Debug)]
pub enum SendAction {
    /// Paste a message and submit it.
    Message(String),
    /// Send explicit tmux key names.
    Keys(String),
    /// Answer the currently visible request.
    Resolve {
        /// Stable request identifier from `awaiting_input`.
        request_id: String,
        /// One advertised choice.
        choice: String,
    },
}

/// Named arguments for one input operation.
pub struct SendOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or unique display name.
    pub session: String,
    /// Requested operation.
    pub action: SendAction,
}

/// Capability or screen-state refusal from `latch send`.
///
/// The gateway maps this to HTTP 409 `{ error: "refused", reason }` so a
/// client can surface the reason instead of treating it as a crash.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{reason}")]
pub struct SendRefused {
    /// Why the operation was not applied.
    pub reason: String,
}

impl SendRefused {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Malformed `latch send` input, as opposed to a capability refusal.
///
/// The gateway maps this to HTTP 400.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{reason}")]
pub struct SendInvalid {
    /// What was wrong with the request.
    pub reason: String,
}

impl SendInvalid {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Machine-readable confirmation for `latch send --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReport {
    /// Resolved Latch session id.
    pub session_id: String,
    /// Operation that was applied.
    pub operation: &'static str,
    /// Request id for a resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Selected choice for a resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// Whether the operation reached the PTY.
    pub sent: bool,
    /// Whether a request-bound resolution was applied.
    pub resolved: bool,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    id: String,
    prompt: String,
    choices: Vec<String>,
    resolution_keys: Vec<Option<Vec<String>>>,
}

enum ScreenState {
    Idle,
    Pending(PendingRequest),
    Unsafe(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolutionRecord<'a> {
    request_id: &'a str,
    choice: &'a str,
    at: String,
}

struct ReportRequest {
    id: SessionId,
    operation: &'static str,
    request_id: Option<String>,
    choice: Option<String>,
    resolved: bool,
}

/// Reports operations that are safe for the session's current screen.
pub fn capabilities(options: InteractionOptions) -> anyhow::Result<InteractionCapabilities> {
    let id = manage::resolve_existing(&options.home, &options.session)?;
    Ok(capabilities_for(&options.home, &id)?.0)
}

/// Applies one capability-gated input operation.
pub fn send(options: SendOptions) -> anyhow::Result<SendReport> {
    let id = manage::resolve_existing(&options.home, &options.session)?;
    let _lock = interaction_lock(&options.home.session(&id))?;
    let (capabilities, state) = capabilities_for(&options.home, &id)?;

    match options.action {
        SendAction::Message(message) => {
            if !capabilities.send_message || !matches!(state, ScreenState::Idle) {
                bail!(SendRefused::new(
                    capabilities.can_send.reason.as_deref().unwrap_or(
                        "the Claude Code composer is not empty; refusing to send a message"
                    )
                ));
            }
            if message.is_empty() {
                bail!(SendInvalid::new("message must not be empty"));
            }
            if message.len() > MAX_MESSAGE_BYTES {
                bail!(SendInvalid::new(format!(
                    "message exceeds {MAX_MESSAGE_BYTES} bytes"
                )));
            }
            engine::paste_message(engine::PasteMessageRequest {
                home: &options.home,
                id: &id,
                message: message.as_bytes(),
            })?;
            Ok(report(ReportRequest {
                id,
                operation: "message",
                request_id: None,
                choice: None,
                resolved: false,
            }))
        }
        SendAction::Keys(keys) => {
            if !capabilities.send_keys {
                bail!(SendRefused::new(
                    capabilities
                        .can_send
                        .reason
                        .as_deref()
                        .unwrap_or("direct keypresses are unsafe on the current screen")
                ));
            }
            let keys = parse_keys(&keys)?;
            engine::send_keys(engine::SendKeysRequest {
                home: &options.home,
                id: &id,
                keys: &keys,
            })?;
            Ok(report(ReportRequest {
                id,
                operation: "keys",
                request_id: None,
                choice: None,
                resolved: false,
            }))
        }
        SendAction::Resolve { request_id, choice } => {
            if request_id.is_empty() || choice.is_empty() {
                bail!(SendInvalid::new("resolve requires requestId and choice"));
            }
            if !capabilities.resolve {
                bail!(SendRefused::new(
                    capabilities
                        .can_send
                        .reason
                        .as_deref()
                        .unwrap_or("there is no visible unresolved request for this session")
                ));
            }
            let ScreenState::Pending(pending) = state else {
                bail!(SendRefused::new(
                    "there is no visible unresolved request for this session"
                ));
            };
            if pending.id != request_id {
                bail!(SendRefused::new(format!(
                    "request `{request_id}` is stale; the visible request is `{}`",
                    pending.id
                )));
            }
            let Some(choice_index) = pending
                .choices
                .iter()
                .position(|candidate| candidate == &choice)
            else {
                bail!(SendRefused::new(format!(
                    "choice `{choice}` is not valid for request `{request_id}`; expected one of: {}",
                    pending.choices.join(", ")
                )));
            };
            let Some(keys) = pending
                .resolution_keys
                .get(choice_index)
                .and_then(Option::as_ref)
            else {
                bail!(SendRefused::new(format!(
                    "choice `{choice}` is not identifiable on the current screen"
                )));
            };
            engine::send_keys(engine::SendKeysRequest {
                home: &options.home,
                id: &id,
                keys,
            })?;
            record_resolution(
                &options.home.session(&id),
                ResolutionRecord {
                    request_id: &request_id,
                    choice: &choice,
                    at: engine::format_rfc3339(SystemTime::now()),
                },
            )?;
            Ok(report(ReportRequest {
                id,
                operation: "resolve",
                request_id: Some(request_id),
                choice: Some(choice),
                resolved: true,
            }))
        }
    }
}

fn interaction_lock(paths: &SessionPaths) -> anyhow::Result<fs::File> {
    let path = paths.harness_interaction_lock();
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(FILE_MODE)
        .open(&path)
        .with_context(|| format!("cannot open interaction lock {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(FILE_MODE))?;
    // SAFETY: `file` remains alive until the input operation and its resolution
    // record are complete.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(std::io::Error::last_os_error()).context("cannot lock session interaction");
    }
    Ok(file)
}

fn capabilities_for(
    home: &LatchHome,
    id: &SessionId,
) -> anyhow::Result<(InteractionCapabilities, ScreenState)> {
    let Some(info) = engine::inspect(home, id)? else {
        return Ok(unavailable("the private tmux session is unavailable"));
    };
    if info.state != SessionState::Running {
        return Ok(unavailable("the session process has exited"));
    }
    let paths = home.session(id);
    let screen = engine::capture_pane(home, id)?;
    let state = classify_screen(&paths, &screen)?;
    if !hosts_known_harness(&paths)? && !matches!(state, ScreenState::Unsafe(_)) {
        return Ok(unknown_harness(state));
    }
    let capabilities = match &state {
        ScreenState::Idle => InteractionCapabilities {
            send_message: true,
            send_keys: true,
            resolve: false,
            can_send: CanSend {
                ok: true,
                reason: None,
            },
        },
        ScreenState::Pending(request) => InteractionCapabilities {
            send_message: false,
            send_keys: true,
            resolve: request.resolution_keys.iter().any(Option::is_some),
            can_send: CanSend {
                ok: true,
                reason: None,
            },
        },
        ScreenState::Unsafe(reason) => InteractionCapabilities {
            send_message: false,
            send_keys: false,
            resolve: false,
            can_send: CanSend {
                ok: false,
                reason: Some(reason.clone()),
            },
        },
    };
    Ok((capabilities, state))
}

fn unknown_harness(state: ScreenState) -> (InteractionCapabilities, ScreenState) {
    (
        InteractionCapabilities {
            send_message: false,
            send_keys: true,
            resolve: false,
            can_send: CanSend {
                ok: false,
                reason: Some(UNKNOWN_HARNESS_REASON.to_owned()),
            },
        },
        state,
    )
}

pub(crate) fn hosts_known_harness(paths: &SessionPaths) -> anyhow::Result<bool> {
    if paths.harness_hooks().is_file() {
        return Ok(true);
    }
    match meta::read(paths) {
        Ok(metadata) => {
            Ok(metadata.harness.as_deref() == Some("claude") || metadata.command_label == "claude")
        }
        Err(meta::MetaError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn unavailable(reason: &str) -> (InteractionCapabilities, ScreenState) {
    (
        InteractionCapabilities {
            send_message: false,
            send_keys: false,
            resolve: false,
            can_send: CanSend {
                ok: false,
                reason: Some(reason.to_owned()),
            },
        },
        ScreenState::Unsafe(reason.to_owned()),
    )
}

fn classify_screen(paths: &SessionPaths, screen: &str) -> anyhow::Result<ScreenState> {
    if let Some(request) = latest_pending_request(paths)? {
        if was_resolved(paths, &request.id)? {
            if screen.lines().any(is_empty_claude_composer) {
                return Ok(ScreenState::Idle);
            }
            return Ok(ScreenState::Unsafe(format!(
                "request `{}` was already resolved",
                request.id
            )));
        }
        let resolution_keys = resolution_keys(screen, &request.choices);
        if pending_is_visible(screen, &request)
            || resolution_keys
                .as_ref()
                .is_some_and(|keys| keys.iter().any(Option::is_some))
        {
            let Some(resolution_keys) = resolution_keys else {
                return Ok(ScreenState::Unsafe(format!(
                    "request `{}` is visible, but its choices cannot be identified safely",
                    request.id
                )));
            };
            return Ok(ScreenState::Pending(PendingRequest {
                resolution_keys,
                ..request
            }));
        }
        if screen.lines().any(is_empty_claude_composer) {
            return Ok(ScreenState::Idle);
        }
        return Ok(ScreenState::Unsafe(format!(
            "request `{}` is not visible on the current screen",
            request.id
        )));
    }

    if screen.lines().any(is_empty_claude_composer) {
        return Ok(ScreenState::Idle);
    }
    if screen.lines().any(is_nonempty_claude_composer) {
        return Ok(ScreenState::Unsafe(
            "the Claude Code composer already contains text".to_owned(),
        ));
    }
    Ok(ScreenState::Unsafe(
        "the current screen is not a recognized empty Claude Code composer or pending prompt"
            .to_owned(),
    ))
}

fn is_empty_claude_composer(line: &str) -> bool {
    line.trim_start()
        .strip_prefix('❯')
        .is_some_and(|rest| rest.trim().is_empty())
}

fn is_nonempty_claude_composer(line: &str) -> bool {
    line.trim_start()
        .strip_prefix('❯')
        .is_some_and(|rest| !rest.trim().is_empty())
}

fn pending_is_visible(screen: &str, request: &PendingRequest) -> bool {
    let screen = screen.to_lowercase();
    let prompt = request.prompt.to_lowercase();
    if prompt.len() >= 4 && screen.contains(&prompt) {
        return true;
    }
    request.choices.iter().any(|choice| {
        let choice = choice.to_lowercase();
        choice.len() >= 2 && screen.contains(&choice)
    })
}

struct ScreenChoice {
    number: u8,
    text: String,
    selected: bool,
}

fn resolution_keys(screen: &str, choices: &[String]) -> Option<Vec<Option<Vec<String>>>> {
    let visible = screen
        .lines()
        .filter_map(parse_screen_choice)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return None;
    }
    let selected = visible.iter().position(|choice| choice.selected);
    Some(
        choices
            .iter()
            .map(|choice| {
                let target = visible
                    .iter()
                    .position(|visible| choice_matches_screen(choice, &visible.text))?;
                if let Some(selected) = selected {
                    let (key, count) = if target >= selected {
                        ("Down", target - selected)
                    } else {
                        ("Up", selected - target)
                    };
                    let mut keys = vec![key.to_owned(); count];
                    keys.push("Enter".to_owned());
                    Some(keys)
                } else {
                    Some(vec![visible[target].number.to_string()])
                }
            })
            .collect(),
    )
}

fn parse_screen_choice(line: &str) -> Option<ScreenChoice> {
    let trimmed = line.trim_start();
    let selected = trimmed.starts_with('❯') || trimmed.starts_with('>');
    let without_marker = trimmed
        .strip_prefix('❯')
        .or_else(|| trimmed.strip_prefix('>'))
        .unwrap_or(trimmed)
        .trim_start();
    let (number, text) = without_marker.split_once('.')?;
    let number = number.trim().parse::<u8>().ok()?;
    if !(1..=9).contains(&number) || text.trim().is_empty() {
        return None;
    }
    Some(ScreenChoice {
        number,
        text: text.trim().to_lowercase(),
        selected,
    })
}

fn choice_matches_screen(choice: &str, screen: &str) -> bool {
    let choice = choice.to_lowercase();
    if choice == "allow once" {
        return (screen.contains("yes") || screen.contains("allow"))
            && !screen.contains("don't ask")
            && !screen.contains("always")
            && !screen.contains("session");
    }
    if choice == "deny" {
        return screen == "no"
            || screen.starts_with("no,")
            || screen.contains("deny")
            || screen.contains("reject");
    }
    screen.contains(&choice)
}

fn latest_pending_request(paths: &SessionPaths) -> anyhow::Result<Option<PendingRequest>> {
    let path = paths.harness_hooks();
    if !path.is_file() {
        return Ok(None);
    }
    let raw = read_locked(&path, "hook sidecar")?;
    for line in raw.lines().rev().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line).context("malformed Claude hook record")?;
        let Some(derived) = permission::from_record(&record) else {
            continue;
        };
        return Ok(Some(PendingRequest {
            id: derived.request_id,
            prompt: derived.prompt,
            choices: derived.choices.unwrap_or_default(),
            resolution_keys: Vec::new(),
        }));
    }
    Ok(None)
}

fn was_resolved(paths: &SessionPaths, request_id: &str) -> anyhow::Result<bool> {
    let path = paths.harness_resolutions();
    if !path.is_file() {
        return Ok(false);
    }
    let raw = read_locked(&path, "resolution ledger")?;
    Ok(raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|record| record.get("requestId").and_then(Value::as_str) == Some(request_id)))
}

fn record_resolution(paths: &SessionPaths, resolution: ResolutionRecord<'_>) -> anyhow::Result<()> {
    let path = paths.harness_resolutions();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(&path)
        .with_context(|| format!("cannot append resolution ledger {}", path.display()))?;
    // SAFETY: `file` remains alive through the complete append and flush.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(std::io::Error::last_os_error()).context("cannot lock resolution ledger");
    }
    let mut line = serde_json::to_vec(&resolution)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn read_locked(path: &std::path::Path, label: &str) -> anyhow::Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot open {label} {}", path.display()))?;
    // SAFETY: `file` remains alive through the complete read.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } == -1 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot lock {label}"));
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("cannot read {label} {}", path.display()))?;
    Ok(raw)
}

fn parse_keys(raw: &str) -> anyhow::Result<Vec<String>> {
    let keys = raw
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if keys.is_empty() {
        bail!(SendInvalid::new("at least one key is required"));
    }
    for key in &keys {
        let valid_named = matches!(
            key.as_str(),
            "Enter"
                | "Escape"
                | "Tab"
                | "Space"
                | "BSpace"
                | "Up"
                | "Down"
                | "Left"
                | "Right"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "DC"
        );
        let valid_modified = key.len() == 3
            && matches!(key.as_bytes()[0], b'C' | b'M')
            && key.as_bytes()[1] == b'-'
            && key.as_bytes()[2].is_ascii_graphic();
        let valid_function = key
            .strip_prefix('F')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=12).contains(&number));
        if key.chars().count() != 1 && !valid_named && !valid_modified && !valid_function {
            bail!(SendInvalid::new(format!("unsupported key `{key}`")));
        }
    }
    Ok(keys)
}

fn report(request: ReportRequest) -> SendReport {
    SendReport {
        session_id: request.id.to_string(),
        operation: request.operation,
        request_id: request.request_id,
        choice: request.choice,
        sent: true,
        resolved: request.resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_detection_distinguishes_empty_and_typed_input() {
        assert!(is_empty_claude_composer("  ❯   "));
        assert!(!is_empty_claude_composer("  ❯ cargo test"));
        assert!(is_nonempty_claude_composer("  ❯ cargo test"));
    }

    #[test]
    fn key_parser_accepts_controls_and_rejects_text_tokens() {
        assert_eq!(
            parse_keys("C-c,Down Enter").unwrap(),
            vec!["C-c", "Down", "Enter"]
        );
        assert!(parse_keys("arbitrary-text").is_err());
    }

    #[test]
    fn known_harness_evidence_comes_from_marker_hooks_or_claude_label() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let id = SessionId::parse("ses_fixture").unwrap();
        let paths = home.session(&id);
        paths.ensure().unwrap();
        assert!(!hosts_known_harness(&paths).unwrap());

        let launch = crate::session::manifest::LaunchSpec {
            argv: vec!["/bin/zsh".to_owned()],
            cwd: directory.path().to_owned(),
            env: Default::default(),
            inherit_env: true,
            size: crate::session::manifest::TerminalSize::new(80, 24),
            term: "xterm-256color".to_owned(),
        };
        let display = crate::session::manifest::DisplayMetadata::default();
        let mut metadata = meta::derive(meta::MetaRequest {
            id: id.as_str(),
            launch: &launch,
            display: &display,
            created_at: "2026-08-13T00:00:00Z",
        });
        meta::write_once(&paths, &metadata).unwrap();
        assert!(!hosts_known_harness(&paths).unwrap());

        metadata.command_label = "claude".to_owned();
        meta::update(&paths, &metadata).unwrap();
        assert!(hosts_known_harness(&paths).unwrap());

        metadata.command_label = "zsh".to_owned();
        meta::update(&paths, &metadata).unwrap();
        fs::write(paths.harness_hooks(), "{}\n").unwrap();
        assert!(hosts_known_harness(&paths).unwrap());
    }

    #[test]
    fn resolution_follows_the_visible_selection_instead_of_guessing_an_index() {
        let keys = resolution_keys(
            "❯ 1. Yes\n  2. Yes, and don't ask again\n  3. No\n",
            &["Allow once".to_owned(), "Deny".to_owned()],
        )
        .unwrap();
        assert_eq!(keys[0], Some(vec!["Enter".to_owned()]));
        assert_eq!(
            keys[1],
            Some(vec![
                "Down".to_owned(),
                "Down".to_owned(),
                "Enter".to_owned()
            ])
        );
    }
}
