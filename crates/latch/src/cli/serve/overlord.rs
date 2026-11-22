//! This Mac's Overlord readiness, as discovery reports it to a paired phone.
//!
//! A chat started from Overlord Mobile reaches Overlord through the
//! connector `ovld agent-setup <agent>` installs and the sign-in `ovld auth
//! login` stores. Both are files the Overlord CLI owns; this module only reads
//! them, never the token itself beyond checking it is present, and never
//! contacts a backend. A revoked sign-in therefore still reads as signed in:
//! the signal is guidance before a chat starts, not authorization.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::contract::{OverlordReadiness, SessionAgent};

/// Where the Overlord CLI keeps the files this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlordPaths {
    /// `auth.json`: `OVLD_HOME`, else `~/.ovld`.
    auth: PathBuf,
    /// One `<agentKey>.json` install record per connector:
    /// `$OVERLORD_HOME/.ovld/connectors`, else `~/.ovld/connectors`.
    connectors: PathBuf,
}

impl OverlordPaths {
    /// The locations the Overlord CLI itself resolves, from this process's
    /// environment. `None` without a home directory to resolve against.
    pub(super) fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        };
        let home = var("HOME");
        let data = var("OVLD_HOME").or_else(|| home.as_ref().map(|home| home.join(".ovld")))?;
        let connector_home = var("OVERLORD_HOME").or(home)?;
        Some(Self {
            auth: data.join("auth.json"),
            connectors: connector_home.join(".ovld").join("connectors"),
        })
    }

    #[cfg(test)]
    pub(super) fn rooted(root: &Path) -> Self {
        Self {
            auth: root.join(".ovld/auth.json"),
            connectors: root.join(".ovld/connectors"),
        }
    }
}

/// The readiness discovery reports for `agents`, at `now` (RFC 3339 UTC).
pub(super) fn readiness(
    paths: Option<&OverlordPaths>,
    agents: &[SessionAgent],
    now: &str,
) -> OverlordReadiness {
    let Some(paths) = paths else {
        return OverlordReadiness {
            signed_in: false,
            backend_origin: None,
            connector_agents: Vec::new(),
        };
    };
    let backend_origin = signed_in_origin(&paths.auth, now);
    OverlordReadiness {
        signed_in: backend_origin.is_some(),
        backend_origin: backend_origin.filter(|origin| !origin.is_empty()),
        connector_agents: agents
            .iter()
            .copied()
            .filter(|agent| connector_installed(&paths.connectors, *agent))
            .collect(),
    }
}

/// The fields of the CLI's stored sign-in this module needs. The token is
/// read only to confirm it is present.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSignIn {
    #[serde(rename = "type")]
    kind: Option<String>,
    token: Option<String>,
    backend_url: Option<String>,
    expires_at: Option<serde_json::Value>,
}

/// `Some(origin)` when the stored sign-in is one the CLI itself would use:
/// a known credential type, a token, a backend, and no recorded expiry at or
/// before `now`. The origin is empty when the backend URL does not parse.
fn signed_in_origin(path: &Path, now: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let stored: StoredSignIn = serde_json::from_slice(&bytes).ok()?;
    if !matches!(
        stored.kind.as_deref(),
        Some("session_bearer" | "user_token")
    ) {
        return None;
    }
    if stored
        .token
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return None;
    }
    let backend = stored.backend_url.filter(|url| !url.trim().is_empty())?;
    // The CLI writes `toISOString()`, fixed-width UTC, so the first 19
    // characters order the same way the instants do. Like the CLI, a value
    // it cannot read is ignored rather than treated as expired.
    if let Some(serde_json::Value::String(expires)) = stored.expires_at {
        if is_canonical_utc(&expires) && expires[..19] <= now[..19.min(now.len())] {
            return None;
        }
    }
    Some(origin(&backend).unwrap_or_default())
}

fn is_canonical_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 20
        && value.ends_with('Z')
        && bytes[..19]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                10 => *byte == b'T',
                13 | 16 => *byte == b':',
                _ => byte.is_ascii_digit(),
            })
}

/// Scheme, host and port only: no path, query or credentials.
fn origin(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let origin = parsed.origin().ascii_serialization();
    (origin.len() <= 512).then_some(origin)
}

