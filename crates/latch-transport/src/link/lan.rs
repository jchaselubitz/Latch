//! Length-prefixed LAN record carrier.

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::io_error;
use super::{LinkError, RecordIo, MAX_RECORD_BYTES};

/// Length-prefixed LAN carrier using the same authenticated protocol.
///
/// Receiving is cancel-safe: partial frames stay in `buffer` across a dropped
/// `recv_record` future, so a `select!` that races a read with a write never
/// loses bytes.
pub struct LanRecordIo<T> {
    stream: T,
    buffer: Vec<u8>,
}

impl<T> LanRecordIo<T> {
    /// Wraps an accepted or connected LAN TCP stream.
    pub fn new(stream: T) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(MAX_RECORD_BYTES + 2),
        }
    }

    fn framed(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        if self.buffer.len() < 2 {
            return Ok(None);
        }
        let length = u16::from_be_bytes([self.buffer[0], self.buffer[1]]) as usize;
        if length == 0 || length > MAX_RECORD_BYTES {
            return Err(LinkError::Limit("invalid LAN record length"));
        }
        if self.buffer.len() < 2 + length {
            return Ok(None);
        }
        let record = self.buffer[2..2 + length].to_vec();
        self.buffer.drain(..2 + length);
        Ok(Some(record))
    }
}

#[async_trait]
impl<T> RecordIo for LanRecordIo<T>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
        let length =
            u16::try_from(record.len()).map_err(|_| LinkError::Limit("oversized LAN record"))?;
        self.stream
            .write_all(&length.to_be_bytes())
            .await
            .map_err(io_error)?;
        self.stream.write_all(&record).await.map_err(io_error)?;
        self.stream.flush().await.map_err(io_error)
    }

    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        loop {
            if let Some(record) = self.framed()? {
                return Ok(Some(record));
            }
            let mut chunk = [0_u8; 16 * 1024];
            let read = self.stream.read(&mut chunk).await.map_err(io_error)?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                return Err(LinkError::Io("LAN record truncated".into()));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        self.stream.shutdown().await.map_err(io_error)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn lan_records_survive_a_cancelled_receive() {
        let (client, server) = tokio::io::duplex(1024);
        let mut writer = LanRecordIo::new(client);
        let mut reader = LanRecordIo::new(server);
        let first = vec![1_u8; 3000];
        let second = vec![2_u8; 5];
        let expected = vec![first.clone(), second.clone()];
        let writing = tokio::spawn(async move {
            writer.send_record(first).await.unwrap();
            writer.send_record(second).await.unwrap();
            writer
        });
        // Cancel the receive repeatedly mid-frame: a cancel-unsafe reader
        // would lose the prefix or a partial body.
        let mut received = Vec::new();
        while received.len() < 2 {
            match tokio::time::timeout(Duration::from_micros(50), reader.recv_record()).await {
                Ok(Ok(Some(record))) => received.push(record),
                Ok(Ok(None)) => panic!("unexpected EOF"),
                Ok(Err(error)) => panic!("{error}"),
                Err(_) => continue,
            }
        }
        assert_eq!(received, expected);
        let _ = writing.await.unwrap();
    }
}
