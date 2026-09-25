//! Protocol-major-2 HTTP and WebSocket gateway.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path as FsPath;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;

use super::attachments::{self, AttachmentError};
use super::attention::AttentionWatcher;
use super::auth::{
    load_token, origin_allowed, presented_token, selected_subprotocol, token_matches,
};
use super::contract::{
    CreateSessionRequest, GatewayFeatures, GatewayReadiness, SessionAgent,
    OPERATION_RETENTION_SECONDS, REMOTE_ACCESS_SCHEMA_VERSION,
};
use super::conversation::{self, ConversationConnect, ConversationQuery};
use super::directory::{self, BrowseError};
use super::routes::{
    route_for, Grant, RouteId, RouteSpec, ATTACHMENT_MAX_BYTES, DEVICE_GRANT_HEADER,
    DEVICE_ID_HEADER, ROUTES,
};
use super::terminal::{self, ResumeRegistry, TerminalConnect, TerminalQuery};
use super::ServeOptions;
use crate::cli::attach::SessionLookupError;
use crate::cli::create::{self, RemoteSessionError, RemoteSessionRequest};
use crate::cli::json::{CapabilitiesReport, CreateReport, CreatedSession};
use crate::cli::manage::{self, InspectOptions, ListOptions, StopRequest};
use crate::conversation::ConversationHub;
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

#[derive(Clone)]
struct AppState {
    home: LatchHome,
    token_file: std::path::PathBuf,
    latch_bin: std::path::PathBuf,
    gateway_instance_id: String,
    /// Also keeps the exclusive Hub writer lock alive for the gateway lifetime.
    conversation_hub: ConversationHub,
    /// Bounded resume grants for terminal surfaces handed to remote devices.
    terminal_resumes: ResumeRegistry,
    /// Gateway-owned attention producer for watched sessions.
    attention: AttentionWatcher,
}

/// Opaque device identity the loopback proxy proved for this request, or
/// `None` for a local client. Receipts and resume grants are scoped to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceContext(pub Option<String>);

fn is_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code: "request_failed",
            message: message.into(),
        }
    }

    fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.code, "reason": self.message })),
        )
            .into_response()
    }
}

/// Binds and serves until SIGINT/SIGTERM.
pub async fn run(options: ServeOptions) -> anyhow::Result<()> {
    let gateway_instance_id = gateway_instance_id();
    let connector_home = options.home.clone();
    let conversation_hub = ConversationHub::with_connector_factory(
        options.home.root().to_owned(),
        std::sync::Arc::new(move |id| {
            crate::conversation::connector_for_session(connector_home.clone(), id)
        }),
    )?;
    let attention = AttentionWatcher::new(options.home.clone(), conversation_hub.clone());
    tokio::spawn(attention.clone().run());
    let state = AppState {
        home: options.home,
        token_file: options.token_file,
        latch_bin: options.latch_bin,
        gateway_instance_id: gateway_instance_id.clone(),
        conversation_hub,
        terminal_resumes: ResumeRegistry::default(),
        attention,
    };
    let app = router(state);

    let listener = TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("cannot bind {}", options.bind))?;
    let addr = listener.local_addr()?;
    if let Some(path) = options.ready_file.as_deref() {
        write_readiness(
            path,
            &GatewayReadiness {
                format_version: REMOTE_ACCESS_SCHEMA_VERSION,
                address: addr.to_string(),
                url: format!("http://{addr}"),
                protocol_version: crate::engine::PROTOCOL_VERSION,
                gateway_instance_id,
            },
        )?;
    }
    eprintln!("latch serve listening on {addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("serve failed")
}

fn router(state: AppState) -> Router {
    let mut router = Router::<AppState>::new();
    for spec in ROUTES {
        router = register(router, *spec);
    }
    router
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .layer(middleware::from_fn(add_cors))
        .with_state(state)
}

/// Builds the production router, including the token and grant middleware, for
/// tests that need a real socket rather than a hand-rolled handler.
///
/// `latch_bin` is what the terminal route spawns as its attach client, so a
/// test can substitute a stub that behaves like one — including exiting with
/// the kernel's release codes — without starting a real daemon.
#[cfg(test)]
pub(crate) fn test_router(
    home: LatchHome,
    token_file: std::path::PathBuf,
    conversation_hub: ConversationHub,
    latch_bin: std::path::PathBuf,
) -> Router {
    let attention = AttentionWatcher::new(home.clone(), conversation_hub.clone());
    router(AppState {
        home,
        token_file,
        latch_bin,
        gateway_instance_id: "gw-test".to_owned(),
        conversation_hub,
        terminal_resumes: ResumeRegistry::default(),
        attention,
    })
}

