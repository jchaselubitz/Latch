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
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;

use super::auth::{
    load_token, origin_allowed, presented_token, selected_subprotocol, token_matches,
};
use super::contract::{
    GatewayFeatures, GatewayReadiness, OPERATION_RETENTION_SECONDS, REMOTE_ACCESS_SCHEMA_VERSION,
};
use super::conversation::{self, ConversationConnect, ConversationQuery};
use super::routes::{route_for, Grant, RouteId, RouteSpec, DEVICE_GRANT_HEADER, ROUTES};
use super::terminal::{self, TerminalConnect, TerminalQuery};
use super::ServeOptions;
use crate::cli::attach::SessionLookupError;
use crate::cli::json::CapabilitiesReport;
use crate::cli::manage::{self, InspectOptions, ListOptions};
use crate::conversation::ConversationHub;
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

#[derive(Clone)]
struct AppState {
    home: LatchHome,
    token_file: std::path::PathBuf,
    latch_bin: std::path::PathBuf,
    bind_is_loopback: bool,
    gateway_instance_id: String,
    /// Also keeps the exclusive Hub writer lock alive for the gateway lifetime.
    conversation_hub: ConversationHub,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
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
    let state = AppState {
        home: options.home,
        token_file: options.token_file,
        latch_bin: options.latch_bin,
        bind_is_loopback: options.bind.ip().is_loopback(),
        gateway_instance_id: gateway_instance_id.clone(),
        conversation_hub,
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
#[cfg(test)]
pub(crate) fn test_router(
    home: LatchHome,
    token_file: std::path::PathBuf,
    conversation_hub: ConversationHub,
) -> Router {
    router(AppState {
        home,
        token_file,
        latch_bin: std::path::PathBuf::from("latch"),
        bind_is_loopback: true,
        gateway_instance_id: "gw-test".to_owned(),
        conversation_hub,
    })
}

fn register(router: Router<AppState>, spec: RouteSpec) -> Router<AppState> {
    match spec.id {
        RouteId::Capabilities => router.route(spec.pattern, get(gateway_capabilities)),
        RouteId::Sessions => router.route(spec.pattern, get(list_sessions)),
        RouteId::Session => router.route(spec.pattern, get(inspect_session)),
        RouteId::Terminal => router.route(spec.pattern, get(terminal_ws)),
        RouteId::Conversation => router.route(spec.pattern, get(conversation_ws)),
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
        HeaderValue::from_static("GET, OPTIONS"),
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
    if !origin_allowed(
        request.headers().get(header::ORIGIN),
        state.bind_is_loopback,
    ) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "origin not allowed"));
    }
    let expected = load_token(&state.token_file).unwrap_or_default();
    let presented = presented_token(request.headers()).unwrap_or_default();
    if !token_matches(&expected, &presented) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid token"));
    }

    let peer_is_loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(state.bind_is_loopback);
    let grant =
        match request.headers().get(DEVICE_GRANT_HEADER) {
            Some(value) if peer_is_loopback => value
                .to_str()
                .ok()
                .and_then(Grant::from_header_value)
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
struct GatewayEndpoints {
    sessions: bool,
    terminal: bool,
    conversation: bool,
}

async fn gateway_capabilities(State(state): State<AppState>) -> Response {
    Json(GatewayCapabilities {
        engine: manage::capabilities(),
        endpoints: GatewayEndpoints {
            sessions: true,
            terminal: true,
            conversation: true,
        },
        features: GatewayFeatures {
            read_only_terminal: true,
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

async fn terminal_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(_grant): Extension<Grant>,
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
        mode: query.mode,
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
        if lookup.is_absent() {
            return ApiError::new(StatusCode::NOT_FOUND, "session not found");
        }
        return ApiError::new(StatusCode::CONFLICT, "session name is ambiguous");
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
        assert_eq!(ROUTES.len(), 5);
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
        assert_eq!(mapped.message, "session not found");
        assert!(!mapped.message.contains("secret-name"));
    }
}
