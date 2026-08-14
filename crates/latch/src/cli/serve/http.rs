//! `/v1` HTTP and WebSocket surface.

use anyhow::Context;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;

use super::auth::{
    load_token, origin_allowed, presented_token, selected_subprotocol, token_matches,
};
use super::events::{self, EventsConnect, EventsQuery};
use super::terminal::{self, TerminalConnect, TerminalQuery};
use super::ServeOptions;
use crate::cli::attach::SessionLookupError;
use crate::cli::json::CapabilitiesReport;
use crate::cli::manage::{self, InspectOptions, ListOptions};
use crate::harness::{
    self, EventsAvailability, InteractionCapabilities, InteractionOptions, SendAction, SendInvalid,
    SendOptions, SendRefused,
};
use crate::session::paths::LatchHome;

#[derive(Clone)]
struct AppState {
    home: LatchHome,
    token_file: std::path::PathBuf,
    latch_bin: std::path::PathBuf,
    bind_is_loopback: bool,
}

struct ApiError {
    status: StatusCode,
    message: String,
    reason: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            reason: None,
        }
    }

    fn refused(reason: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "refused".to_owned(),
            reason: Some(reason.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match &self.reason {
            Some(reason) => json!({ "error": self.message, "reason": reason }),
            None => json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
    }
}

/// Binds and serves until SIGINT/SIGTERM.
pub async fn run(options: ServeOptions) -> anyhow::Result<()> {
    let state = AppState {
        home: options.home,
        token_file: options.token_file,
        latch_bin: options.latch_bin,
        bind_is_loopback: options.bind.ip().is_loopback(),
    };
    let app = Router::new()
        .route("/v1/capabilities", get(gateway_capabilities))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/{id}", get(inspect_session))
        .route("/v1/sessions/{id}/capabilities", get(session_capabilities))
        .route("/v1/sessions/{id}/send", post(send_to_session))
        .route("/v1/sessions/{id}/terminal", get(terminal_ws))
        .route("/v1/sessions/{id}/events", get(events_ws))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .layer(middleware::from_fn(add_cors))
        .with_state(state);

    let listener = TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("cannot bind {}", options.bind))?;
    let addr = listener.local_addr()?;
    eprintln!("latch serve listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve failed")
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
    request: Request<axum::body::Body>,
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
    Ok(next.run(request).await)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayCapabilities {
    #[serde(flatten)]
    engine: CapabilitiesReport,
    endpoints: GatewayEndpoints,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayEndpoints {
    sessions: bool,
    session_capabilities: bool,
    terminal: bool,
    events: bool,
    send: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionCapabilitiesDocument {
    #[serde(flatten)]
    interaction: InteractionCapabilities,
    events: EventsAvailability,
}

async fn gateway_capabilities() -> Response {
    Json(GatewayCapabilities {
        engine: manage::capabilities(),
        endpoints: GatewayEndpoints {
            sessions: true,
            session_capabilities: true,
            terminal: true,
            events: true,
            send: true,
        },
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

async fn session_capabilities(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let home = state.home.clone();
    let report =
        tokio::task::spawn_blocking(move || -> anyhow::Result<SessionCapabilitiesDocument> {
            let interaction = harness::interaction_capabilities(InteractionOptions {
                home: home.clone(),
                session: id.clone(),
            })?;
            let events = harness::events_availability(InteractionOptions { home, session: id })?;
            Ok(SessionCapabilitiesDocument {
                interaction,
                events,
            })
        })
        .await
        .map_err(|_| internal("session capabilities"))?
        .map_err(map_engine_error)?;
    Ok(Json(report).into_response())
}

#[derive(Debug, Deserialize)]
struct SendRequestBody {
    message: Option<String>,
    keys: Option<String>,
    resolve: Option<ResolveBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveBody {
    request_id: String,
    choice: String,
}

fn send_action(body: SendRequestBody) -> Result<SendAction, ApiError> {
    let supplied = usize::from(body.message.is_some())
        + usize::from(body.keys.is_some())
        + usize::from(body.resolve.is_some());
    if supplied != 1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "choose exactly one of message, keys, or resolve",
        ));
    }
    if let Some(message) = body.message {
        return Ok(SendAction::Message(message));
    }
    if let Some(keys) = body.keys {
        return Ok(SendAction::Keys(keys));
    }
    let resolve = body.resolve.expect("one send operation was supplied");
    Ok(SendAction::Resolve {
        request_id: resolve.request_id,
        choice: resolve.choice,
    })
}

async fn send_to_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<SendRequestBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) =
        body.map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid send body"))?;
    let action = send_action(body)?;
    let home = state.home.clone();
    let report = tokio::task::spawn_blocking(move || {
        harness::send(SendOptions {
            home,
            session: id,
            action,
        })
    })
    .await
    .map_err(|_| internal("send"))?
    .map_err(map_engine_error)?;
    Ok(Json(report).into_response())
}

async fn terminal_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
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
    };
    upgrade.on_upgrade(move |socket| terminal::run(socket, connect))
}