fn register(router: Router<AppState>, spec: RouteSpec) -> Router<AppState> {
    match spec.id {
        RouteId::Capabilities => router.route(spec.pattern, get(gateway_capabilities)),
        RouteId::Sessions => router.route(spec.pattern, get(list_sessions)),
        RouteId::CreateSession => router.route(spec.pattern, post(create_session)),
        RouteId::Directories => router.route(spec.pattern, get(browse_directories)),
        RouteId::Session => router.route(spec.pattern, get(inspect_session)),
        RouteId::Preview => router.route(spec.pattern, get(preview_session)),
        RouteId::StopSession => router.route(spec.pattern, post(stop_session)),
        RouteId::Terminal => router.route(spec.pattern, get(terminal_ws)),
        RouteId::Conversation => router.route(spec.pattern, get(conversation_ws)),
        RouteId::Attachments => router.route(spec.pattern, post(upload_attachment)),
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}

async fn add_cors(request: Request<axum::body::Body>, next: Next) -> Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        apply_cors(response.headers_mut(), origin.as_ref());
        return response;
    }
    let mut response = next.run(request).await;
    apply_cors(response.headers_mut(), origin.as_ref());
    response
}

fn apply_cors(headers: &mut HeaderMap, origin: Option<&HeaderValue>) {
    if let Some(origin) = origin {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    } else {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization, Content-Type, Sec-WebSocket-Protocol"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
}

async fn require_token(
    State(state): State<AppState>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiError> {
    if request.method() == Method::OPTIONS {
        return Ok(next.run(request).await);
    }
    if !origin_allowed(request.headers().get(header::ORIGIN)) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "origin not allowed"));
    }
    let expected = load_token(&state.token_file).unwrap_or_default();
    let presented = presented_token(request.headers()).unwrap_or_default();
    if !token_matches(&expected, &presented) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid token"));
    }

    let grant_header = unique_authority_header(request.headers(), DEVICE_GRANT_HEADER)?;
    let device_header = unique_authority_header(request.headers(), DEVICE_ID_HEADER)?;

    // The listener itself is loopback-only (see `refuse_non_loopback`), so a
    // missing `ConnectInfo` (the in-process test router) means loopback. The
    // per-peer check stays as defense in depth against a future bind change.
    let peer_is_loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(true);
    let grant = match grant_header {
        Some(value) if peer_is_loopback => Grant::from_header_value(&value)
            .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid device grant"))?,
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "device grant header is trusted only from the loopback proxy",
            ))
        }
        None if peer_is_loopback => Grant::Control,
        None => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "non-loopback requests require the paired proxy",
            ))
        }
    };
    request.headers_mut().remove(DEVICE_GRANT_HEADER);
    let device = match device_header {
        Some(value) if peer_is_loopback => Some(
            is_device_id(&value)
                .then_some(value)
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid device id"))?,
        ),
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "device id header is trusted only from the loopback proxy",
            ))
        }
        None => None,
    };
    request.headers_mut().remove(DEVICE_ID_HEADER);
    request.extensions_mut().insert(DeviceContext(device));
    let method = request.method().as_str();
    let target = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_else(|| request.uri().path());
    let (_, required) = route_for(method, target)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "route not found"))?;
    if !grant.permits(required) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "device grant does not permit this route",
        ));
    }
    request.extensions_mut().insert(grant);
    Ok(next.run(request).await)
}

