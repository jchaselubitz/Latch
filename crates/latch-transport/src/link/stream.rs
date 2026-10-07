//! One authenticated logical stream and the service header that opens it.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use yamux::Stream;

use super::error::io_error;
use super::{LinkError, OpenService};

/// One authenticated logical stream.
pub struct LogicalStream {
    pub(super) inner: tokio_util::compat::Compat<Stream>,
}

impl AsyncRead for LogicalStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl AsyncWrite for LogicalStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub(super) async fn write_header<T: AsyncWrite + Unpin>(
    stream: &mut T,
    header: &OpenService,
) -> Result<(), LinkError> {
    let encoded =
        serde_json::to_vec(header).map_err(|error| LinkError::Authentication(error.to_string()))?;
    let length =
        u16::try_from(encoded.len()).map_err(|_| LinkError::Limit("service header too large"))?;
    stream
        .write_all(&length.to_be_bytes())
        .await
        .map_err(io_error)?;
    stream.write_all(&encoded).await.map_err(io_error)?;
    stream.flush().await.map_err(io_error)
}

pub(super) async fn read_header<T: AsyncRead + Unpin>(
    stream: &mut T,
) -> Result<OpenService, LinkError> {
    let mut prefix = [0_u8; 2];
    stream.read_exact(&mut prefix).await.map_err(io_error)?;
    let length = u16::from_be_bytes(prefix) as usize;
    if length == 0 || length > 1024 {
        return Err(LinkError::Limit("invalid service header length"));
    }
    let mut encoded = vec![0_u8; length];
    stream.read_exact(&mut encoded).await.map_err(io_error)?;
    let header: OpenService = serde_json::from_slice(&encoded)
        .map_err(|error| LinkError::Authentication(error.to_string()))?;
    if header.r#type != "open_service" || header.version != 1 {
        return Err(LinkError::Authentication(
            "unsupported service header".into(),
        ));
    }
    Ok(header)
}
