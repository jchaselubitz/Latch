//! HTTP boundary for the paired device's explicit Files shares.

use axum::extract::{Extension, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{internal, ApiError, AppState, DeviceContext};
use crate::cli::serve::shared_files::{self, Error};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DirectoryQuery {
    root_id: String,
    #[serde(default)]
    path: String,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PreviewQuery {
    root_id: String,
    path: String,
}

pub(super) async fn roots(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
) -> Result<Response, ApiError> {
    let device = paired(device)?;
    run(move || shared_files::roots(&state.home, &device)).await
}

pub(super) async fn directory(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Response, ApiError> {
    let device = paired(device)?;
    run(move || {
        state.shared_cursors.page(
            &state.home,
            &device,
            &query.root_id,
            query.path,
            query.cursor.as_deref(),
        )
    })
    .await
}

pub(super) async fn preview(
    State(state): State<AppState>,
    Extension(device): Extension<DeviceContext>,
    Query(query): Query<PreviewQuery>,
) -> Result<Response, ApiError> {
    let device = paired(device)?;
    run(move || shared_files::preview(&state.home, &device, &query.root_id, &query.path)).await
}

fn paired(device: DeviceContext) -> Result<String, ApiError> {
    device.0.ok_or_else(|| map_error(Error::Denied))
}

async fn run<T: Serialize + Send + 'static>(
    operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<Response, ApiError> {
    let value = tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| internal("read shared files"))?
        .map_err(map_error)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response())
}

fn map_error(error: Error) -> ApiError {
    let (status, code) = match error {
        Error::Denied => (StatusCode::FORBIDDEN, "shared_files_access_denied"),
        Error::RootUnavailable => (StatusCode::NOT_FOUND, "shared_root_unavailable"),
        Error::InvalidPath => (StatusCode::BAD_REQUEST, "invalid_shared_path"),
        Error::EntryUnavailable => (StatusCode::NOT_FOUND, "shared_entry_unavailable"),
        Error::Changed => (StatusCode::CONFLICT, "shared_entry_changed"),
        Error::StaleCursor => (StatusCode::CONFLICT, "stale_cursor"),
        Error::DirectoryTooLarge => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "shared_directory_too_large",
        ),
        Error::Failed => (StatusCode::INTERNAL_SERVER_ERROR, "shared_files_failed"),
    };
    ApiError::coded(status, code, error.to_string())
}
