//! The authenticated link owner, its sole Yamux driver, and the encrypted
//! record transport loop beneath it.

use std::sync::Arc;
use std::time::Duration;

use futures::future::poll_fn;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config as YamuxConfig, Connection, Mode, Stream};

use super::config::CLOSE_DRAIN_LIMIT;
use super::error::{auth_error, io_error};
use super::handshake::{
    hex, new_hello, prologue, recv_encrypted_json, run_handshake, send_encrypted_json,
    validate_hello, NOISE_PATTERN,
};
use super::stream::{read_header, write_header};
use super::{
    LinkConfig, LinkError, LinkHello, LinkPurpose, LinkRole, LinkTimings, LogicalStream,
    OpenService, RecordIo, Service, HANDSHAKE_TIMEOUT, MAX_RECEIVE_WINDOW_BYTES, MAX_RECORD_BYTES,
    MAX_STREAMS, WRITE_QUANTUM_BYTES,
};

const DUPLEX_BUFFER_BYTES: usize = 256 * 1024;

enum DriverCommand {
    Open(oneshot::Sender<Result<Stream, LinkError>>),
    Close(oneshot::Sender<()>),
}

/// Authenticated link owner. Its driver is the sole Yamux poller.
pub struct SecureLink {
    commands: mpsc::Sender<DriverCommand>,
    inbound: Mutex<mpsc::Receiver<Stream>>,
    task: Mutex<Option<JoinHandle<()>>>,
    closed: tokio::sync::watch::Receiver<bool>,
    /// Remote static public key authenticated by Noise.
    pub remote_public_key: Vec<u8>,
    /// Fresh local/remote nonces identify this generation.
    pub generation: String,
    /// How long authentication took, for content-free diagnostics.
    pub authenticate_ms: u64,
    handshake_hash: Vec<u8>,
}

impl SecureLink {
    /// Authenticates the record carrier, exchanges LinkHello, and starts the
    /// bounded Yamux driver. The deadline covers authentication only; callers
    /// wait for relay peer presence before invoking this.
    pub async fn establish<R: RecordIo>(
        records: R,
        config: LinkConfig,
    ) -> Result<Arc<Self>, LinkError> {
        config.validate()?;
        timeout(HANDSHAKE_TIMEOUT, Self::establish_inner(records, config))
            .await
            .map_err(|_| LinkError::Timeout)?
    }

