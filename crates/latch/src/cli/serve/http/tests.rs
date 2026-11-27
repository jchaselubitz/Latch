use super::*;
use crate::cli::serve::{attachments, routes::DEVICE_GRANT_HEADER};
use axum::http::header;

#[test]
fn every_registered_handler_comes_from_the_shared_route_table() {
    assert_eq!(ROUTES.len(), 45);
    let mut ids = ROUTES.iter().map(|route| route.id).collect::<Vec<_>>();
    ids.sort_by_key(|id| *id as u8);
    ids.dedup();
    assert_eq!(ids.len(), ROUTES.len());
}

#[tokio::test]
async fn saved_chat_resume_requires_control_and_an_original_pairing() {
    let harness = harness().await;
    let path = "/v2/saved-chats/2f1c8a0e-6a1b-4f7e-9d2c-5b8e7a6c4d31/resume";
    for method in ["GET", "POST"] {
        let (denied, _) = send(&harness, method, path, Some("interact"), "").await;
        assert_eq!(denied, 403);
    }
    let (missing, result) = send_as_device(&harness, "GET", path, "other-phone", "").await;
    assert_eq!(missing, 404);
    assert_eq!(result["error"], "saved_chat_unavailable");
    let (bad_input, _) = send_as_device(
        &harness,
        "POST",
        path,
        "other-phone",
        r#"{"requestId":"2f1c8a0e-6a1b-4f7e-9d2c-5b8e7a6c4d31","cwd":"/tmp"}"#,
    )
    .await;
    assert_eq!(bad_input, 400);
}

#[tokio::test]
async fn saved_chat_discovery_requires_a_control_grant_and_pairing() {
    let harness = harness().await;
    let path = "/v2/saved-chats";
    for grant in ["observe", "interact"] {
        let (status, _) = send(&harness, "GET", path, Some(grant), "").await;
        assert_eq!(status, 403);
    }
    let (unpaired, _) = send(&harness, "GET", path, Some("control"), "").await;
    assert_eq!(unpaired, 404);
    let (status, result) = send_as_device(&harness, "GET", path, "original-phone", "").await;
    assert_eq!(status, 200);
    assert_eq!(result["chats"], serde_json::json!([]));
}

#[tokio::test]
async fn git_wire_bound_counts_json_escaping_and_never_sends_a_512_kib_payload() {
    let error = git::bounded_json(StatusCode::OK, "\u{1}".repeat(100_000)).unwrap_err();
    assert_eq!(error.code, "response_too_large");
    let response = error.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), 512 * 1024)
        .await
        .unwrap();
    assert!(bytes.len() < 512 * 1024);
    let response = git::bounded_json(StatusCode::ACCEPTED, "small").unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
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
        shared_files: true,
        create_session: true,
        saved_chats: true,
        saved_chat_resume: true,
        stop_session: true,
        attachments: true,
        agent_models: true,
        notification_key: true,
        git_repositories: true,
        git_resolve: true,
        git_worktree_reads: true,
        git_worktree_summaries: true,
        git_worktree_writes: true,
        git_worktree_previews: true,
        git_status: true,
        git_diff: true,
        git_branches: true,
        git_stage: true,
        git_unstage: true,
        git_commit: true,
        git_commit_message: true,
        git_create_branch: true,
        git_switch_branch: true,
        git_receipts: true,
        git_sync: true,
        git_fetch: true,
        git_fast_forward: true,
        git_merge: true,
        git_rebase: true,
        git_continue: true,
        git_abort: true,
        git_stage_resolved: true,
        git_push_preview: true,
        git_push: true,
        git_push_reconcile: true,
    })
    .unwrap();
    for name in [
        "gitRepositories",
        "gitResolve",
        "gitWorktreeReads",
        "gitWorktreeSummaries",
        "gitWorktreeWrites",
        "gitWorktreePreviews",
        "gitStatus",
        "gitDiff",
        "gitBranches",
        "gitStage",
        "gitUnstage",
        "gitCommit",
        "gitCreateBranch",
        "gitSwitchBranch",
        "gitReceipts",
        "gitSync",
        "gitFetch",
        "gitFastForward",
        "gitMerge",
        "gitRebase",
        "gitContinue",
        "gitAbort",
        "gitStageResolved",
        "gitPushPreview",
        "gitPush",
        "gitPushReconcile",
    ] {
        assert_eq!(value[name], true, "{name}");
    }
    assert_eq!(value["gitCommitMessage"], true);
    assert_eq!(value["savedChats"], true);
    assert_eq!(value["savedChatResume"], true);
    assert!(ROUTES
        .iter()
        .any(|route| route.pattern == "/v2/git/commit-message"
            && route.required_grant == Grant::Control));
    assert_eq!(value["notificationKey"], true);
    assert_eq!(value["browseDirectories"], true);
    assert_eq!(value["sharedFiles"], true);
    assert_eq!(value["createSession"], true);
    assert_eq!(value["stopSession"], true);
    assert_eq!(value["agentModels"], true);
}

/// The create route names the agents it launches so a phone shows only
/// controls this gateway serves; the wire spelling is the schema's enum.
#[test]
fn current_capabilities_advertise_supported_session_agents() {
    let value = serde_json::to_value(GatewayFeatures {
        git_commit_message: None,
        exclusive_terminal: true,
        session_agents: SESSION_AGENTS.to_vec(),
        attachment_max_bytes: None,
        session_provenance: None,
        session_instructions: None,
        overlord: None,
    })
    .unwrap();
    assert_eq!(
        value["sessionAgents"],
        serde_json::json!(["claude", "codex"])
    );
    // A gateway that launches no agents omits the key, which an older
    // client decoding a closed object still accepts.
    let none = serde_json::to_value(GatewayFeatures {
        git_commit_message: None,
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: None,
        session_provenance: None,
        session_instructions: None,
        overlord: None,
    })
    .unwrap();
    assert!(none.get("sessionAgents").is_none());
    assert!(none.get("attachmentMaxBytes").is_none());
    assert!(none.get("sessionProvenance").is_none());
    assert_eq!(
        serde_json::from_value::<GatewayFeatures>(none)
            .unwrap()
            .session_provenance,
        None
    );
}

