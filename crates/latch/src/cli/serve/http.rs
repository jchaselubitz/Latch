//! `/v1` HTTP and WebSocket surface.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use tokio::sync::{oneshot, Mutex};

use super::auth::{
    load_token, origin_allowed, presented_token, selected_subprotocol, token_matches,
};
use super::contract::{GatewayFeatures, GatewayReadiness, REMOTE_ACCESS_SCHEMA_VERSION};
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
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

const IDEMPOTENCY_TTL: Duration = Duration::from_secs(10 * 60);
const IDEMPOTENCY_LIMIT: usize = 1024;

#[derive(Clone)]
struct AppState {
    home: LatchHome,
    token_file: std::path::PathBuf,
    latch_bin: std::path::PathBuf,
    bind_is_loopback: bool,
    gateway_instance_id: String,
    idempotency: Arc<Mutex<IdempotencyStore>>,
}

#[derive(Default)]
struct IdempotencyStore {
    entries: HashMap<IdempotencyCacheKey, IdempotencyEntry>,
}

#[derive(Hash, PartialEq, Eq)]
struct IdempotencyCacheKey {
    session_id: String,
    key: String,
}

enum IdempotencyEntry {
    InFlight {
        fingerprint: u64,
        waiters: Vec<oneshot::Sender<()>>,
    },
    Complete {
        fingerprint: u64,
        report: harness::SendReport,
        completed_at: Instant,
    },
}

#[derive(Debug)]
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
    let gateway_instance_id = gateway_instance_id();
    let state = AppState {
        home: options.home,
        token_file: options.token_file,
        latch_bin: options.latch_bin,
        bind_is_loopback: options.bind.ip().is_loopback(),
        gateway_instance_id: gateway_instance_id.clone(),
        idempotency: Arc::new(Mutex::new(IdempotencyStore::default())),
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
        HeaderValue::from_static(
            "Authorization, Content-Type, Idempotency-Key, Sec-WebSocket-Protocol",
        ),
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
    features: GatewayFeatures,
    gateway_instance_id: String,
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

async fn gateway_capabilities(State(state): State<AppState>) -> Response {
    Json(GatewayCapabilities {
        engine: manage::capabilities(),
        endpoints: GatewayEndpoints {
            sessions: true,
            session_capabilities: true,
            terminal: true,
            events: true,
            send: true,
        },
        features: GatewayFeatures {
            idempotency_keys: true,
            read_only_terminal: true,
        },
        gateway_instance_id: state.gateway_instance_id,
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
    headers: HeaderMap,
    body: Result<Json<SendRequestBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) =
        body.map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid send body"))?;
    let action = send_action(body)?;
    let key = idempotency_key(&headers)?;
    let fingerprint = action_fingerprint(&action);
    if key.is_some() && fingerprint.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key is supported only for message and resolve operations",
        ));
    }

    let home = state.home.clone();
    let session_id = tokio::task::spawn_blocking(move || manage::resolve_existing(&home, &id))
        .await
        .map_err(|_| internal("resolve session"))?
        .map_err(map_engine_error)?
        .to_string();

    let cache_key = key.map(|key| IdempotencyCacheKey {
        key,
        session_id: session_id.clone(),
    });
    if let (Some(cache_key), Some(fingerprint)) = (&cache_key, fingerprint) {
        if let Some(report) =
            begin_idempotent_request(&state.idempotency, cache_key, fingerprint).await?
        {
            return Ok(Json(report).into_response());
        }
    }

    let home = state.home.clone();
    let report = tokio::task::spawn_blocking(move || {
        harness::send(SendOptions {
            home,
            session: session_id,
            action,
        })
    })
    .await
    .map_err(|_| internal("send"))?
    .map_err(map_engine_error);
    if let (Some(cache_key), Some(fingerprint)) = (&cache_key, fingerprint) {
        finish_idempotent_request(
            &state.idempotency,
            cache_key,
            fingerprint,
            report.as_ref().ok(),
        )
        .await;
    }
    let report = report?;
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
        mode: query.mode,
    };
    upgrade.on_upgrade(move |socket| terminal::run(socket, connect))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid Idempotency-Key"))?;
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid Idempotency-Key",
        ));
    }
    Ok(Some(key.to_owned()))
}

fn action_fingerprint(action: &SendAction) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    match action {
        SendAction::Message(message) => {
            "message".hash(&mut hasher);
            message.hash(&mut hasher);
        }
        SendAction::Resolve { request_id, choice } => {
            "resolve".hash(&mut hasher);
            request_id.hash(&mut hasher);
            choice.hash(&mut hasher);
        }
        SendAction::Keys(_) => return None,
    }
    Some(hasher.finish())
}

