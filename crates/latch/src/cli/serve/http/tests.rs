use super::*;
use crate::cli::serve::{attachments, routes::DEVICE_GRANT_HEADER};
use axum::http::header;

#[test]
fn every_registered_handler_comes_from_the_shared_route_table() {
    assert_eq!(ROUTES.len(), 11);
    let mut ids = ROUTES.iter().map(|route| route.id).collect::<Vec<_>>();
    ids.sort_by_key(|id| *id as u8);
    ids.dedup();
    assert_eq!(ids.len(), ROUTES.len());
}

#[test]
fn missing_session_is_404_without_internal_detail() {
    let mapped = map_engine_error(
        SessionLookupError::UnknownName {
            session: "secret-name".to_owned(),
        }
        .into(),
    );
    assert_eq!(mapped.status, StatusCode::NOT_FOUND);
    assert_eq!(mapped.code, "session_not_found");
    assert_eq!(mapped.message, "session not found");
    assert!(!mapped.message.contains("secret-name"));
}

#[test]
fn current_capabilities_advertise_browsing_and_creation() {
    let value = serde_json::to_value(GatewayEndpoints {
        sessions: true,
        preview: true,
        terminal: true,
        conversation: true,
        browse_directories: true,
        create_session: true,
        stop_session: true,
        attachments: true,
        agent_models: true,
    })
    .unwrap();
    assert_eq!(value["browseDirectories"], true);
    assert_eq!(value["createSession"], true);
    assert_eq!(value["stopSession"], true);
    assert_eq!(value["agentModels"], true);
}

/// The create route names the agents it launches so a phone shows only
/// controls this gateway serves; the wire spelling is the schema's enum.
#[test]
fn current_capabilities_advertise_supported_session_agents() {
    let value = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: SESSION_AGENTS.to_vec(),
        attachment_max_bytes: None,
    })
    .unwrap();
    assert_eq!(
        value["sessionAgents"],
        serde_json::json!(["claude", "codex"])
    );
    // A gateway that launches no agents omits the key, which an older
    // client decoding a closed object still accepts.
    let none = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: None,
    })
    .unwrap();
    assert!(none.get("sessionAgents").is_none());
    assert!(none.get("attachmentMaxBytes").is_none());
}

#[test]
fn cors_allows_the_bounded_post_route() {
    let mut headers = HeaderMap::new();
    apply_cors(&mut headers, None);
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET, POST, OPTIONS"
    );
}

struct Harness {
    _dir: tempfile::TempDir,
    address: SocketAddr,
    home: LatchHome,
    work: std::path::PathBuf,
}

/// The production router on a real socket, so creation requests travel the
/// same token, grant, and route-table path a phone's do.
async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp");
    let home = LatchHome::new(dir.path().join("latch"));
    home.ensure().expect("home");
    let work = dir.path().join("work");
    std::fs::create_dir(&work).expect("work");
    let token_file = dir.path().join("serve.token");
    std::fs::write(&token_file, "gateway-token").expect("token");
    let hub = crate::conversation::ConversationHub::new(dir.path().join("hub")).expect("hub");
    let app = test_router(home.clone(), token_file, hub, dir.path().join("stub-latch"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Harness {
        _dir: dir,
        address,
        home,
        work,
    }
}

async fn post_create(
    harness: &Harness,
    grant: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    send(harness, "POST", "/v2/sessions", grant, body).await
}

#[tokio::test]
async fn loopback_gateway_rejects_duplicate_grant_headers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let harness = harness().await;
    let request = format!(
        "GET /v2/sessions HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\n{DEVICE_GRANT_HEADER}: observe\r\n{DEVICE_GRANT_HEADER}: control\r\nConnection: close\r\n\r\n"
    );
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 400 "),
        "{}",
        String::from_utf8_lossy(&response)
    );
}

async fn send(
    harness: &Harness,
    method: &str,
    target: &str,
    grant: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let grant = grant.map_or_else(String::new, |grant| {
        format!("{DEVICE_GRANT_HEADER}: {grant}\r\n")
    });
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/json\r\nConnection: close\r\n{grant}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status line");
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let payload = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    (status, payload)
}

fn create_body(cwd: &str) -> String {
    serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": cwd,
    })
    .to_string()
}