    /// Resolves once the link's driver has stopped for any reason: peer
    /// closure, dead-peer timeout, protocol failure, or a local `close`.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        while !*closed.borrow() {
            if closed.changed().await.is_err() {
                return;
            }
        }
    }

    /// Whether the driver has already stopped.
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    async fn establish_inner<R: RecordIo>(
        mut records: R,
        config: LinkConfig,
    ) -> Result<Arc<Self>, LinkError> {
        let params = NOISE_PATTERN
            .parse()
            .map_err(|error: snow::Error| LinkError::Authentication(error.to_string()))?;
        let prologue = prologue(&config)?;
        let builder = snow::Builder::new(params)
            .prologue(&prologue)
            .map_err(auth_error)?
            .local_private_key(&config.local_private_key)
            .map_err(auth_error)?;
        let mut handshake = match config.role {
            LinkRole::Controller => builder.build_initiator(),
            LinkRole::Host => builder.build_responder(),
        }
        .map_err(auth_error)?;
        let started = std::time::Instant::now();
        run_handshake(&mut records, &mut handshake, config.role).await?;
        let remote_public_key = handshake
            .get_remote_static()
            .ok_or_else(|| LinkError::Authentication("peer supplied no static key".into()))?
            .to_vec();
        if let Some(expected) = &config.expected_remote_public_key {
            if expected.as_slice() != remote_public_key {
                return Err(LinkError::Authentication(
                    "peer static key does not match pin".into(),
                ));
            }
        }
        let handshake_hash = handshake.get_handshake_hash().to_vec();
        let mut cipher = handshake.into_transport_mode().map_err(auth_error)?;
        let local_hello = new_hello(config.grant_revision);
        send_encrypted_json(&mut records, &mut cipher, &local_hello).await?;
        let remote_hello: LinkHello = recv_encrypted_json(&mut records, &mut cipher).await?;
        validate_hello(&remote_hello)?;
        let generation = format!("{}:{}", local_hello.nonce, remote_hello.nonce);
        let authenticate_ms = started.elapsed().as_millis() as u64;

        let (application, crypt) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
        let mut crypt_task = tokio::spawn(run_transport(records, cipher, crypt, config.timings));
        let mut yamux_config = YamuxConfig::default();
        yamux_config
            .set_max_num_streams(MAX_STREAMS)
            .set_max_connection_receive_window(Some(MAX_RECEIVE_WINDOW_BYTES))
            .set_split_send_size(WRITE_QUANTUM_BYTES)
            .set_read_after_close(true);
        let mode = match config.role {
            LinkRole::Controller => Mode::Client,
            LinkRole::Host => Mode::Server,
        };
        let connection = Connection::new(application.compat(), yamux_config, mode);
        let (commands, command_rx) = mpsc::channel(32);
        let (inbound_tx, inbound) = mpsc::channel(MAX_STREAMS);
        let (closed_tx, closed) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            run_driver(connection, command_rx, inbound_tx, &mut crypt_task).await;
            // `run_driver` returning dropped the yamux connection and with it
            // the plaintext pipe's application end, so the transport task now
            // reads whatever yamux flushed last (final data frames, the
            // stream FIN, GoAway), sends it, and closes the carrier itself.
            // Aborting it here lost the enrollment receipt in the field: the
            // frames were queued but never encrypted and sent. The select
            // above may already have consumed the task's completion; a
            // finished JoinHandle must not be polled again.
            if !crypt_task.is_finished()
                && timeout(CLOSE_DRAIN_LIMIT, &mut crypt_task).await.is_err()
            {
                crypt_task.abort();
                let _ = crypt_task.await;
            }
            let _ = closed_tx.send(true);
        });
        Ok(Arc::new(Self {
            commands,
            inbound: Mutex::new(inbound),
            task: Mutex::new(Some(driver)),
            closed,
            remote_public_key,
            generation,
            authenticate_ms,
            handshake_hash,
        }))
    }

    /// Stable 64-bit owner comparison derived from the authenticated
    /// enrollment transcript and the exact proposed key/grant.
    pub fn enrollment_comparison(
        &self,
        enrollment_id: &str,
        controller_public_key: &[u8],
        permission: &str,
    ) -> Result<String, LinkError> {
        if enrollment_id.is_empty()
            || enrollment_id.len() > 64
            || controller_public_key.len() != 32
            || !matches!(permission, "observe" | "interact" | "control")
        {
            return Err(LinkError::Configuration(
                "invalid enrollment comparison input",
            ));
        }
        let mut digest = Sha256::new();
        digest.update(b"latch-remote-link/v1/enrollment-comparison\0");
        digest.update(&self.handshake_hash);
        digest.update(enrollment_id.as_bytes());
        digest.update([0]);
        digest.update(controller_public_key);
        digest.update([0]);
        digest.update(permission.as_bytes());
        let value = digest.finalize();
        Ok(value[..8].chunks(2).map(hex).collect::<Vec<_>>().join(" "))
    }

    /// Opens an outbound Yamux stream and writes its bounded service header.
    pub async fn open(
        &self,
        service: Service,
        grant_revision: u64,
    ) -> Result<LogicalStream, LinkError> {
        let (send, receive) = oneshot::channel();
        self.commands
            .send(DriverCommand::Open(send))
            .await
            .map_err(|_| LinkError::Closed)?;
        let stream = receive.await.map_err(|_| LinkError::Closed)??;
        let mut stream = LogicalStream {
            inner: stream.compat(),
        };
        write_header(
            &mut stream,
            &OpenService {
                r#type: "open_service".into(),
                version: 1,
                service,
                grant_revision,
            },
        )
        .await?;
        Ok(stream)
    }

    /// Accepts the next inbound stream and validates its service header.
    pub async fn accept(
        &self,
        purpose: LinkPurpose,
    ) -> Result<(OpenService, LogicalStream), LinkError> {
        let stream = self
            .inbound
            .lock()
            .await
            .recv()
            .await
            .ok_or(LinkError::Closed)?;
        let mut stream = LogicalStream {
            inner: stream.compat(),
        };
        let header: OpenService = read_header(&mut stream).await?;
        let allowed = matches!(
            (purpose, header.service),
            (LinkPurpose::Session, Service::Gateway | Service::Control)
                | (
                    LinkPurpose::Enrollment,
                    Service::Enrollment | Service::Control
                )
        );
        if !allowed {
            return Err(LinkError::Authentication(
                "service is forbidden for this link purpose".into(),
            ));
        }
        Ok((header, stream))
    }

    /// Cancels and joins all link-owned work.
    pub async fn close(&self) {
        let (send, receive) = oneshot::channel();
        let _ = self.commands.send(DriverCommand::Close(send)).await;
        let _ = timeout(Duration::from_secs(2), receive).await;
        if let Some(task) = self.task.lock().await.take() {
            let _ = timeout(CLOSE_DRAIN_LIMIT + Duration::from_secs(1), task).await;
        }
    }
}