fn unique_authority_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Option<String>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        eprintln!("latch serve: duplicate remote authority header ({name})");
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "duplicate device authority header",
        ));
    }
    first
        .map(|value| {
            let text = value.to_str().map_err(|_| {
                ApiError::new(StatusCode::BAD_REQUEST, "invalid device authority header")
            })?;
            if text.contains(['\r', '\n']) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid device authority header",
                ));
            }
            Ok(text.to_owned())
        })
        .transpose()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayCapabilities {
    #[serde(flatten)]
    engine: CapabilitiesReport,
    endpoints: GatewayEndpoints,
    features: GatewayFeatures,
    gateway_instance_id: String,
    operation_retention_seconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayEndpoints {
    sessions: bool,
    preview: bool,
    terminal: bool,
    conversation: bool,
    browse_directories: bool,
    create_session: bool,
    stop_session: bool,
    attachments: bool,
}

async fn gateway_capabilities(State(state): State<AppState>) -> Response {
    Json(GatewayCapabilities {
        engine: manage::capabilities(),
        endpoints: GatewayEndpoints {
            sessions: true,
            preview: true,
            terminal: true,
            conversation: true,
            browse_directories: true,
            create_session: true,
            stop_session: true,
            attachments: true,
        },
        features: GatewayFeatures {
            exclusive_terminal: true,
            session_agents: SESSION_AGENTS.to_vec(),
            attachment_max_bytes: Some(ATTACHMENT_MAX_BYTES),
        },
        gateway_instance_id: state.gateway_instance_id,
        operation_retention_seconds: OPERATION_RETENTION_SECONDS,
    })
    .into_response()
}

async fn list_sessions(State(state): State<AppState>) -> Result<Response, ApiError> {
    let home = state.home.clone();
    let report = tokio::task::spawn_blocking(move || manage::list(ListOptions { home }))
        .await
        .map_err(|_| internal("list sessions"))?
        .map_err(map_engine_error)?;
    Ok(Json(report).into_response())
}

#[derive(serde::Deserialize)]
struct DirectoryQuery {
    path: Option<String>,
    cursor: Option<String>,
}

async fn browse_directories(Query(query): Query<DirectoryQuery>) -> Result<Response, ApiError> {
    let page = tokio::task::spawn_blocking(move || {
        directory::browse(query.path.as_deref(), query.cursor.as_deref())
    })
    .await
    .map_err(|_| internal("browse directories"))?
    .map_err(map_browse_error)?;
    Ok(Json(page).into_response())
}

/// Upper bound on a creation body. The request carries three short strings;
/// the paired proxy already refuses a larger initial request, and this is the
/// same refusal for the manual HTTPS route.
const MAX_CREATE_BODY_BYTES: usize = 1024;

/// Hosted agents the create route will launch when asked. Advertised in
/// discovery so a phone shows only controls this gateway can serve; whether
/// the agent is actually installed is answered at creation time.
const SESSION_AGENTS: &[SessionAgent] = &[SessionAgent::Claude, SessionAgent::Codex];

/// Starts exactly one standard login shell, or one hosted agent, in a
/// validated directory.
///
/// Nothing else about the session is caller-controlled: no argv, environment,
/// shell, display metadata, or terminal type crosses this boundary — an agent
/// is named by kind and resolved on the Mac — and no attach is spawned.
async fn create_session(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    if body.len() > MAX_CREATE_BODY_BYTES {
        return Err(ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "creation request is too large",
        ));
    }
    let request: CreateSessionRequest = serde_json::from_slice(&body).map_err(|_| {
        ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "creation request is malformed",
        )
    })?;
    if !is_request_id(&request.request_id) {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "requestId must be a UUID",
        ));
    }
    if let Some(agent) = request.agent {
        if !SESSION_AGENTS.contains(&agent) {
            return Err(ApiError::coded(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "this gateway does not launch that agent",
            ));
        }
    }
    let cwd = directory::canonical_directory_from_str(&request.cwd).map_err(map_browse_error)?;

    let home = state.home.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        create::create_remote_session(RemoteSessionRequest {
            home,
            request_id: request.request_id,
            cwd,
            agent: request.agent,
            device: device.0,
        })
    })
    .await
    .map_err(|_| internal("create session"))?
    .map_err(map_remote_session_error)?;

    Ok(Json(CreateReport {
        protocol_version: crate::engine::PROTOCOL_VERSION,
        session: CreatedSession {
            id: outcome.id,
            name: outcome.name,
            state: "running".to_owned(),
            created_at: outcome.created_at,
        },
    })
    .into_response())
}

/// Accepts only a canonical hyphenated UUID, the one shape the contract
/// promises and the only shape the phone generates.
fn is_request_id(value: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');
    for expected in groups {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != expected || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

fn map_remote_session_error(error: RemoteSessionError) -> ApiError {
    match error {
        RemoteSessionError::RequestIdConflict => ApiError::coded(
            StatusCode::CONFLICT,
            "request_id_conflict",
            "this request id already created a different session",
        ),
        // Another device owns this id. Saying which would leak that device's
        // existence; the stable code is enough for the phone to stop retrying.
        RemoteSessionError::DeviceConflict => ApiError::coded(
            StatusCode::FORBIDDEN,
            "request_id_foreign",
            "this request id belongs to another device",
        ),
        // Nothing to retry until the agent is installed on the Mac; the
        // message is the sentence the phone shows, and names no path.
        RemoteSessionError::AgentUnavailable(agent) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "agent_unavailable",
            format!(
                "{} is not installed on this Mac, or its login shell cannot find it",
                agent_display_name(agent)
            ),
        ),
        // The engine's failure detail can name paths, binaries, and kernel
        // state. The phone gets the stable code instead.
        RemoteSessionError::Failed(_) => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_creation_failed",
            "the session could not be created",
        ),
    }
}

