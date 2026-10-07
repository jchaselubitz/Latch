//! Protocol-major-2 HTTP and WebSocket gateway.

use std::net::SocketAddr;
use std::time::SystemTime;

use anyhow::Context;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;

mod authority;
mod readiness;
mod uploads;
use super::attention::AttentionWatcher;
use super::auth::selected_subprotocol;
use super::contract::{
    AgentModelCatalog, CreateSessionRequest, GatewayFeatures, GatewayReadiness, SessionAgent,
    OPERATION_RETENTION_SECONDS, REMOTE_ACCESS_SCHEMA_VERSION,
};
use super::conversation::{self, ConversationConnect, ConversationQuery};
use super::directory::{self, BrowseError};
use super::routes::{Grant, RouteId, RouteSpec, ATTACHMENT_MAX_BYTES, ROUTES};
use super::terminal::{self, ResumeRegistry, TerminalConnect, TerminalQuery};
use super::ServeOptions;
use crate::cli::agent_models;
use crate::cli::attach::SessionLookupError;
use crate::cli::create::{self, RemoteSessionError, RemoteSessionRequest};
use crate::cli::json::{CapabilitiesReport, CreateReport, CreatedSession};
use crate::cli::manage::{self, InspectOptions, ListOptions, StopRequest};
use crate::conversation::ConversationHub;
use crate::session::paths::LatchHome;
#[cfg(test)]
use authority::apply_cors;
use authority::{add_cors, require_token};
use readiness::{gateway_instance_id, write_readiness};
use uploads::upload_attachment;

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
        RouteId::AgentModels => router.route(spec.pattern, get(agent_models_catalog)),
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
    agent_models: bool,
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
            agent_models: true,
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

/// The models `agent` can start with, read fresh from its own cache on this
/// Mac each time, so a phone that opens the picker sees what the agent itself
/// would offer today.
async fn agent_models_catalog(Path(agent): Path<String>) -> Result<Response, ApiError> {
    let agent = SESSION_AGENTS
        .iter()
        .copied()
        .find(|known| known.harness() == agent)
        .ok_or_else(|| {
            ApiError::coded(
                StatusCode::NOT_FOUND,
                "not_found",
                "this gateway does not launch that agent",
            )
        })?;
    let catalog: AgentModelCatalog =
        tokio::task::spawn_blocking(move || agent_models::catalog(agent))
            .await
            .map_err(|_| internal("list agent models"))?;
    Ok(Json(catalog).into_response())
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
    if let Some(model) = &request.model {
        if request.agent.is_none() || !agent_models::is_model_id(model) {
            return Err(ApiError::coded(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "model must name one of the agent's listed models",
            ));
        }
    }
    let cwd = directory::canonical_directory_from_str(&request.cwd).map_err(map_browse_error)?;

    let home = state.home.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        // Only a model the agent lists today is launched: the phone chose
        // from this same catalog, and anything else is a stale or forged id.
        if let (Some(agent), Some(model)) = (request.agent, request.model.as_deref()) {
            if !agent_models::catalog(agent)
                .models
                .iter()
                .any(|listed| listed.id == model)
            {
                return Err(RemoteSessionError::ModelUnavailable(agent));
            }
        }
        create::create_remote_session(RemoteSessionRequest {
            home,
            request_id: request.request_id,
            cwd,
            agent: request.agent,
            model: request.model,
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
        // The phone's list was stale: it can reopen the picker and choose
        // again, so the sentence says that rather than naming the id.
        RemoteSessionError::ModelUnavailable(agent) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "model_unavailable",
            format!(
                "{} on this Mac no longer offers that model; choose another",
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
mod tests;