#[tokio::test]
async fn creation_refuses_malformed_requests_before_touching_the_engine() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap().to_owned();
    let cases = [
        ("not json", 400, "invalid_request"),
        (r#"{"cwd":"/tmp"}"#, 400, "invalid_request"),
        (
            &serde_json::json!({"requestId": "not-a-uuid", "cwd": work}).to_string(),
            400,
            "invalid_request",
        ),
        (&create_body("relative/path"), 400, "invalid_path"),
        // An agent kind this gateway does not advertise is refused as a
        // malformed request, before any directory or shell lookup.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "unsupported-agent",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        // A model belongs to an agent, and is only ever an id: never a
        // shell's model, and never something an agent could read as a flag.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "model": "claude-opus-5-5",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "claude",
                "model": "--dangerously-skip-permissions",
            })
            .to_string(),
            400,
            "invalid_request",
        ),
        // A well-formed id the agent does not list is a stale choice: the
        // phone is told to choose again, and no agent is looked up.
        (
            &serde_json::json!({
                "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
                "cwd": work,
                "agent": "codex",
                "model": "no-such-model-anywhere",
            })
            .to_string(),
            422,
            "model_unavailable",
        ),
        (
            &create_body(harness._dir.path().join("missing").to_str().unwrap()),
            404,
            "unavailable_path",
        ),
        (
            &create_body(harness._dir.path().join("serve.token").to_str().unwrap()),
            400,
            "invalid_path",
        ),
    ];
    for (body, status, code) in cases {
        let (observed, payload) = post_create(&harness, None, body).await;
        assert_eq!(observed, status, "{body}");
        assert_eq!(payload["error"], code, "{body}");
    }
    // A refused request is a refused request: nothing was created.
    assert!(harness.home.session_ids().unwrap().is_empty());
}

#[tokio::test]
async fn agent_models_are_listed_at_control_for_known_agents_only() {
    let harness = harness().await;
    for agent in ["claude", "codex"] {
        let target = format!("/v2/agents/{agent}/models");
        let (status, payload) = send(&harness, "GET", &target, Some("control"), "").await;
        assert_eq!(status, 200, "{agent}");
        assert_eq!(payload["agent"], agent);
        let models = payload["models"].as_array().expect("models");
        assert!(!models.is_empty(), "{agent} always has a list");
        for model in models {
            assert!(agent_models::is_model_id(model["id"].as_str().unwrap()));
            assert!(!model["name"].as_str().unwrap().is_empty());
        }
        assert!(payload.get("defaultModel").is_some());
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", &target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not list {agent} models");
        }
    }
    let (status, payload) = send(
        &harness,
        "GET",
        "/v2/agents/cursor/models",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(payload["error"], "not_found");
}

#[tokio::test]
async fn creation_is_refused_below_control_and_oversized_bodies_are_bounded() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap();
    let claude = serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": work,
        "agent": "claude",
    })
    .to_string();
    for grant in ["observe", "interact"] {
        let (status, _) = post_create(&harness, Some(grant), &create_body(work)).await;
        assert_eq!(status, 403, "{grant} must not create sessions");
        let (status, _) = post_create(&harness, Some(grant), &claude).await;
        assert_eq!(status, 403, "{grant} must not launch agents");
    }
    let oversized = serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": "/".to_owned() + &"a".repeat(MAX_CREATE_BODY_BYTES),
    })
    .to_string();
    let (status, payload) = post_create(&harness, Some("control"), &oversized).await;
    assert_eq!(status, 413);
    assert_eq!(payload["error"], "invalid_request");
    assert!(harness.home.session_ids().unwrap().is_empty());
}

/// Ending what is running on the Mac is a control operation. An observing
/// or interacting phone must not reach it, and the refusal has to happen
/// before the engine is asked anything.
#[tokio::test]
async fn stopping_is_refused_below_the_control_grant() {
    let harness = harness().await;
    for grant in ["observe", "interact"] {
        let (status, _) = send(
            &harness,
            "POST",
            "/v2/sessions/ses_missing/stop",
            Some(grant),
            "",
        )
        .await;
        assert_eq!(status, 403, "{grant} must not stop sessions");
    }
}