fn agent_display_name(agent: SessionAgent) -> &'static str {
    match agent {
        SessionAgent::Claude => "Claude Code",
        SessionAgent::Codex => "Codex",
    }
}

fn map_browse_error(error: BrowseError) -> ApiError {
    match error {
        BrowseError::InvalidPath => ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must be an accessible absolute directory path",
        ),
        BrowseError::UnavailablePath => ApiError::coded(
            StatusCode::NOT_FOUND,
            "unavailable_path",
            "directory is unavailable",
        ),
        BrowseError::UnreadableDirectory => ApiError::coded(
            StatusCode::FORBIDDEN,
            "unreadable_directory",
            "directory cannot be read",
        ),
        BrowseError::StaleCursor => ApiError::coded(
            StatusCode::CONFLICT,
            "stale_cursor",
            "directory contents changed; reload the first page",
        ),
    }
}

async fn inspect_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let home = state.home.clone();
    let report =
        tokio::task::spawn_blocking(move || manage::inspect(InspectOptions { home, session: id }))
            .await
            .map_err(|_| internal("inspect session"))?
            .map_err(map_engine_error)?;
    Ok(Json(report).into_response())
}

/// Stops one session's hosted process, leaving its dead pane in place.
///
/// This is `latch stop` reached from a paired device, and it is deliberately
/// the *graceful* form: the engine sends SIGTERM, waits, and escalates to
/// SIGKILL on its own. There is no caller-controlled signal, no force flag,
/// and no removal — a remote device may end what is running, not erase the
/// record of it. Removing a session stays a decision made at the Mac.
///
/// The route is idempotent: stopping a session that has already exited is the
/// same successful answer, which is what lets a phone retry a request whose
/// response it never saw.
async fn stop_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let home = state.home.clone();
    let report = tokio::task::spawn_blocking(move || {
        manage::stop(StopRequest {
            home,
            session: id,
            force: false,
        })
    })
    .await
    .map_err(|_| internal("stop session"))?
    .map_err(map_engine_error)?;
    // The engine escalates to SIGKILL before answering, so a live pane here
    // survived both signals. Saying so as a stable code keeps the phone from
    // reporting a stop that did not happen.
    if !report.stopped {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "session_still_running",
            "the session did not stop",
        ));
    }
    Ok(Json(report).into_response())
}

#[derive(serde::Deserialize)]
struct AttachmentQuery {
    /// Suggested file name. The gateway reduces it to a safe alphabet and
    /// makes it unique; the receipt says what it actually chose.
    name: Option<String>,
}

/// Places one uploaded file under the session's working directory.
///
/// The body is the file itself, streamed to disk as it arrives rather than
/// held in memory. It must declare its length up front, within
/// `ATTACHMENT_MAX_BYTES`, and deliver exactly that many bytes; anything else
/// removes what was written. The directory is the one recorded for the
/// session, never a path from the request.
async fn upload_attachment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<AttachmentQuery>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Response, ApiError> {
    let declared = declared_attachment_length(&headers)?;
    let home = state.home.clone();
    let pending = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let session = manage::resolve_existing(&home, &id).map_err(map_engine_error)?;
        let metadata = crate::session::meta::read(&home.session(&session))
            .map_err(|error| map_engine_error(error.into()))?;
        attachments::create(&metadata.cwd, query.name.as_deref()).map_err(map_attachment_error)
    })
    .await
    .map_err(|_| internal("upload attachment"))??;

    // The pending file removes itself if this handler returns early or is
    // dropped because the phone went away; only `commit` keeps it.
    let file = pending
        .file
        .try_clone()
        .map_err(|_| internal("upload attachment"))?;
    let mut file = tokio::fs::File::from_std(file);
    let written = stream_body(body, &mut file, declared).await?;
    file.sync_all()
        .await
        .map_err(|_| internal("upload attachment"))?;
    drop(file);
    let receipt = pending.commit(written);
    Ok((StatusCode::CREATED, Json(receipt)).into_response())
}