async fn begin_idempotent_request(
    store: &Arc<Mutex<IdempotencyStore>>,
    key: &IdempotencyCacheKey,
    fingerprint: u64,
) -> Result<Option<harness::SendReport>, ApiError> {
    loop {
        let waiting = {
            let mut store = store.lock().await;
            prune_idempotency(&mut store);
            match store.entries.get_mut(key) {
                Some(IdempotencyEntry::Complete {
                    fingerprint: stored,
                    report,
                    ..
                }) => {
                    if *stored != fingerprint {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "Idempotency-Key was already used for a different operation",
                        ));
                    }
                    return Ok(Some(report.clone()));
                }
                Some(IdempotencyEntry::InFlight {
                    fingerprint: stored,
                    waiters,
                }) => {
                    if *stored != fingerprint {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "Idempotency-Key was already used for a different operation",
                        ));
                    }
                    let (sender, receiver) = oneshot::channel();
                    waiters.push(sender);
                    Some(receiver)
                }
                None => {
                    if store.entries.len() >= IDEMPOTENCY_LIMIT {
                        return Err(ApiError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "idempotency cache is full; retry with the same key",
                        ));
                    }
                    store.entries.insert(
                        IdempotencyCacheKey {
                            session_id: key.session_id.clone(),
                            key: key.key.clone(),
                        },
                        IdempotencyEntry::InFlight {
                            fingerprint,
                            waiters: Vec::new(),
                        },
                    );
                    return Ok(None);
                }
            }
        };
        if let Some(receiver) = waiting {
            let _ = receiver.await;
        }
    }
}

async fn finish_idempotent_request(
    store: &Arc<Mutex<IdempotencyStore>>,
    key: &IdempotencyCacheKey,
    fingerprint: u64,
    report: Option<&harness::SendReport>,
) {
    let mut store = store.lock().await;
    let Some(IdempotencyEntry::InFlight {
        fingerprint: stored,
        waiters,
    }) = store.entries.remove(key)
    else {
        return;
    };
    if stored == fingerprint {
        if let Some(report) = report {
            store.entries.insert(
                IdempotencyCacheKey {
                    session_id: key.session_id.clone(),
                    key: key.key.clone(),
                },
                IdempotencyEntry::Complete {
                    fingerprint,
                    report: report.clone(),
                    completed_at: Instant::now(),
                },
            );
        }
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }
}

fn prune_idempotency(store: &mut IdempotencyStore) {
    let now = Instant::now();
    store.entries.retain(|_, entry| {
        !matches!(entry, IdempotencyEntry::Complete { completed_at, .. } if now.duration_since(*completed_at) > IDEMPOTENCY_TTL)
    });
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

    #[test]
    fn idempotency_keys_are_bounded_ascii_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", HeaderValue::from_static("mobile-1_2.3"));
        assert_eq!(
            idempotency_key(&headers).unwrap().as_deref(),
            Some("mobile-1_2.3")
        );

        headers.insert("idempotency-key", HeaderValue::from_static("has space"));
        let error = idempotency_key(&headers).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn completed_idempotent_request_returns_its_first_report() {
        let store = Arc::new(Mutex::new(IdempotencyStore::default()));
        let key = IdempotencyCacheKey {
            session_id: "ses_one".to_owned(),
            key: "retry-1".to_owned(),
        };
        let fingerprint = action_fingerprint(&SendAction::Message("hello".to_owned())).unwrap();
        assert!(begin_idempotent_request(&store, &key, fingerprint)
            .await
            .unwrap()
            .is_none());
        let report = harness::SendReport {
            session_id: "ses_one".to_owned(),
            operation: "message",
            request_id: None,
            choice: None,
            sent: true,
            resolved: false,
        };
        finish_idempotent_request(&store, &key, fingerprint, Some(&report)).await;
        assert_eq!(
            begin_idempotent_request(&store, &key, fingerprint)
                .await
                .unwrap(),
            Some(report)
        );
    }

    #[tokio::test]
    async fn idempotency_key_cannot_be_reused_for_new_content() {
        let store = Arc::new(Mutex::new(IdempotencyStore::default()));
        let key = IdempotencyCacheKey {
            session_id: "ses_one".to_owned(),
            key: "retry-1".to_owned(),
        };
        let first = action_fingerprint(&SendAction::Message("first".to_owned())).unwrap();
        let second = action_fingerprint(&SendAction::Message("second".to_owned())).unwrap();
        assert!(begin_idempotent_request(&store, &key, first)
            .await
            .unwrap()
            .is_none());
        let error = begin_idempotent_request(&store, &key, second)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
    }

    #[test]
    fn readiness_handoff_is_private_and_token_free() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("supervision").join("ready.json");
        let readiness = GatewayReadiness {
            format_version: REMOTE_ACCESS_SCHEMA_VERSION,
            address: "127.0.0.1:46123".to_owned(),
            url: "http://127.0.0.1:46123".to_owned(),
            protocol_version: 1,
            gateway_instance_id: "gw-7fa-1234abcd".to_owned(),
        };
        write_readiness(&path, &readiness).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("token"));
        let decoded: GatewayReadiness = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded, readiness);
    }

    #[test]
    fn readiness_refuses_a_shared_parent_directory() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("shared");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let readiness = GatewayReadiness {
            format_version: REMOTE_ACCESS_SCHEMA_VERSION,
            address: "127.0.0.1:46123".to_owned(),
            url: "http://127.0.0.1:46123".to_owned(),
            protocol_version: 1,
            gateway_instance_id: "gw-7fa-1234abcd".to_owned(),
        };
        let error = write_readiness(&parent.join("ready.json"), &readiness).unwrap_err();
        assert!(error.to_string().contains("owner-only"));
    }
}
