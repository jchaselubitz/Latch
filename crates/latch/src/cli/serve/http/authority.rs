//! HTTP origin, bearer, and local proxy grant enforcement.

use super::{is_device_id, ApiError, AppState, DeviceContext};
use crate::cli::serve::auth::{load_token, origin_allowed, presented_token, token_matches};
use crate::cli::serve::routes::{route_for, Grant, DEVICE_GRANT_HEADER, DEVICE_ID_HEADER};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

pub(super) async fn add_cors(request: Request<axum::body::Body>, next: Next) -> Response {
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

pub(super) fn apply_cors(headers: &mut HeaderMap, origin: Option<&HeaderValue>) {
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

pub(super) async fn require_token(
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