fn declared_attachment_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            ApiError::coded(
                StatusCode::LENGTH_REQUIRED,
                "invalid_request",
                "an attachment must declare its length",
            )
        })?;
    if declared == 0 {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "an attachment cannot be empty",
        ));
    }
    if declared > ATTACHMENT_MAX_BYTES {
        return Err(attachment_too_large());
    }
    Ok(declared)
}

fn attachment_too_large() -> ApiError {
    ApiError::coded(
        StatusCode::PAYLOAD_TOO_LARGE,
        "attachment_too_large",
        format!(
            "attachments are limited to {} MB",
            ATTACHMENT_MAX_BYTES / (1024 * 1024)
        ),
    )
}

/// Copies exactly `declared` bytes of `body` into `file`. More than declared
/// is refused as soon as it arrives; fewer is refused at the end.
async fn stream_body(
    mut body: axum::body::Body,
    file: &mut tokio::fs::File,
    declared: u64,
) -> Result<u64, ApiError> {
    use axum::body::HttpBody;
    use tokio::io::AsyncWriteExt;

    let mut written = 0_u64;
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.map_err(|_| {
            ApiError::coded(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the attachment upload was interrupted",
            )
        })?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        written += data.len() as u64;
        if written > declared {
            return Err(attachment_too_large());
        }
        file.write_all(&data)
            .await
            .map_err(|_| internal("upload attachment"))?;
    }
    if written != declared {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the attachment upload was interrupted",
        ));
    }
    Ok(written)
}

fn map_attachment_error(error: AttachmentError) -> ApiError {
    match error {
        AttachmentError::WorkspaceUnavailable => ApiError::coded(
            StatusCode::CONFLICT,
            "workspace_unavailable",
            "the session's folder is no longer available on this Mac",
        ),
        AttachmentError::UnsafeFolder => ApiError::coded(
            StatusCode::CONFLICT,
            "attachments_folder_unsafe",
            "the attachments folder in this session's folder is not a plain folder",
        ),
        AttachmentError::NameExhausted => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "attachment_failed",
            "the attachment could not be saved",
        ),
        AttachmentError::Io(error) => {
            eprintln!("latch serve: attachment write failed: {error}");
            ApiError::coded(
                StatusCode::INTERNAL_SERVER_ERROR,
                "attachment_failed",
                "the attachment could not be saved",
            )
        }
    }
}

/// Upper bound on the scrollback a preview will read, matching the number
/// `DECISION_SCROLLBACK.md` chose for the same reason: past a screen or two of
/// history nothing is being decided, and the payload is not free.
const PREVIEW_SCROLLBACK_CAP: u32 = 200;

/// A preview is a screen-open convenience. If latchd cannot answer inside this,
/// the phone says so and offers Attach rather than holding a spinner for the
/// 30 seconds the engine helper defaults to.
const PREVIEW_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewQuery {
    #[serde(default)]
    scrollback_lines: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionPreview {
    /// Escape-encoded pane content, drawable by the same renderer as the live
    /// terminal stream.
    content: String,
    cols: u16,
    rows: u16,
    /// True while a full-screen application owns the pane. Scrollback does not
    /// exist there, so a client stops asking for what it cannot receive.
    alternate_screen: bool,
    /// A preview is stale the moment it is taken; a screen showing a still has
    /// to be able to say when it was taken.
    captured_at: String,
    /// Lines of scrollback actually included, after the cap and the alternate
    /// screen have had their say.
    scrollback_lines: u32,
}

/// Reads the live pane without attaching.
///
/// This is the only terminal-shaped route an observing device may call, and it
/// is allowed precisely because `capture-pane` is a query: it does not enter
/// the exclusive surface, so nothing is stolen from whoever holds it.
async fn preview_session(
    Path(id): Path<String>,
    Query(query): Query<PreviewQuery>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let home = state.home.clone();
    let requested = query
        .scrollback_lines
        .unwrap_or(0)
        .min(PREVIEW_SCROLLBACK_CAP);
    let preview = tokio::task::spawn_blocking(move || -> anyhow::Result<SessionPreview> {
        let started = std::time::Instant::now();
        let session = manage::resolve_existing(&home, &id)?;
        let metrics = crate::engine::pane_metrics_with_timeout(
            &home,
            &session,
            PREVIEW_DEADLINE.saturating_sub(started.elapsed()),
        )?;
        // The alternate screen has no history to read, so asking for some
        // would only widen the capture window over the same one screen.
        let scrollback_lines = if metrics.alternate_screen {
            0
        } else {
            requested
        };
        let content = crate::engine::capture_pane_with_timeout(
            &home,
            &session,
            PREVIEW_DEADLINE.saturating_sub(started.elapsed()),
            crate::engine::CapturePaneOptions {
                styled: true,
                scrollback_lines,
            },
        )?;
        // Text snapshots end with a newline after the last row. Fed into an
        // emulator whose grid is exactly `rows` tall, that newline scrolls the
        // whole still up one line and the top row is lost. Measured against
        // every `fixtures/vt` case: with the trailing newline the replayed
        // grid is off by one row, and without it the grid — colors included —
        // is byte-identical to the source pane.
        let content = content
            .strip_suffix('\n')
            .map(str::to_owned)
            .unwrap_or(content);
        Ok(SessionPreview {
            content,
            cols: metrics.cols,
            rows: metrics.rows,
            alternate_screen: metrics.alternate_screen,
            captured_at: crate::engine::format_rfc3339(SystemTime::now()),
            scrollback_lines,
        })
    })
    .await
    .map_err(|_| internal("preview session"))?
    .map_err(map_engine_error)?;
    Ok(Json(preview).into_response())
}

async fn terminal_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(_grant): Extension<Grant>,
    Extension(device): Extension<DeviceContext>,
) -> Response {
    let mut upgrade = ws;
    if let Some(protocol) = selected_subprotocol(&headers) {
        upgrade = upgrade.protocols([protocol]);
    }
    let connect = TerminalConnect {
        home: state.home.clone(),
        latch_bin: state.latch_bin.clone(),
        session: id,
        cols: query.cols,
        rows: query.rows,
        resume: query.resume,
        device: device.0,
        resumes: state.terminal_resumes.clone(),
    };
    upgrade.on_upgrade(move |socket| terminal::run(socket, connect))
}