/// The Overlord connector key `ovld agent-setup` records for each agent
/// Latch launches.
fn connector_key(agent: SessionAgent) -> &'static str {
    match agent {
        SessionAgent::Claude => "claude",
        SessionAgent::Codex => "codex",
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectorInstall {
    install_path: Option<String>,
}

/// Whether the connector's install record exists and still names a
/// directory: the same evidence `ovld doctor` reports as installed.
fn connector_installed(connectors: &Path, agent: SessionAgent) -> bool {
    let record = connectors.join(format!("{}.json", connector_key(agent)));
    let Ok(bytes) = std::fs::read(&record) else {
        return false;
    };
    let Ok(install) = serde_json::from_slice::<ConnectorInstall>(&bytes) else {
        return false;
    };
    let Some(path) = install.install_path.filter(|path| !path.is_empty()) else {
        return false;
    };
    // `~/` is expanded against the directory that holds `.ovld`, as the CLI
    // expands it against its resolved home.
    let path = match path.strip_prefix("~/") {
        Some(rest) => match connectors.parent().and_then(Path::parent) {
            Some(home) => home.join(rest),
            None => return false,
        },
        None => PathBuf::from(path),
    };
    path.is_absolute() && path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-22T12:00:00Z";
    const AGENTS: &[SessionAgent] = &[SessionAgent::Claude, SessionAgent::Codex];

    fn write(path: &Path, value: serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn sign_in(root: &Path, expires_at: serde_json::Value) {
        write(
            &root.join(".ovld/auth.json"),
            serde_json::json!({
                "type": "user_token",
                "token": "out_secret",
                "backendUrl": "https://user:pw@backend.example.com:8443/api?x=1",
                "updatedAt": "2026-11-01T00:00:00.000Z",
                "expiresAt": expires_at,
            }),
        );
    }

    fn install(root: &Path, key: &str, install_path: &str) {
        write(
            &root.join(format!(".ovld/connectors/{key}.json")),
            serde_json::json!({"agentKey": key, "installPath": install_path, "files": []}),
        );
    }

    #[test]
    fn a_mac_without_overlord_reports_nothing_ready() {
        let temp = tempfile::tempdir().unwrap();
        let paths = OverlordPaths::rooted(temp.path());
        let readiness = readiness(Some(&paths), AGENTS, NOW);
        assert!(!readiness.signed_in);
        assert_eq!(readiness.backend_origin, None);
        assert!(readiness.connector_agents.is_empty());
        assert!(!super::readiness(None, AGENTS, NOW).signed_in);
    }

    #[test]
    fn a_current_sign_in_reports_only_the_backend_origin() {
        let temp = tempfile::tempdir().unwrap();
        sign_in(temp.path(), serde_json::json!("2027-02-20T09:30:00.000Z"));
        let readiness = readiness(Some(&OverlordPaths::rooted(temp.path())), AGENTS, NOW);
        assert!(readiness.signed_in);
        assert_eq!(
            readiness.backend_origin.as_deref(),
            Some("https://backend.example.com:8443")
        );
        let wire = serde_json::to_string(&readiness).unwrap();
        assert!(!wire.contains("out_secret") && !wire.contains("user") && !wire.contains("api"));
    }

    #[test]
    fn an_expired_or_malformed_sign_in_is_not_signed_in() {
        let temp = tempfile::tempdir().unwrap();
        let paths = OverlordPaths::rooted(temp.path());
        sign_in(temp.path(), serde_json::json!("2026-11-22T11:59:59.000Z"));
        assert!(!readiness(Some(&paths), AGENTS, NOW).signed_in);
        // No recorded expiry (a pasted token) and an unreadable one are both
        // treated as current, the way the CLI treats them.
        sign_in(temp.path(), serde_json::Value::Null);
        assert!(readiness(Some(&paths), AGENTS, NOW).signed_in);
        sign_in(temp.path(), serde_json::json!("next week"));
        assert!(readiness(Some(&paths), AGENTS, NOW).signed_in);
        write(
            &temp.path().join(".ovld/auth.json"),
            serde_json::json!({"type": "user_token", "token": " ", "backendUrl": "https://x"}),
        );
        assert!(!readiness(Some(&paths), AGENTS, NOW).signed_in);
        std::fs::write(temp.path().join(".ovld/auth.json"), b"{not json").unwrap();
        assert!(!readiness(Some(&paths), AGENTS, NOW).signed_in);
    }

    #[test]
    fn a_connector_counts_only_while_its_install_directory_exists() {
        let temp = tempfile::tempdir().unwrap();
        let plugin = temp
            .path()
            .join(".ovld/claude/marketplace/plugins/overlord");
        std::fs::create_dir_all(&plugin).unwrap();
        install(temp.path(), "claude", plugin.to_str().unwrap());
        // A record whose directory was removed, and the app-server adapter,
        // which a terminal Codex session does not load, do not count.
        install(temp.path(), "codex", "/nonexistent/overlord");
        install(temp.path(), "codex-app-server", plugin.to_str().unwrap());
        let paths = OverlordPaths::rooted(temp.path());
        assert_eq!(
            readiness(Some(&paths), AGENTS, NOW).connector_agents,
            vec![SessionAgent::Claude]
        );
        std::fs::create_dir_all(temp.path().join(".codex/plugins/overlord")).unwrap();
        install(temp.path(), "codex", "~/.codex/plugins/overlord");
        assert_eq!(
            readiness(Some(&paths), AGENTS, NOW).connector_agents,
            vec![SessionAgent::Claude, SessionAgent::Codex]
        );
        // Only agents this gateway launches are reported.
        assert_eq!(
            readiness(Some(&paths), &[SessionAgent::Codex], NOW).connector_agents,
            vec![SessionAgent::Codex]
        );
    }
}