/// Orientation and Overlord readiness are optional keys: a gateway without
/// them omits both, which a client reads as "create without instructions" and
/// "unknown", and an older client decoding a closed object never sees them.
#[test]
fn instruction_and_overlord_features_are_absent_unless_served() {
    use super::super::contract::{OverlordReadiness, SessionInstructionsSupport};
    let none = serde_json::to_value(GatewayFeatures {
        git_commit_message: None,
        exclusive_terminal: true,
        session_agents: SESSION_AGENTS.to_vec(),
        attachment_max_bytes: None,
        session_provenance: None,
        session_instructions: None,
        overlord: None,
    })
    .unwrap();
    assert!(none.get("sessionInstructions").is_none());
    assert!(none.get("overlord").is_none());
    let served = serde_json::to_value(GatewayFeatures {
        git_commit_message: None,
        exclusive_terminal: true,
        session_agents: SESSION_AGENTS.to_vec(),
        attachment_max_bytes: None,
        session_provenance: None,
        session_instructions: Some(SessionInstructionsSupport {
            agents: vec![SessionAgent::Claude],
            max_bytes: 8192,
        }),
        overlord: Some(OverlordReadiness {
            signed_in: false,
            backend_origin: None,
            connector_agents: vec![SessionAgent::Codex],
        }),
    })
    .unwrap();
    assert_eq!(
        served["sessionInstructions"],
        serde_json::json!({"agents": ["claude"], "maxBytes": 8192})
    );
    assert_eq!(
        served["overlord"],
        serde_json::json!({"signedIn": false, "connectorAgents": ["codex"]})
    );
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
    let address = start_harness_router(dir.path(), &home).await;
    Harness {
        _dir: dir,
        address,
        home,
        work,
    }
}