async fn events_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let mut upgrade = ws;
    if let Some(protocol) = selected_subprotocol(&headers) {
        upgrade = upgrade.protocols([protocol]);
    }
    let connect = EventsConnect {
        home: state.home.clone(),
        latch_bin: state.latch_bin.clone(),
        session: id,
        cursor: query.cursor.unwrap_or(0),
    };
    upgrade.on_upgrade(move |socket| events::run(socket, connect))
}

fn map_engine_error(error: anyhow::Error) -> ApiError {
    if let Some(refused) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SendRefused>())
    {
        return ApiError::refused(refused.reason.clone());
    }
    if let Some(invalid) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SendInvalid>())
    {
        return ApiError::new(StatusCode::BAD_REQUEST, invalid.reason.clone());
    }
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
    use crate::cli::attach::SessionLookupError;

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

    #[test]
    fn absent_tmux_session_is_404() {
        let mapped = map_engine_error(
            SessionLookupError::NotInServer {
                session: "ses_dead".to_owned(),
            }
            .into(),
        );
        assert_eq!(mapped.status, StatusCode::NOT_FOUND);
        assert_eq!(mapped.message, "session not found");
    }

    #[test]
    fn ambiguous_name_is_conflict_without_the_name() {
        let mapped = map_engine_error(
            SessionLookupError::Ambiguous {
                session: "agent".to_owned(),
            }
            .into(),
        );
        assert_eq!(mapped.status, StatusCode::CONFLICT);
        assert_eq!(mapped.message, "session name is ambiguous");
        assert!(!mapped.message.contains("agent"));
    }

    #[test]
    fn other_errors_are_generic_500() {
        let mapped = map_engine_error(anyhow::anyhow!("cannot read /private/path/meta.json"));
        assert_eq!(mapped.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(mapped.message, "internal error");
        assert!(mapped.reason.is_none());
        assert!(!mapped.message.contains("/private"));
    }

    #[test]
    fn send_refusal_is_409_with_a_reason() {
        let mapped = map_engine_error(
            SendRefused {
                reason: "the Claude Code composer already contains text".to_owned(),
            }
            .into(),
        );
        assert_eq!(mapped.status, StatusCode::CONFLICT);
        assert_eq!(mapped.message, "refused");
        assert_eq!(
            mapped.reason.as_deref(),
            Some("the Claude Code composer already contains text")
        );
    }

    #[test]
    fn send_invalid_input_is_400() {
        let mapped = map_engine_error(
            SendInvalid {
                reason: "message must not be empty".to_owned(),
            }
            .into(),
        );
        assert_eq!(mapped.status, StatusCode::BAD_REQUEST);
        assert_eq!(mapped.message, "message must not be empty");
        assert!(mapped.reason.is_none());
    }

    #[test]
    fn send_body_requires_exactly_one_operation() {
        let error = send_action(SendRequestBody {
            message: Some("hello".to_owned()),
            keys: Some("Enter".to_owned()),
            resolve: None,
        })
        .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("exactly one"));
    }
}
