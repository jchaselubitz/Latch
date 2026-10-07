//! Attachment request validation and bounded body streaming.

use super::{internal, map_engine_error, ApiError, AppState};
use crate::cli::manage;
use crate::cli::serve::attachments::{self, AttachmentError};
use crate::cli::serve::routes::ATTACHMENT_MAX_BYTES;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(serde::Deserialize)]
pub(super) struct AttachmentQuery {
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
pub(super) async fn upload_attachment(
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