async fn conversation_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(query): Query<ConversationQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(grant): Extension<Grant>,
    Extension(device): Extension<DeviceContext>,
) -> Response {
    let mut upgrade = ws;
    if let Some(protocol) = selected_subprotocol(&headers) {
        upgrade = upgrade.protocols([protocol]);
    }
    // The proxy authorized this one upgrade; the Hub re-checks the grant on
    // every operation frame that follows.
    let connect = ConversationConnect {
        home: state.home.clone(),
        hub: state.conversation_hub.clone(),
        session: id,
        grant,
        device: device.0,
        attention: Some(state.attention.clone()),
        query,
    };
    upgrade.on_upgrade(move |socket| conversation::run(socket, connect))
}

fn gateway_instance_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("gw-{:x}-{:x}", std::process::id(), nanos)
}

fn write_readiness(path: &FsPath, readiness: &GatewayReadiness) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if parent.exists() {
            let mode = fs::metadata(parent)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                anyhow::bail!(
                    "refusing readiness directory {}: it must be owner-only",
                    parent.display()
                );
            }
        } else {
            fs::create_dir_all(parent).with_context(|| {
                format!("cannot create readiness directory {}", parent.display())
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE)).with_context(
                || format!("cannot tighten readiness directory {}", parent.display()),
            )?;
        }
    }
    let payload = serde_json::to_vec(readiness).context("cannot serialize gateway readiness")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot write readiness file {}", path.display()))?;
    file.write_all(&payload)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn map_engine_error(error: anyhow::Error) -> ApiError {
    if let Some(lookup) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SessionLookupError>())
    {
        // Stable codes, because a client reaching the Mac through a gateway
        // has nothing else to branch on: `latch stop --json` reports "no such
        // session" with an exit status, and this is the same fact on the wire.
        if lookup.is_absent() {
            return ApiError::coded(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "session not found",
            );
        }
        return ApiError::coded(
            StatusCode::CONFLICT,
            "session_ambiguous",
            "session name is ambiguous",
        );
    }
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

fn internal(what: &str) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{what} failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_handler_comes_from_the_shared_route_table() {
        assert_eq!(ROUTES.len(), 10);
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
        })
        .unwrap();
        assert_eq!(value["browseDirectories"], true);
        assert_eq!(value["createSession"], true);
        assert_eq!(value["stopSession"], true);
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

    fn upload_request(
        target: &str,
        grant: Option<&str>,
        length: Option<u64>,
        body: &[u8],
    ) -> Vec<u8> {
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
        let target =
            format!("/v2/sessions/{ATTACHMENT_SESSION}/attachments?name=Screen%20Shot.PNG");
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
}