/// A stale list is the ordinary case for a phone: the row it tapped may
/// already be gone. That answers 404, not an internal error, and it never
/// echoes the name back.
#[tokio::test]
async fn stopping_an_unknown_session_is_a_plain_not_found() {
    let harness = harness().await;
    let (status, payload) = send(
        &harness,
        "POST",
        "/v2/sessions/ses_missing/stop",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
    // The code is what a client branches on: a bare 404 from a gateway
    // that never had the route carries no `error` at all, and this must
    // never be mistaken for it.
    assert_eq!(payload["error"], "session_not_found");
    assert_eq!(payload["reason"], "session not found");
    assert!(!payload["reason"].to_string().contains("ses_missing"));
}

/// The route table is method-scoped, so a read of the same path is not a
/// route at all. Nothing about a session can be ended by fetching a URL.
#[tokio::test]
async fn a_get_on_the_stop_path_is_not_a_route() {
    let harness = harness().await;
    let (status, _) = send(
        &harness,
        "GET",
        "/v2/sessions/ses_missing/stop",
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404);
}

const ATTACHMENT_SESSION: &str = "ses_attachtest";

/// Records one session whose working directory is the harness's `work`
/// folder. No daemon runs: the attachments route reads only the record.
fn record_attachment_session(harness: &Harness) {
    use crate::session::manifest::{SourceInfo, TerminalSize};
    use crate::session::meta::{self, SessionMeta};
    use crate::session::paths::SessionId;

    let id = SessionId::parse(ATTACHMENT_SESSION).expect("session id");
    let paths = harness.home.session(&id);
    paths.ensure().expect("session dir");
    meta::write_once(
        &paths,
        &SessionMeta {
            format_version: 1,
            id: id.as_str().to_owned(),
            name: "attach".into(),
            title: None,
            cwd: harness.work.clone(),
            command_label: "claude".into(),
            harness: None,
            created_at: "2026-09-25T00:00:00Z".into(),
            initial_size: TerminalSize::new(80, 24),
            source: SourceInfo {
                kind: "test".into(),
                external_run_id: None,
            },
        },
    )
    .expect("write meta");
}

/// Sends raw bytes, so a test can lie about `Content-Length` or omit it.
async fn send_raw(harness: &Harness, request: Vec<u8>) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream.write_all(&request).await.expect("write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status line");
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    (
        status,
        serde_json::from_str(payload).unwrap_or(serde_json::Value::Null),
    )
}

fn upload_request(target: &str, grant: Option<&str>, length: Option<u64>, body: &[u8]) -> Vec<u8> {
    let grant = grant.map_or_else(String::new, |grant| {
        format!("{DEVICE_GRANT_HEADER}: {grant}\r\n")
    });
    let length = length.map_or_else(String::new, |length| {
        format!("Content-Length: {length}\r\n")
    });
    let mut request = format!(
        "POST {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n{grant}{length}\r\n"
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

fn attachments_in(harness: &Harness) -> Vec<String> {
    let folder = harness.work.join(attachments::ATTACHMENTS_DIR);
    let mut names = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != ".gitignore")
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The happy path: the file lands under the session's own folder with a
/// sanitized name, and the receipt carries the path the message will use.
#[tokio::test]
async fn an_attachment_lands_in_the_session_workspace() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=Screen%20Shot.PNG");
    let (status, receipt) = send_raw(
        &harness,
        upload_request(&target, Some("interact"), Some(5), b"hello"),
    )
    .await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["name"], "Screen-Shot.png");
    assert_eq!(
        receipt["relativePath"],
        ".latch-attachments/Screen-Shot.png"
    );
    assert_eq!(receipt["bytes"], 5);
    let path = std::path::PathBuf::from(receipt["path"].as_str().unwrap());
    assert!(path.starts_with(harness.work.canonicalize().unwrap()));
    assert_eq!(std::fs::read(path).unwrap(), b"hello");
}

/// Writing into the workspace is part of sending a message: an observing
/// phone is refused before the engine or the filesystem is touched.
#[tokio::test]
async fn attachments_are_refused_below_the_interact_grant() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let (status, _) = send_raw(
        &harness,
        upload_request(&target, Some("observe"), Some(1), b"x"),
    )
    .await;
    assert_eq!(status, 403);
    assert!(!harness.work.join(attachments::ATTACHMENTS_DIR).exists());
}

#[tokio::test]
async fn attachment_bodies_must_declare_a_bounded_nonzero_length() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let cases = [
        (None, b"x".as_slice(), 411, "invalid_request"),
        (Some(0), b"".as_slice(), 400, "invalid_request"),
        (
            Some(ATTACHMENT_MAX_BYTES + 1),
            b"x".as_slice(),
            413,
            "attachment_too_large",
        ),
    ];
    for (length, body, status, code) in cases {
        let (observed, payload) = send_raw(
            &harness,
            upload_request(&target, Some("interact"), length, body),
        )
        .await;
        assert_eq!(observed, status, "{length:?}");
        assert_eq!(payload["error"], code, "{length:?}");
    }
    // A refused length is refused before the folder is even created.
    assert!(!harness.work.join(attachments::ATTACHMENTS_DIR).exists());
}