async fn run_driver<T>(
    mut connection: Connection<T>,
    mut commands: mpsc::Receiver<DriverCommand>,
    inbound: mpsc::Sender<Stream>,
    crypt_task: &mut JoinHandle<Result<(), LinkError>>,
) where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            // The record carrier ending (peer gone, dead-peer timeout, protocol
            // failure) stops the driver even when no stream is being polled.
            // Records decrypted just before the end may still sit in the
            // plaintext pipe as unparsed frames: a peer that answers and then
            // closes puts its answer and its close on the wire back to back.
            // Parse them into their streams (bounded) before the connection
            // goes away, so a reader sees the final bytes and then EOF rather
            // than only EOF.
            _ = &mut *crypt_task => {
                let drain = async {
                    while let Some(Ok(stream)) = poll_fn(|cx| connection.poll_next_inbound(cx)).await {
                        if inbound.try_send(stream).is_err() {
                            break;
                        }
                    }
                };
                let _ = timeout(CLOSE_DRAIN_LIMIT, drain).await;
                break;
            }
            command = commands.recv() => match command {
                Some(DriverCommand::Open(reply)) => {
                    let result = poll_fn(|cx| connection.poll_new_outbound(cx))
                        .await
                        .map_err(|error| LinkError::Io(error.to_string()));
                    let _ = reply.send(result);
                }
                Some(DriverCommand::Close(reply)) => {
                    let _ = poll_fn(|cx| connection.poll_close(cx)).await;
                    let _ = reply.send(());
                    break;
                }
                None => break,
            },
            next = poll_fn(|cx| connection.poll_next_inbound(cx)) => match next {
                Some(Ok(stream)) => {
                    if inbound.try_send(stream).is_err() { break; }
                }
                Some(Err(_)) | None => break,
            }
        }
    }
}

async fn run_transport<R: RecordIo>(
    mut records: R,
    mut cipher: snow::TransportState,
    stream: tokio::io::DuplexStream,
    timings: LinkTimings,
) -> Result<(), LinkError> {
    let (mut plain_read, mut plain_write) = tokio::io::split(stream);
    let mut plaintext = vec![0_u8; WRITE_QUANTUM_BYTES];
    let mut decrypted = vec![0_u8; MAX_RECORD_BYTES];
    let mut last_sent = tokio::time::Instant::now();
    let dead = tokio::time::sleep(timings.dead_peer_timeout);
    tokio::pin!(dead);
    let mut keepalive = tokio::time::interval(timings.keepalive_interval);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        tokio::select! {
            read = plain_read.read(&mut plaintext) => {
                let count = match read {
                    Ok(count) => count,
                    Err(error) => break Err(io_error(error)),
                };
                if count == 0 { break Ok(()); }
                let mut encrypted = vec![0_u8; count + 16];
                let written = match cipher.write_message(&plaintext[..count], &mut encrypted) {
                    Ok(written) => written,
                    Err(error) => break Err(auth_error(error)),
                };
                encrypted.truncate(written);
                if let Err(error) = records.send_record(encrypted).await { break Err(error); }
                last_sent = tokio::time::Instant::now();
            }
            record = records.recv_record() => {
                let record = match record {
                    Ok(Some(record)) => record,
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                };
                dead.as_mut().reset(tokio::time::Instant::now() + timings.dead_peer_timeout);
                let written = match cipher.read_message(&record, &mut decrypted) {
                    Ok(written) => written,
                    Err(error) => break Err(auth_error(error)),
                };
                // An empty plaintext is a keepalive: authenticated liveness
                // with no application bytes.
                if written > 0 {
                    if let Err(error) = plain_write.write_all(&decrypted[..written]).await {
                        break Err(io_error(error));
                    }
                }
            }
            _ = keepalive.tick() => {
                if last_sent.elapsed() < timings.keepalive_interval { continue; }
                let mut encrypted = vec![0_u8; 16];
                let written = match cipher.write_message(&[], &mut encrypted) {
                    Ok(written) => written,
                    Err(error) => break Err(auth_error(error)),
                };
                encrypted.truncate(written);
                if let Err(error) = records.send_record(encrypted).await { break Err(error); }
                last_sent = tokio::time::Instant::now();
            }
            _ = &mut dead => break Err(LinkError::Timeout),
        }
    };
    let _ = plain_write.shutdown().await;
    let _ = records.close().await;
    result
}