async fn start_harness_router(dir: &std::path::Path, home: &LatchHome) -> SocketAddr {
    let token_file = dir.join("serve.token");
    std::fs::write(&token_file, "gateway-token").expect("token");
    let hub = crate::conversation::ConversationHub::new(dir.join("hub")).expect("hub");
    let app = test_router(home.clone(), token_file, hub, dir.join("stub-latch"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    address
}

async fn post_create(
    harness: &Harness,
    grant: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    send(harness, "POST", "/v2/sessions", grant, body).await
}

#[tokio::test]
async fn session_list_exposes_only_public_provenance_at_each_read_grant() {
    use crate::session::paths::SessionId;
    use serde_json::json;

    let harness = harness().await;
    let cases = [
        (
            "overlord",
            Some(
                json!({"kind":"overlord", "external_run_id":"shared-run", "request_id":"private-retry"}),
            ),
            Some(json!({"kind":"overlord", "externalRunId":"shared-run"})),
        ),
        (
            "mobile",
            Some(json!({"kind":"mobile", "external_run_id":"shared-run"})),
            Some(json!({"kind":"mobile", "externalRunId":"shared-run"})),
        ),
        (
            "shell",
            Some(json!({"kind":"cli"})),
            Some(json!({"kind":"cli", "externalRunId":null})),
        ),
        ("legacy", None, None),
        (
            "incomplete",
            Some(json!({"external_run_id":"orphan"})),
            None,
        ),
    ];
    for (name, source, _) in &cases {
        let id = SessionId::parse(&format!("ses_{name}")).unwrap();
        let paths = harness.home.session(&id);
        paths.ensure().unwrap();
        let mut meta = json!({
            "format_version":1, "id":id.as_str(), "name":name, "cwd":harness.work,
            "command_label":"test", "created_at":"2026-11-22T00:00:00Z",
            "initial_size":{"cols":80,"rows":24}
        });
        if let Some(source) = source {
            meta["source"] = source.clone();
        }
        std::fs::write(paths.meta(), serde_json::to_vec(&meta).unwrap()).unwrap();
    }
    for grant in ["observe", "interact", "control"] {
        let (status, report) = send(&harness, "GET", "/v2/sessions", Some(grant), "").await;
        assert_eq!(status, 200, "{report}");
        let rows = report["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), cases.len());
        for (name, _, expected) in &cases {
            let row = rows.iter().find(|row| row["name"] == *name).unwrap();
            assert_eq!(row.get("source"), expected.as_ref());
        }
    }
    let (status, body) = send_raw(
        &harness,
        b"GET /v2/sessions HTTP/1.1\r\nHost: gateway\r\nConnection: close\r\n\r\n".to_vec(),
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.get("sessions").is_none());
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
async fn draft_endpoint_smoke_checks_control_configuration_and_readiness() {
    let harness = harness().await;
    let body = serde_json::json!({
        "requestId": "12345678-1234-1234-1234-123456789abc",
        "path": "/not-read-when-unconfigured",
        "expected": { "branch": null, "head": null, "index": "a".repeat(64) }
    })
    .to_string();
    let (status, value) = send(
        &harness,
        "POST",
        "/v2/git/commit-message",
        Some("observe"),
        &body,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(value["code"], "unauthorized");
    let (status, value) = send(
        &harness,
        "POST",
        "/v2/git/commit-message",
        Some("control"),
        &body,
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(value["code"], "unconfigured");
    assert!(!value.to_string().contains("not-read"));
    let (status, value) = send(&harness, "GET", "/v2/capabilities", Some("observe"), "").await;
    assert_eq!(status, 200);
    assert_eq!(value["endpoints"]["gitCommitMessage"], true);
    assert_eq!(
        value["features"]["gitCommitMessage"]["state"],
        "unconfigured"
    );
    std::fs::write(
        harness.home.root().join("commit-message-model"),
        "https://other-host/model",
    )
    .unwrap();
    let (status, value) = send(
        &harness,
        "POST",
        "/v2/git/commit-message",
        Some("control"),
        &body,
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(value["code"], "provider_error");
    let (_, value) = send(&harness, "GET", "/v2/capabilities", Some("observe"), "").await;
    assert_eq!(
        value["features"]["gitCommitMessage"]["state"],
        "unavailable"
    );
}

#[tokio::test]
async fn draft_endpoint_rejects_invalid_requests_and_cancel_requires_control() {
    let harness = harness().await;
    for body in ["not JSON".to_owned(), "{}".into(), " ".repeat(8193),
        serde_json::json!({"requestId":"bad-id", "path":"/never-read", "expected":{"branch":null,"head":null,"index":"a".repeat(64)}}).to_string()] {
        let (status, value) = send(&harness, "POST", "/v2/git/commit-message", Some("control"), &body).await;
        assert_eq!(status, 400);
        assert_eq!(value["code"], "invalid_request");
        assert!(!value.to_string().contains("never-read"));
    }
    let body = r#"{"requestId":"12345678-1234-1234-1234-123456789abc"}"#;
    for grant in ["observe", "interact"] {
        let (status, value) = send(
            &harness,
            "POST",
            "/v2/git/commit-message/cancel",
            Some(grant),
            body,
        )
        .await;
        assert_eq!(status, 403);
        assert_eq!(value["code"], "unauthorized");
    }
    // No admission/configuration is required to cancel an unknown request.
    let (status, _) = send(
        &harness,
        "POST",
        "/v2/git/commit-message/cancel",
        Some("control"),
        body,
    )
    .await;
    assert_eq!(status, 204);
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

/// Discovery is how a phone learns it may send `instructions`: every agent
/// the create route launches is listed with the byte limit, and the Mac's
/// Overlord readiness is always present (its values depend on the Mac).
#[tokio::test]
async fn capabilities_advertise_provenance_instructions_and_overlord_readiness() {
    let harness = harness().await;
    let (status, value) = send(&harness, "GET", "/v2/capabilities", Some("observe"), "").await;
    assert_eq!(status, 200);
    assert_eq!(value["features"]["sessionProvenance"], true);
    assert_eq!(
        value["features"]["sessionInstructions"],
        serde_json::json!({
            "agents": ["claude", "codex"],
            "maxBytes": create::INSTRUCTIONS_MAX_BYTES,
        })
    );
    let overlord = &value["features"]["overlord"];
    assert!(overlord["signedIn"].is_boolean(), "{overlord}");
    assert!(overlord["connectorAgents"].is_array(), "{overlord}");
    assert!(!overlord.to_string().contains("token"));
}

/// Instructions are refused before anything is accepted when there is no
/// agent to give them to or the text is outside the advertised bounds, and a
/// largest-allowed orientation still fits the create body.
#[tokio::test]
async fn creation_refuses_instructions_it_cannot_deliver_as_system_text() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap();
    let body = |agent: Option<&str>, instructions: serde_json::Value| {
        let mut body = serde_json::json!({
            "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
            "cwd": work,
            "instructions": instructions,
        });
        if let Some(agent) = agent {
            body["agent"] = agent.into();
        }
        body.to_string()
    };
    let too_long = "x".repeat(create::INSTRUCTIONS_MAX_BYTES + 1);
    for refused in [
        body(None, "Orient the shell".into()),
        body(Some("claude"), "".into()),
        body(Some("claude"), "  \n\t ".into()),
        body(Some("codex"), "bell\u{7}".into()),
        body(Some("claude"), "nul\u{0}byte".into()),
        body(Some("claude"), too_long.into()),
        body(Some("claude"), serde_json::json!(42)),
    ] {
        let (status, payload) = post_create(&harness, Some("control"), &refused).await;
        assert_eq!(status, 400, "{refused}");
        assert_eq!(payload["error"], "invalid_request", "{refused}");
    }
    // The largest orientation, escaped at worst case, stays under the bound:
    // the refusal it meets is the request validation, not the body limit.
    let largest = "\"".repeat(create::INSTRUCTIONS_MAX_BYTES);
    let largest = serde_json::json!({
        "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
        "cwd": "/".to_owned() + &"a".repeat(4095),
        "agent": "claude",
        "model": "m".repeat(128),
        "instructions": largest,
    })
    .to_string();
    assert!(largest.len() < MAX_CREATE_BODY_BYTES);
    let (status, _) = post_create(&harness, Some("control"), &largest).await;
    assert_ne!(status, 413);
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
                request_id: None,
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
        git_commit_message: None,
        exclusive_terminal: true,
        session_agents: Vec::new(),
        attachment_max_bytes: Some(ATTACHMENT_MAX_BYTES),
        session_provenance: None,
        session_instructions: None,
        overlord: None,
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

/// Every Git route is refused below the control grant on the loopback
/// router, before any folder is resolved or Git is run, and reached at
/// control through the same route table.
#[tokio::test]
async fn git_routes_are_refused_below_the_control_grant() {
    let harness = harness().await;
    let work = harness.work.to_str().unwrap().replace('/', "%2F");
    let targets = [
        "/v2/git/repositories".to_owned(),
        format!("/v2/git/resolve?path={work}"),
        format!("/v2/git/status?path={work}"),
        format!("/v2/git/diff?path={work}&file=61&area=unstaged"),
    ];
    for target in &targets {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
        let (status, body) = send(&harness, "GET", target, Some("control"), "").await;
        assert_ne!(status, 403, "control reaches {target}: {body}");
        // Git routes are reads; a write method is not a route at all.
        let (status, _) = send(&harness, "POST", target, Some("control"), "").await;
        assert!(status == 404 || status == 405, "POST {target} -> {status}");
    }
}

/// A folder that is not a repository is a stable code with no path in it.
#[tokio::test]
async fn git_refusals_have_stable_codes_without_path_detail() {
    let harness = harness().await;
    let plain = tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("plain folder outside any repository");
    let path = plain.path().to_str().unwrap().replace('/', "%2F");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/status?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"], "not_a_repository");
    assert!(!body["reason"].as_str().unwrap().contains("/tmp"));

    let (status, body) = send(&harness, "GET", "/v2/git/status", Some("control"), "").await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/diff?path={path}&file=61&area=everything"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");
}

/// Sends `body` as the loopback proxy would for a paired device.
async fn send_as_device(
    harness: &Harness,
    method: &str,
    target: &str,
    device: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer gateway-token\r\nContent-Type: application/json\r\nConnection: close\r\n{DEVICE_GRANT_HEADER}: control\r\n{}: {device}\r\nContent-Length: {}\r\n\r\n{body}",
        crate::cli::serve::routes::DEVICE_ID_HEADER,
        body.len()
    );
    send_raw(harness, request.into_bytes()).await
}

/// Runs Git for fixture setup without any global configuration.
fn fixture_git(dir: &std::path::Path, words: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(words)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {words:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository in the harness's work folder with one committed file and
/// one unstaged edit.
fn harness_repository(harness: &Harness) -> String {
    let root = std::fs::canonicalize(&harness.work).unwrap();
    fixture_git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Latch Test"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        fixture_git(&root, &["config", key, value]);
    }
    std::fs::write(root.join("README.md"), "one\n").unwrap();
    fixture_git(&root, &["add", "README.md"]);
    fixture_git(&root, &["commit", "-q", "-m", "initial"]);
    std::fs::write(root.join("README.md"), "two\n").unwrap();
    root.to_str().unwrap().to_owned()
}

fn enroll_git_phone(harness: &Harness, fixture: char) -> String {
    remote_access::set_enabled(&harness.home, true).unwrap();
    remote_access::authorize_enrollment(
        &harness.home,
        &format!("enr_{}", fixture.to_string().repeat(32)),
        &fixture.to_string().repeat(64),
        "Git test phone",
        Grant::Control,
        &format!("dev_{}", fixture.to_string().repeat(32)),
    )
    .unwrap()
    .device_id
}

#[tokio::test]
async fn shared_files_are_device_scoped_and_leave_the_legacy_picker_available() {
    let harness = harness().await;
    let owner = enroll_git_phone(&harness, 'a');
    let other = enroll_git_phone(&harness, 'b');
    let roots = "/v2/files/roots";
    for permission in ["observe", "interact", "control"] {
        assert_eq!(
            send(&harness, "GET", roots, Some(permission), "").await.0,
            403
        );
    }

    let work = harness_repository(&harness);
    fixture_git(std::path::Path::new(&work), &["restore", "README.md"]);
    assert!(fixture_git(std::path::Path::new(&work), &["status", "--porcelain"]).is_empty());
    let grant = remote_access::share_root(&harness.home, &owner, &harness.work).unwrap();
    let other_roots = send_as_device(&harness, "GET", roots, &other, "").await;
    assert_eq!(other_roots, (200, serde_json::json!({"roots": []})));
    let visible = send_as_device(&harness, "GET", roots, &owner, "").await;
    assert_eq!(
        visible,
        (
            200,
            serde_json::json!({"roots": [{"rootId": grant.root_id, "name": "work"}]})
        )
    );
    let read = format!("/v2/files/preview?rootId={}&path=README.md", grant.root_id);
    assert_eq!(
        send_as_device(&harness, "GET", &read, &owner, "").await,
        (200, serde_json::json!({"kind": "text", "text": "one\n"}))
    );
    let path = format!("/v2/files/directory?rootId={}", grant.root_id);
    let (status, page) = send_as_device(&harness, "GET", &path, &owner, "").await;
    assert_eq!(status, 200);
    assert_eq!(
        page,
        serde_json::json!({
            "path": "", "entries": [{"name": "README.md", "kind": "file"}], "nextCursor": null
        })
    );
    assert_eq!(
        send_as_device(&harness, "GET", &read, &other, "").await.0,
        404
    );
    remote_access::revoke_shared_root(&harness.home, &grant.root_id).unwrap();
    assert_eq!(
        send_as_device(&harness, "GET", &read, &owner, "").await.0,
        404
    );
    let mut picker = reqwest::Url::parse("http://gateway/v2/directories").unwrap();
    picker.query_pairs_mut().append_pair("path", &work);
    let picker = format!("{}?{}", picker.path(), picker.query().unwrap());
    let (status, page) = send_as_device(&harness, "GET", &picker, &owner, "").await;
    assert_eq!(status, 200);
    assert_eq!(page["path"], work);
}

async fn open_worktree(
    harness: &Harness,
    device: &str,
    path: &std::path::Path,
) -> (u16, serde_json::Value) {
    let mut target = reqwest::Url::parse("http://gateway/v2/git/worktree/open").unwrap();
    target
        .query_pairs_mut()
        .append_pair("path", path.to_str().unwrap());
    send_as_device(
        harness,
        "GET",
        &format!("{}?{}", target.path(), target.query().unwrap()),
        device,
        "",
    )
    .await
}

#[tokio::test]
async fn git_worktree_open_requires_an_exact_root_and_accepts_aliases_and_linked_worktrees() {
    use std::path::Path;

    let harness = harness().await;
    let root = harness_repository(&harness);
    let root = Path::new(&root);
    let device = enroll_git_phone(&harness, 'a');
    let nested = root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let bare = root.with_file_name("bare.git");
    std::fs::create_dir(&bare).unwrap();
    fixture_git(&bare, &["init", "--bare", "-q"]);
    let alias = root.with_file_name("source alias");
    std::os::unix::fs::symlink(root, &alias).unwrap();
    let linked = root.with_file_name("linked");
    fixture_git(
        root,
        &[
            "worktree",
            "add",
            "-qb",
            "secondary",
            linked.to_str().unwrap(),
        ],
    );

    for (path, code) in [
        (nested.as_path(), "worktree_root_required"),
        (alias.join("nested").as_path(), "worktree_root_required"),
        (bare.as_path(), "bare_repository"),
    ] {
        let (status, reply) = open_worktree(&harness, &device, path).await;
        assert_eq!((status, reply["error"].as_str()), (422, Some(code)));
    }
    let legacy_path = nested.to_str().unwrap().replace('/', "%2F");
    let (status, reply) = send(
        &harness,
        "GET",
        &format!("/v2/git/status?path={legacy_path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["root"], root.to_str().unwrap());

    for (source, expected_root, kind, branch) in [
        (&alias, root, "repository", "main"),
        (&linked, linked.as_path(), "linked_worktree", "secondary"),
    ] {
        let (status, opened) = open_worktree(&harness, &device, source).await;
        assert_eq!(status, 200, "{opened}");
        assert_eq!(
            opened["repository"]["root"],
            expected_root.to_str().unwrap()
        );
        assert_eq!(opened["repository"]["kind"], kind);
        let reference = opened["worktreeRef"].as_str().unwrap();
        let target = format!("/v2/git/worktree/status?worktreeRef={reference}");
        let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["root"], expected_root.to_str().unwrap());
        assert_eq!(reply["head"]["branch"], branch);
        if source == &alias {
            std::fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink(&linked, &alias).unwrap();
            let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
            assert_eq!(status, 409, "{reply}");
            assert_eq!(reply["error"], "stale_worktree_ref");
        }
    }
}

#[tokio::test]
async fn git_worktree_pages_recheck_checkout_device_host_and_grant() {
    use std::path::Path;

    let harness = harness().await;
    let other_host = self::harness().await;
    let root = harness_repository(&harness);
    let root = Path::new(&root);
    let device = enroll_git_phone(&harness, 'a');
    let other_device = enroll_git_phone(&harness, 'b');
    // Even the same local pairing ID cannot move a reference across gateways.
    remote_access::set_enabled(&other_host.home, true).unwrap();
    let devices_file = "remote-access/devices.json";
    std::fs::copy(
        harness.home.root().join(devices_file),
        other_host.home.root().join(devices_file),
    )
    .unwrap();

    let text = |edited: bool| {
        (0..1800)
            .map(|line| {
                let prefix = if edited && line % 12 == 0 {
                    "new"
                } else {
                    "old"
                };
                format!("{prefix} {line} {}\n", "x".repeat(512))
            })
            .collect::<String>()
    };
    let file = root.join("README.md");
    std::fs::write(&file, text(false)).unwrap();
    fixture_git(root, &["commit", "-qam", "large diff base"]);
    std::fs::write(&file, text(true)).unwrap();

    let (status, opened) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{opened}");
    let reference = opened["worktreeRef"].as_str().unwrap();
    let diff = |reference: &str| {
        format!(
            "/v2/git/worktree/diff?worktreeRef={reference}&file=524541444d452e6d64&area=unstaged"
        )
    };
    let first = diff(reference);
    for grant in ["observe", "interact", "control"] {
        let (status, _) = send(&harness, "GET", &first, Some(grant), "").await;
        assert_eq!(status, 403);
    }
    let (status, page) = send_as_device(&harness, "GET", &first, &device, "").await;
    assert_eq!(status, 200, "{page}");
    let cursor = page["nextCursor"].as_str().expect("second page");
    let next = format!("{first}&cursor={cursor}");
    let (status, page) = send_as_device(&harness, "GET", &next, &device, "").await;
    assert_eq!(status, 200, "{page}");
    assert!(page["hunks"][0]["index"].as_u64().unwrap() > 0);
    for (host, phone) in [(&harness, &other_device), (&other_host, &device)] {
        let (status, reply) = send_as_device(host, "GET", &next, phone, "").await;
        assert_eq!(status, 409, "{reply}");
        assert_eq!(reply["error"], "stale_worktree_ref");
    }

    let (status, reopened) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{reopened}");
    let new_first = diff(reopened["worktreeRef"].as_str().unwrap());
    let (status, reply) = send_as_device(
        &harness,
        "GET",
        &format!("{new_first}&cursor={cursor}"),
        &device,
        "",
    )
    .await;
    assert_eq!(status, 409, "{reply}");
    assert_eq!(reply["error"], "stale_cursor");

    remote_access::grant(&harness.home, &device, Grant::Observe).unwrap();
    let (status, reply) = send_as_device(&harness, "GET", &next, &device, "").await;
    assert_eq!(status, 403, "{reply}");
    remote_access::grant(&harness.home, &device, Grant::Control).unwrap();
    let (status, reply) = send_as_device(&harness, "GET", &next, &device, "").await;
    assert_eq!(status, 409, "{reply}");
    assert_eq!(reply["error"], "stale_worktree_ref");

    let (status, reopened) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{reopened}");
    let reference = reopened["worktreeRef"].as_str().unwrap();
    let before = root.with_file_name("original");
    std::fs::rename(root, &before).unwrap();
    std::fs::create_dir(root).unwrap();
    fixture_git(root, &["init", "-qb", "replacement"]);
    for target in [
        diff(reference),
        format!("/v2/git/worktree/status?worktreeRef={reference}"),
    ] {
        let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
        assert_eq!(status, 409, "{reply}");
        assert_eq!(reply["error"], "stale_worktree_ref");
    }
    let (status, replacement) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{replacement}");
    let reference = replacement["worktreeRef"].as_str().unwrap();
    remote_access::revoke(&harness.home, &device).unwrap();
    let (status, reply) = send_as_device(&harness, "GET", &diff(reference), &device, "").await;
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"], "worktree_access_denied");
    let (status, reply) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"], "worktree_access_denied");
}

#[tokio::test]
async fn remote_reviews_require_the_original_paired_worktree() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let root = std::path::Path::new(&root);
    let device = enroll_git_phone(&harness, 'a');
    let other = enroll_git_phone(&harness, 'b');
    let (_, opened) = open_worktree(&harness, &device, root).await;
    let reference = opened["worktreeRef"].as_str().unwrap();
    for route in ["/v2/git/branches", "/v2/git/sync", "/v2/git/push/preview"] {
        let target = format!("{route}?worktreeRef={reference}");
        let (status, data) = send_as_device(&harness, "GET", &target, &device, "").await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(data["root"], root.canonicalize().unwrap().to_str().unwrap());
        assert!(data["preconditions"].is_object());
        let (status, data) = send_as_device(&harness, "GET", &target, &other, "").await;
        assert_eq!(status, 409, "{data}");
        assert_eq!(data["error"], "stale_worktree_ref");
        let (status, _) = send(&harness, "GET", &target, Some("control"), "").await;
        assert_eq!(status, 403);
        let (status, _) =
            send_as_device(&harness, "GET", &format!("{target}&path=/tmp"), &device, "").await;
        assert_eq!(status, 400);
    }
    let target = format!(
        "/v2/git/sync?worktreeRef={reference}&cursor={}:v2",
        "0".repeat(64)
    );
    let (status, data) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 409);
    assert_eq!(data["error"], "stale_cursor");
    std::fs::rename(root, root.with_file_name("original")).unwrap();
    std::fs::create_dir(root).unwrap();
    fixture_git(root, &["init", "-qb", "replacement"]);
    let target = format!("/v2/git/push/preview?worktreeRef={reference}");
    let (status, data) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 409, "{data}");
    assert_eq!(data["error"], "stale_worktree_ref");
}

#[tokio::test]
async fn scoped_push_review_reaches_only_the_reviewed_disposable_remote_branch() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let root = std::path::Path::new(&root);
    let remote = root.with_file_name("disposable-remote.git");
    std::fs::create_dir(&remote).unwrap();
    fixture_git(&remote, &["init", "--bare", "-qb", "main"]);
    fixture_git(root, &["remote", "add", "origin", remote.to_str().unwrap()]);
    let device = enroll_git_phone(&harness, 'a');
    let (_, opened) = open_worktree(&harness, &device, root).await;
    let reference = opened["worktreeRef"].as_str().unwrap();
    let preview_route = format!(
        "/v2/git/push/preview?worktreeRef={reference}&remote=origin&destination=review%2Btest"
    );
    let (status, preview) = send_as_device(&harness, "GET", &preview_route, &device, "").await;
    assert_eq!(status, 200, "{preview}");
    assert!(preview["blockedReason"].is_null(), "{preview}");
    assert_eq!(preview["source"]["branch"], "main");
    assert_eq!(
        preview["target"]["destinationRef"],
        "refs/heads/review+test"
    );
    assert!(!remote.join("refs/heads/review+test").exists());
    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let body = serde_json::json!({
        "requestId": id,
        "path": format!("worktree:{reference}"),
        "expected": preview["preconditions"],
        "remote": preview["target"]["remote"],
        "remoteUrl": preview["target"]["remoteUrl"],
        "destination": preview["target"]["destination"],
        "sourceBranch": preview["source"]["branch"],
        "sourceOid": preview["source"]["oid"],
        "destinationOid": preview["target"]["destinationOid"],
    });
    let (status, mut receipt) =
        send_as_device(&harness, "POST", "/v2/git/push", &device, &body.to_string()).await;
    assert!(matches!(status, 200 | 202), "{receipt}");
    let lookup = format!("/v2/git/receipts/{id}?worktreeRef={reference}");
    for _ in 0..100 {
        if receipt["state"] != "pending" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (status, next) = send_as_device(&harness, "GET", &lookup, &device, "").await;
        assert!(matches!(status, 200 | 202), "{next}");
        receipt = next;
    }
    assert_eq!(receipt["state"], "completed", "{receipt}");
    let written = std::fs::read_to_string(remote.join("refs/heads/review+test")).unwrap();
    assert_eq!(written.trim(), preview["source"]["oid"].as_str().unwrap());
    assert!(!remote.join("refs/heads/main").exists());
}

#[tokio::test]
async fn scoped_write_receipts_survive_a_new_gateway_without_reopening_or_replaying_git() {
    use std::path::Path;

    let mut harness = harness().await;
    let root = harness_repository(&harness);
    let root = Path::new(&root);
    let device = enroll_git_phone(&harness, 'a');
    let other = enroll_git_phone(&harness, 'b');
    let (_, opened) = open_worktree(&harness, &device, root).await;
    let reference = opened["worktreeRef"].as_str().unwrap();
    let status_route = format!("/v2/git/worktree/status?worktreeRef={reference}");
    let (_, status) = send_as_device(&harness, "GET", &status_route, &device, "").await;
    let request_id = "00000000-0000-4000-8000-000000000001";
    let body = serde_json::json!({
        "requestId": request_id,
        "path": format!("worktree:{reference}"),
        "expected": status["preconditions"],
        "all": ["unstaged"]
    });
    let stage = "/v2/git/stage";
    let (status, reply) = send_as_device(&harness, "POST", stage, &other, &body.to_string()).await;
    assert_eq!(status, 409, "{reply}");
    assert_eq!(reply["error"], "stale_worktree_ref");
    // The caller loses the initial response after the host accepts it.
    let (status, accepted) =
        send_as_device(&harness, "POST", stage, &device, &body.to_string()).await;
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["state"], "completed");

    let restarted = harness._dir.path().join("restarted-gateway");
    std::fs::create_dir(&restarted).unwrap();
    harness.address = start_harness_router(&restarted, &harness.home).await;
    let (status, _) = send_as_device(&harness, "GET", &status_route, &device, "").await;
    assert_eq!(status, 409, "read reference is instance-ephemeral");
    let receipt = format!("/v2/git/receipts/{request_id}");
    let scoped_receipt = format!("{receipt}?worktreeRef={reference}");
    let (status, recovered) = send_as_device(&harness, "GET", &scoped_receipt, &device, "").await;
    assert_eq!((status, recovered), (200, accepted.clone()));
    let (status, _) = send_as_device(&harness, "GET", &scoped_receipt, &other, "").await;
    assert_eq!(status, 404);
    let (status, _) = send_as_device(&harness, "GET", &receipt, &device, "").await;
    assert_eq!(status, 409);

    // Both a different worktree reference and changed preconditions conflict.
    let (_, reopened) = open_worktree(&harness, &device, root).await;
    let mut changed = body.clone();
    changed["path"] = format!("worktree:{}", reopened["worktreeRef"].as_str().unwrap()).into();
    let (status, reply) =
        send_as_device(&harness, "POST", stage, &device, &changed.to_string()).await;
    assert_eq!(status, 409);
    assert_eq!(reply["error"], "request_id_conflict");
    changed = body.clone();
    changed["expected"]["index"] = "f".repeat(64).into();
    let (status, reply) =
        send_as_device(&harness, "POST", stage, &device, &changed.to_string()).await;
    assert_eq!(status, 409);
    assert_eq!(reply["error"], "request_id_conflict");

    fixture_git(root, &["reset", "-q", "HEAD"]);
    let (status, replay) =
        send_as_device(&harness, "POST", stage, &device, &body.to_string()).await;
    assert_eq!((status, replay), (200, accepted));
    assert!(fixture_git(root, &["diff", "--cached", "--name-only"]).is_empty());

    std::fs::rename(root, root.with_file_name("original-worktree")).unwrap();
    std::fs::create_dir(root).unwrap();
    fixture_git(root, &["init", "-qb", "replacement"]);
    changed = body.clone();
    changed["requestId"] = "00000000-0000-4000-8000-000000000002".into();
    changed["path"] = format!("worktree:{}", reopened["worktreeRef"].as_str().unwrap()).into();
    let (status, reply) =
        send_as_device(&harness, "POST", stage, &device, &changed.to_string()).await;
    assert_eq!(status, 409);
    assert_eq!(reply["error"], "stale_worktree_ref");
    let (status, recovered) = send_as_device(&harness, "GET", &scoped_receipt, &device, "").await;
    assert_eq!(
        (status, recovered["state"].as_str()),
        (200, Some("completed"))
    );

    remote_access::grant(&harness.home, &device, Grant::Observe).unwrap();
    let (status, _) = send_as_device(&harness, "GET", &scoped_receipt, &device, "").await;
    assert_eq!(status, 403);
    remote_access::grant(&harness.home, &device, Grant::Control).unwrap();
    let (status, _) = send_as_device(&harness, "GET", &scoped_receipt, &device, "").await;
    assert_eq!(status, 409);
}

#[tokio::test]
async fn git_worktree_summaries_keep_available_rows_and_leave_recents_unchanged() {
    use std::path::Path;

    let harness = harness().await;
    let root = harness_repository(&harness);
    let device = enroll_git_phone(&harness, 'a');
    let other_device = enroll_git_phone(&harness, 'b');
    let root = Path::new(&root);
    let linked = root.with_file_name("linked");
    fixture_git(
        root,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    fixture_git(root, &["branch", "tracking"]);
    fixture_git(root, &["branch", "--set-upstream-to", "tracking", "main"]);
    fixture_git(root, &["commit", "--allow-empty", "-qm", "ahead"]);
    fixture_git(root, &["add", "README.md"]);
    std::fs::write(root.join("README.md"), "three\n").unwrap();
    std::fs::write(root.join("private-file"), "").unwrap();
    let recent = harness.home.root().join("git/recent-repositories.json");
    std::fs::create_dir_all(recent.parent().unwrap()).unwrap();
    let saved = br#"{"formatVersion":1,"repositories":[]}"#;
    std::fs::write(&recent, saved).unwrap();

    let (status, opened) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{opened}");
    let valid = opened["worktreeRef"].as_str().unwrap();
    let (status, opened) = open_worktree(&harness, &device, &linked).await;
    assert_eq!(status, 200, "{opened}");
    let unavailable = opened["worktreeRef"].as_str().unwrap();
    let (status, opened) = open_worktree(&harness, &other_device, root).await;
    assert_eq!(status, 200, "{opened}");
    let foreign = opened["worktreeRef"].as_str().unwrap();
    let clean_target = format!("/v2/git/worktree/summaries?worktreeRefs={unavailable}");
    let (status, reply) = send_as_device(&harness, "GET", &clean_target, &device, "").await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(
        reply["results"][0]["summary"],
        serde_json::json!({
            "branch": null, "ahead": null, "behind": null, "staged": 0,
            "unstaged": 0, "conflicted": 0, "untracked": 0, "changesComplete": true
        })
    );
    std::fs::remove_dir_all(&linked).unwrap();

    let target = format!(
        "/v2/git/worktree/summaries?worktreeRefs={unavailable},{valid},bad,{foreign},{valid}"
    );
    let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 200, "{reply}");
    let ok = serde_json::json!({"outcome": "ok", "summary": {
        "branch": "main", "ahead": 1, "behind": 0, "staged": 1, "unstaged": 1,
        "conflicted": 0, "untracked": 1, "changesComplete": true
    }});
    let stale = serde_json::json!({"outcome": "error", "error": "stale_worktree_ref"});
    assert_eq!(
        reply["results"],
        serde_json::json!([
            stale, ok, {"outcome": "error", "error": "invalid_request"}, stale, ok
        ])
    );
    assert_eq!(std::fs::read(&recent).unwrap(), saved);

    let (status, _) = send(&harness, "GET", &target, Some("control"), "").await;
    assert_eq!(
        status, 403,
        "a local control token has no paired-device identity"
    );
    remote_access::grant(&harness.home, &device, Grant::Observe).unwrap();
    let (status, _) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn git_worktree_summaries_bound_the_batch_and_report_partial_change_counts() {
    use std::path::Path;

    let harness = harness().await;
    let root = harness_repository(&harness);
    let root = Path::new(&root);
    // With the existing unstaged edit, this makes exactly 1,000 records.
    for index in 0..999 {
        std::fs::write(root.join(format!("untracked-{index:04}")), "").unwrap();
    }
    let device = enroll_git_phone(&harness, 'a');
    let (status, opened) = open_worktree(&harness, &device, root).await;
    assert_eq!(status, 200, "{opened}");
    let reference = opened["worktreeRef"].as_str().unwrap();
    let base = "/v2/git/worktree/summaries?worktreeRefs=";
    for refs in [String::new(), [reference; 9].join(",")] {
        let (status, reply) =
            send_as_device(&harness, "GET", &format!("{base}{refs}"), &device, "").await;
        assert_eq!(status, 400, "{reply}");
        assert_eq!(reply["error"], "invalid_request");
    }
    let refs = [reference; 8].join(",");
    let target = format!("{base}{refs}");
    let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 200, "{reply}");
    let results = reply["results"].as_array().unwrap();
    assert_eq!(results.len(), 8);
    for result in results {
        assert_eq!(result["summary"]["changesComplete"], true);
        assert_eq!(result["summary"]["untracked"], 999);
    }

    std::fs::write(root.join("extra"), "").unwrap();
    let target = format!("{base}{reference}");
    let (status, reply) = send_as_device(&harness, "GET", &target, &device, "").await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["results"][0]["summary"]["changesComplete"], false);
    assert_eq!(reply["results"][0]["summary"]["untracked"], 999);
}

const GIT_MUTATIONS: [&str; 13] = [
    "/v2/git/stage",
    "/v2/git/unstage",
    "/v2/git/commit",
    "/v2/git/branches/create",
    "/v2/git/branches/switch",
    "/v2/git/fetch",
    "/v2/git/fast-forward",
    "/v2/git/merge",
    "/v2/git/rebase",
    "/v2/git/continue",
    "/v2/git/abort",
    "/v2/git/push",
    "/v2/git/push/reconcile",
];

/// Every Git mutation, the branch list and the receipt lookup are refused
/// below control on the loopback router, before the body is read or any
/// folder resolved, and are not reachable with another method.
#[tokio::test]
async fn git_mutations_are_refused_below_the_control_grant() {
    let harness = harness().await;
    for target in GIT_MUTATIONS {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "POST", target, Some(grant), "{}").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
        let (status, body) = send(&harness, "POST", target, Some("control"), "{}").await;
        assert_eq!(
            status, 400,
            "control reaches {target} and its body is checked: {body}"
        );
        let (status, _) = send(&harness, "GET", target, Some("control"), "").await;
        assert!(status == 404 || status == 405, "GET {target} -> {status}");
    }
    let work = harness.work.to_str().unwrap().replace('/', "%2F");
    for target in [
        format!("/v2/git/branches?path={work}"),
        format!("/v2/git/sync?path={work}"),
        format!("/v2/git/push/preview?path={work}"),
        "/v2/git/receipts/0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e".to_owned(),
    ] {
        for grant in ["observe", "interact"] {
            let (status, _) = send(&harness, "GET", &target, Some(grant), "").await;
            assert_eq!(status, 403, "{grant} must not reach {target}");
        }
    }
}

/// A mutation is shaped and typed before anything is accepted: no free-form
/// field, no Git option, no path outside the working tree.
#[tokio::test]
async fn git_mutation_bodies_are_checked_before_acceptance() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let expected = serde_json::json!({
        "branch": "main",
        "head": "0".repeat(40),
        "index": "0".repeat(64),
        "worktree": "0".repeat(64),
    });
    let file = |key: &str, area: &str| serde_json::json!({"pathKey": key, "area": area, "fingerprint": "0".repeat(32)});
    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let cases = [
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": "not-a-uuid", "path": root, "expected": expected, "files": [file("61", "unstaged")]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "unstaged")], "args": ["--force"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "unstaged")], "all": ["untracked"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "all": ["conflicted"]}),
        ),
        (
            "/v2/git/unstage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("61", "staged")], "confirmConflictMarkers": true}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "--upload-pack=touch /tmp/x"}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin main"}),
        ),
        (
            "/v2/git/fetch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "refspec": "+refs/*:refs/*"}),
        ),
        (
            "/v2/git/fast-forward",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "origin/main"}),
        ),
        (
            "/v2/git/merge",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "-Xours", "upstreamOid": "0".repeat(40)}),
        ),
        (
            "/v2/git/merge",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "0".repeat(40), "strategy": "ours"}),
        ),
        (
            "/v2/git/rebase",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "upstream": "origin/main", "upstreamOid": "0".repeat(40), "interactive": true}),
        ),
        (
            "/v2/git/continue",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "cherry_pick"}),
        ),
        (
            "/v2/git/abort",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "rebase", "confirmed": true, "hard": true}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("2e2e2f6574632f706173737764", "unstaged")]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "files": [file("2f6574632f706173737764", "unstaged")]}),
        ),
        (
            "/v2/git/unstage",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "all": ["unstaged"]}),
        ),
        (
            "/v2/git/stage",
            serde_json::json!({"requestId": id, "path": root, "expected": {"branch": "main", "head": "HEAD", "index": "x", "worktree": "y"}, "all": ["unstaged"]}),
        ),
        (
            "/v2/git/commit",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "two\nlines"}),
        ),
        (
            "/v2/git/commit",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "   "}),
        ),
        (
            "/v2/git/branches/create",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "--force", "startPoint": "HEAD", "startOid": "0".repeat(40), "switch": false}),
        ),
        (
            "/v2/git/branches/create",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "ok", "startPoint": "origin/main~3", "startOid": "0".repeat(40), "switch": false}),
        ),
        (
            "/v2/git/branches/switch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "@{-1}", "oid": "0".repeat(40)}),
        ),
        (
            "/v2/git/branches/switch",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "name": "main", "oid": "HEAD"}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null, "force": true}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "--receive-pack=touch /tmp/x", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "-f", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main:refs/tags/x", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "HEAD", "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "--all", "sourceOid": "0".repeat(40), "destinationOid": null}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": "origin/main"}),
        ),
        (
            "/v2/git/push",
            serde_json::json!({"requestId": id, "path": root, "expected": expected, "remote": "origin", "remoteUrl": null, "destination": "main", "sourceBranch": "main", "sourceOid": "0".repeat(40), "destinationOid": null, "refspec": "+refs/heads/*:refs/heads/*"}),
        ),
        (
            "/v2/git/push/reconcile",
            serde_json::json!({"requestId": "not-a-uuid"}),
        ),
        (
            "/v2/git/push/reconcile",
            serde_json::json!({"requestId": id, "push": true}),
        ),
    ];
    for (target, body) in cases {
        let (status, reply) =
            send(&harness, "POST", target, Some("control"), &body.to_string()).await;
        assert_eq!(status, 400, "{target} {body}: {reply}");
        assert_eq!(reply["error"], "invalid_request");
    }
    // The push preview takes no option or revision expression either.
    let folder = root.replace('/', "%2F");
    for query in [
        "remote=--upload-pack%3Dtouch%20%2Ftmp%2Fx&destination=main",
        "remote=origin&destination=main~1",
        "remote=origin&destination=HEAD",
        "remote=origin",
        "destination=main",
    ] {
        let target = format!("/v2/git/push/preview?path={folder}&{query}");
        let (status, reply) = send(&harness, "GET", &target, Some("control"), "").await;
        assert_eq!(status, 400, "{target}: {reply}");
        assert_eq!(reply["error"], "invalid_request");
    }
    // Abort is refused unless the user confirmed it.
    for body in [
        serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "merge"}),
        serde_json::json!({"requestId": id, "path": root, "expected": expected, "operation": "merge", "confirmed": false}),
    ] {
        let (status, reply) = send(
            &harness,
            "POST",
            "/v2/git/abort",
            Some("control"),
            &body.to_string(),
        )
        .await;
        assert_eq!(status, 400, "{body}: {reply}");
        assert_eq!(reply["error"], "confirmation_required");
    }
    // Nothing above was accepted.
    let (status, reply) = send(
        &harness,
        "GET",
        &format!("/v2/git/receipts/{id}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 404, "{reply}");
    assert_eq!(reply["error"], "receipt_not_found");

    let large = serde_json::json!({"requestId": id, "path": root, "expected": expected, "summary": "x", "description": "y".repeat(40 * 1024)});
    let (status, _) = send(
        &harness,
        "POST",
        "/v2/git/commit",
        Some("control"),
        &large.to_string(),
    )
    .await;
    assert_eq!(status, 413);
}

/// Through the real router: a stage is accepted for one device, replayed by
/// the same id without running again, refused when the id is reused for
/// something else, and its receipt is invisible to every other device.
#[tokio::test]
async fn git_receipts_are_scoped_to_the_device_through_the_router() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let encoded = root.replace('/', "%2F");
    let (status, status_body) = send_as_device(
        &harness,
        "GET",
        &format!("/v2/git/status?path={encoded}"),
        "phone-a",
        "",
    )
    .await;
    assert_eq!(status, 200, "{status_body}");
    let readme = status_body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "README.md")
        .unwrap()
        .clone();
    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let body = serde_json::json!({
        "requestId": id,
        "path": root,
        "expected": status_body["preconditions"],
        "files": [{"pathKey": readme["pathKey"], "area": "unstaged", "fingerprint": readme["fingerprint"]}],
    })
    .to_string();
    let (status, first) = send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &body).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["state"], "completed");
    assert_eq!(first["outcome"]["code"], "staged");

    let (status, replay) =
        send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &body).await;
    assert_eq!(status, 200);
    assert_eq!(replay, first);

    let reused = body.replace("\"area\":\"unstaged\"", "\"area\":\"untracked\"");
    assert_ne!(reused, body);
    let (status, conflict) =
        send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &reused).await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["error"], "request_id_conflict");

    let lookup = format!("/v2/git/receipts/{id}");
    let (status, mine) = send_as_device(&harness, "GET", &lookup, "phone-a", "").await;
    assert_eq!(status, 200);
    assert_eq!(mine, first);
    let (status, theirs) = send_as_device(&harness, "GET", &lookup, "phone-b", "").await;
    assert_eq!(status, 404, "{theirs}");
    assert_eq!(theirs["error"], "receipt_not_found");
    let (status, _) = send(&harness, "GET", &lookup, Some("control"), "").await;
    assert_eq!(status, 404);
}

/// The sync preview is a read at control: it serves what the last fetch
/// left (here, a repository with no remote) and refuses a cursor that no
/// longer describes the upstream.
#[tokio::test]
async fn git_sync_previews_without_fetching_and_maps_a_stale_cursor() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let path = root.replace('/', "%2F");
    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/sync?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["relation"], "no_upstream");
    assert_eq!(body["upstream"], serde_json::Value::Null);
    assert_eq!(body["incoming"], serde_json::json!([]));
    assert!(body["fastForwardBlockedReason"].is_string());
    assert!(body["preconditions"]["index"].is_string());

    let (status, body) = send(
        &harness,
        "GET",
        &format!("/v2/git/sync?path={path}&cursor=v1-{}-50", "a".repeat(40)),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "stale_cursor");
}

/// The push preview is a read at control that never contacts a remote (here
/// there is none to contact); a push check is scoped to the device that
/// sent the push and is only for pushes.
#[tokio::test]
async fn git_push_preview_and_checks_through_the_router() {
    let harness = harness().await;
    let root = harness_repository(&harness);
    let path = root.replace('/', "%2F");
    let (status, preview) = send(
        &harness,
        "GET",
        &format!("/v2/git/push/preview?path={path}"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["source"]["branch"], "main");
    assert_eq!(preview["target"], serde_json::Value::Null);
    assert_eq!(preview["selectionRequired"], false);
    assert!(preview["blockedReason"]
        .as_str()
        .unwrap()
        .contains("no remotes"));
    let (status, stale) = send(
        &harness,
        "GET",
        &format!("/v2/git/push/preview?path={path}&cursor=p1-a-b-50"),
        Some("control"),
        "",
    )
    .await;
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["error"], "stale_cursor");

    let id = "0b7c2f4e-9a7d-4a8e-9c1e-2f3a4b5c6d7e";
    let check = serde_json::json!({ "requestId": id }).to_string();
    let (status, missing) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-a",
        &check,
    )
    .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["error"], "receipt_not_found");

    // A receipt that is not a push cannot be checked against a remote, and
    // another device cannot even find it.
    let (_, status_body) = send_as_device(
        &harness,
        "GET",
        &format!("/v2/git/status?path={path}"),
        "phone-a",
        "",
    )
    .await;
    let readme = status_body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "README.md")
        .unwrap()
        .clone();
    let stage = serde_json::json!({
        "requestId": id,
        "path": root,
        "expected": status_body["preconditions"],
        "files": [{"pathKey": readme["pathKey"], "area": "unstaged", "fingerprint": readme["fingerprint"]}],
    })
    .to_string();
    let (status, _) = send_as_device(&harness, "POST", "/v2/git/stage", "phone-a", &stage).await;
    assert_eq!(status, 200);
    let (status, not_push) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-a",
        &check,
    )
    .await;
    assert_eq!(status, 409, "{not_push}");
    assert_eq!(not_push["error"], "not_a_push");
    let (status, theirs) = send_as_device(
        &harness,
        "POST",
        "/v2/git/push/reconcile",
        "phone-b",
        &check,
    )
    .await;
    assert_eq!(status, 404, "{theirs}");
    assert_eq!(theirs["error"], "receipt_not_found");
}