/// A body that stops short of what it declared leaves no partial file for
/// an agent to find.
#[tokio::test]
async fn a_truncated_attachment_leaves_nothing_behind() {
    use tokio::io::AsyncWriteExt;

    let harness = harness().await;
    record_attachment_session(&harness);
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    // The phone promises ten bytes, sends five, and goes away.
    let mut stream = tokio::net::TcpStream::connect(harness.address)
        .await
        .expect("connect");
    stream
        .write_all(&upload_request(
            &target,
            Some("interact"),
            Some(10),
            b"short",
        ))
        .await
        .expect("write request");
    let mut appeared = false;
    for _ in 0..100 {
        appeared = !attachments_in(&harness).is_empty();
        if appeared {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        appeared,
        "the partial file is created while the body streams"
    );
    drop(stream);
    // The handler cleans up when its future ends; give it a moment.
    for _ in 0..100 {
        if attachments_in(&harness).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(attachments_in(&harness).is_empty());
}

#[tokio::test]
async fn attaching_to_an_unknown_session_is_a_plain_not_found() {
    let harness = harness().await;
    let (status, payload) = send_raw(
        &harness,
        upload_request(
            "/v2/sessions/ses_missing/attachments?name=a.txt",
            Some("interact"),
            Some(1),
            b"x",
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(payload["error"], "session_not_found");
}

/// A symlink where the folder belongs would let a planted link send the
/// write outside the workspace. The route refuses it with a stable code
/// and writes nothing at the link's target.
#[tokio::test]
async fn a_symlinked_attachments_folder_is_refused() {
    let harness = harness().await;
    record_attachment_session(&harness);
    let elsewhere = harness._dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, harness.work.join(attachments::ATTACHMENTS_DIR))
        .unwrap();
    let target = format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=a.txt");
    let (status, payload) = send_raw(
        &harness,
        upload_request(&target, Some("interact"), Some(1), b"x"),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(payload["error"], "attachments_folder_unsafe");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn current_capabilities_advertise_attachments_with_their_limit() {
    let value = serde_json::to_value(GatewayFeatures {
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: Some(ATTACHMENT_MAX_BYTES),
    })
    .unwrap();
    assert_eq!(value["attachmentMaxBytes"], ATTACHMENT_MAX_BYTES);
}

#[test]
fn only_a_canonical_uuid_is_accepted_as_a_request_id() {
    assert!(is_request_id("8cba5d78-79a0-4a55-9047-f77e57e463c7"));
    assert!(is_request_id("8CBA5D78-79A0-4A55-9047-F77E57E463C7"));
    for rejected in [
        "",
        "8cba5d78",
        "8cba5d78-79a0-4a55-9047-f77e57e463c",
        "8cba5d78-79a0-4a55-9047-f77e57e463c7-extra",
        "8cba5d78-79a0-4a55-9047-f77e57e463cg",
        "8cba5d7879a04a559047f77e57e463c7",
        "../../etc/passwd",
    ] {
        assert!(!is_request_id(rejected), "{rejected} must be refused");
    }
}

#[test]
fn creation_failures_have_stable_codes_without_engine_detail() {
    let conflict = map_remote_session_error(RemoteSessionError::RequestIdConflict);
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.code, "request_id_conflict");

    let failed = map_remote_session_error(RemoteSessionError::Failed(anyhow::anyhow!(
        "cannot spawn /opt/homebrew/bin/latchd for /Users/person/secret"
    )));
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(failed.code, "session_creation_failed");
    assert!(!failed.message.contains("/Users/"));
    assert!(!failed.message.contains("latchd"));

    // A missing agent is a stable, phone-readable refusal that names the
    // product, not a path on the Mac.
    let missing =
        map_remote_session_error(RemoteSessionError::AgentUnavailable(SessionAgent::Claude));
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(missing.code, "agent_unavailable");
    assert!(missing.message.contains("Claude Code"));
    assert!(!missing.message.contains('/'));
}

#[test]
fn directory_failures_have_stable_codes_without_path_detail() {
    let cases = [
        (BrowseError::InvalidPath, "invalid_path"),
        (BrowseError::UnavailablePath, "unavailable_path"),
        (BrowseError::UnreadableDirectory, "unreadable_directory"),
        (BrowseError::StaleCursor, "stale_cursor"),
    ];
    for (error, code) in cases {
        let mapped = map_browse_error(error);
        assert_eq!(mapped.code, code);
        assert!(!mapped.message.contains("/Users/"));
    }
}
