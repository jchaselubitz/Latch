//! Authenticated, multiplexed Remote Link v1.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::{future::poll_fn, SinkExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
#[cfg(feature = "test-ca")]
use tokio_tungstenite::{connect_async_tls_with_config, Connector};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config as YamuxConfig, Connection, Mode, Stream};
use zeroize::Zeroizing;

/// Maximum encoded Noise record carried by one WebSocket message or LAN frame.
pub const MAX_RECORD_BYTES: usize = 65_535;
/// Plaintext is deliberately split so one writer cannot monopolize the link.
pub const WRITE_QUANTUM_BYTES: usize = 16 * 1024;
/// Maximum concurrent application streams per authenticated link.
pub const MAX_STREAMS: usize = 32;
/// Total Yamux receive credit across all streams.
pub const MAX_RECEIVE_WINDOW_BYTES: usize = 8 * 1024 * 1024;
/// Deadline for Noise authentication and LinkHello once both peers are present.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// An idle link sends one empty encrypted record at this interval so silent
/// transport loss is detected on both carriers without any relay help.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// A link that receives nothing for this long is dead, whatever the socket says.
pub const DEAD_PEER_TIMEOUT: Duration = Duration::from_secs(45);

const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const DUPLEX_BUFFER_BYTES: usize = 256 * 1024;

/// Liveness timing for one link. Production callers use the defaults; tests
/// shorten them to exercise the same code paths in milliseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkTimings {
    /// Idle interval between keepalive records.
    pub keepalive_interval: Duration,
    /// Silence after which the link is declared dead and torn down.
    pub dead_peer_timeout: Duration,
}

impl Default for LinkTimings {
    fn default() -> Self {
        Self {
            keepalive_interval: KEEPALIVE_INTERVAL,
            dead_peer_timeout: DEAD_PEER_TIMEOUT,
        }
    }
}

/// Content-free stage durations for one link establishment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LinkStageTimings {
    /// Transport connect (TCP/TLS/WebSocket upgrade, or LAN TCP connect).
    pub connect_ms: u64,
    /// Time spent waiting for the relay to report the opposite role present.
    pub peer_wait_ms: u64,
    /// Noise XX plus LinkHello.
    pub authenticate_ms: u64,
}

/// Link purpose is cryptographically bound so enrollment cannot open a gateway.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPurpose {
    /// Provisional, QR-bound enrollment only.
    Enrollment,
    /// A normal link between two locally pinned devices.
    Session,
}

/// Fixed endpoint roles. Controllers initiate Noise and Yamux.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    /// Mac helper and gateway authority.
    Host,
    /// Phone or another approved controller.
    Controller,
}

/// Only these services may be opened over a logical stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    /// Fixed local `/v2` gateway.
    Gateway,
    /// Provisional enrollment exchange.
    Enrollment,
    /// Reserved link-control stream.
    Control,
}

/// Negotiated resource bounds. V1 accepts exactly these values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkLimits {
    /// Maximum application streams.
    pub max_streams: u16,
    /// Initial receive credit per stream.
    pub stream_window_bytes: u32,
    /// Maximum aggregate receive credit.
    pub total_buffer_bytes: u32,
    /// Maximum Noise record size.
    pub record_bytes: u32,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            max_streams: MAX_STREAMS as u16,
            stream_window_bytes: 256 * 1024,
            total_buffer_bytes: MAX_RECEIVE_WINDOW_BYTES as u32,
            record_bytes: MAX_RECORD_BYTES as u32,
        }
    }
}

/// Encrypted hello exchanged before the multiplexer is exposed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkHello {
    /// Must be `link_hello`.
    pub r#type: String,
    /// Remote Link version.
    pub version: u16,
    /// Fresh endpoint nonce, hex encoded.
    pub nonce: String,
    /// Host-owned grant revision known to this endpoint.
    pub grant_revision: u64,
    /// Fixed V1 limits.
    pub limits: LinkLimits,
}

/// Header sent first on every Yamux stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenService {
    /// Must be `open_service`.
    pub r#type: String,
    /// Remote Link version.
    pub version: u16,
    /// Requested fixed service.
    pub service: Service,
    /// Grant revision held by the controller.
    pub grant_revision: u64,
}

/// Inputs required to authenticate one endpoint.
pub struct LinkConfig {
    /// Purpose bound into the Noise prologue.
    pub purpose: LinkPurpose,
    /// Local fixed role.
    pub role: LinkRole,
    /// Local 32-byte X25519 private key.
    pub local_private_key: Zeroizing<Vec<u8>>,
    /// Local 32-byte X25519 public key used in the canonical session prologue.
    pub local_public_key: Vec<u8>,
    /// Exact peer static key, when already paired.
    pub expected_remote_public_key: Option<Vec<u8>>,
    /// Enrollment identifier for provisional links.
    pub enrollment_id: Option<String>,
    /// QR-only 256-bit enrollment secret, never sent to a service.
    pub enrollment_secret: Option<Zeroizing<Vec<u8>>>,
    /// Current host grant revision.
    pub grant_revision: u64,
    /// Keepalive and dead-peer timing.
    pub timings: LinkTimings,
}

impl LinkConfig {
    fn validate(&self) -> Result<(), LinkError> {
        if self.local_private_key.len() != 32 {
            return Err(LinkError::Configuration(
                "local private key must be 32 bytes",
            ));
        }
        if self.local_public_key.len() != 32 {
            return Err(LinkError::Configuration(
                "local public key must be 32 bytes",
            ));
        }
        if self
            .expected_remote_public_key
            .as_ref()
            .is_some_and(|key| key.len() != 32)
        {
            return Err(LinkError::Configuration(
                "remote public key must be 32 bytes",
            ));
        }
        match self.purpose {
            LinkPurpose::Session if self.expected_remote_public_key.is_none() => Err(
                LinkError::Configuration("session links require an exact peer pin"),
            ),
            LinkPurpose::Enrollment
                if self.enrollment_id.as_deref().unwrap_or_default().is_empty()
                    || self
                        .enrollment_secret
                        .as_ref()
                        .is_none_or(|secret| secret.len() != 32) =>
            {
                Err(LinkError::Configuration(
                    "enrollment links require an id and 32-byte QR secret",
                ))
            }
            _ => Ok(()),
        }
    }
}

/// Fail-closed remote-link errors.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// Invalid local inputs.
    #[error("invalid remote-link configuration: {0}")]
    Configuration(&'static str),
    /// Authentication or protocol mismatch.
    #[error("remote-link authentication failed: {0}")]
    Authentication(String),
    /// The peer or transport exceeded a fixed bound.
    #[error("remote-link limit exceeded: {0}")]
    Limit(&'static str),
    /// The operation exceeded its deadline.
    #[error("remote-link operation timed out")]
    Timeout,
    /// The relay never reported the opposite endpoint within the wait bound.
    #[error("the paired endpoint is not connected to the relay")]
    PeerUnavailable,
    /// The link has closed.
    #[error("remote link is closed")]
    Closed,
    /// Underlying I/O failed.
    #[error("remote-link I/O failed: {0}")]
    Io(String),
}

/// A record-preserving carrier. WSS uses one binary message per record; LAN
/// uses one bounded length-prefixed record.
#[async_trait]
pub trait RecordIo: Send + Unpin + 'static {
    /// Sends exactly one record.
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError>;
    /// Receives exactly one record, or `None` after normal closure.
    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError>;
    /// Closes the carrier.
    async fn close(&mut self) -> Result<(), LinkError>;
}

/// System-validated WSS carrier. Admission is only accepted in the
/// Authorization header and never appears in a URL or loggable query string.
pub struct WssRecordIo {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    control: mpsc::Receiver<String>,
    statuses: mpsc::Sender<RelayStatus>,
    /// Binary records that arrived while waiting for a relay status frame.
    pending: std::collections::VecDeque<Vec<u8>>,
    /// Set once the relay reported the opposite role present.
    peer_ready: bool,
}

/// Relay-only control events. They are untrusted reachability/lease metadata
/// and never participate in peer authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayStatus {
    /// Both opaque room roles are currently attached.
    PeerReady,
    /// No opposite role is attached; endpoint ciphertext was not buffered.
    PeerUnavailable,
    /// Opaque lease handle returned after private ticket redemption.
    LeaseStarted {
        /// Opaque lease identifier, containing no account or endpoint identity.
        lease_id: String,
        /// Unix expiry enforced by the relay.
        expires_at: u64,
    },
}

/// Bounded side channel for signed relay lease extensions and status.
pub struct WssControl {
    outbound: mpsc::Sender<String>,
    statuses: Mutex<mpsc::Receiver<RelayStatus>>,
}

impl WssRecordIo {
    /// Connects using platform trust through `native-tls`/Security.framework.
    pub async fn connect(url: &str, admission: &str) -> Result<Self, LinkError> {
        let (records, _control) = Self::connect_with_control(url, admission).await?;
        Ok(records)
    }

    /// Connects and returns the relay control handle retained by the endpoint
    /// owner for lease renewal.
    pub async fn connect_with_control(
        url: &str,
        admission: &str,
    ) -> Result<(Self, WssControl), LinkError> {
        Self::connect_with(url, admission, None).await
    }

    /// Test-only system-TLS path with one explicit private CA. This method is
    /// absent from production builds, so shipping code cannot weaken trust.
    #[cfg(feature = "test-ca")]
    pub async fn connect_with_test_ca(
        url: &str,
        admission: &str,
        ca_pem: &[u8],
    ) -> Result<(Self, WssControl), LinkError> {
        let certificate = native_tls::Certificate::from_pem(ca_pem)
            .map_err(|error| LinkError::Io(error.to_string()))?;
        let mut builder = native_tls::TlsConnector::builder();
        builder.add_root_certificate(certificate);
        let connector = builder
            .build()
            .map_err(|error| LinkError::Io(error.to_string()))?;
        Self::connect_with(url, admission, Some(Connector::NativeTls(connector))).await
    }

    async fn connect_with(
        url: &str,
        admission: &str,
        #[cfg(feature = "test-ca")] connector: Option<Connector>,
        #[cfg(not(feature = "test-ca"))] _connector: Option<()>,
    ) -> Result<(Self, WssControl), LinkError> {
        let mut request = url
            .into_client_request()
            .map_err(|error| LinkError::Io(error.to_string()))?;
        let value = format!("Bearer {admission}")
            .parse()
            .map_err(|_| LinkError::Configuration("invalid admission header"))?;
        request.headers_mut().insert(AUTHORIZATION, value);
        #[cfg(feature = "test-ca")]
        let connection = if connector.is_some() {
            connect_async_tls_with_config(request, None, false, connector).await
        } else {
            connect_async(request).await
        };
        #[cfg(not(feature = "test-ca"))]
        let connection = connect_async(request).await;
        let (socket, _) = connection.map_err(|error| LinkError::Io(error.to_string()))?;
        let (outbound, control) = mpsc::channel(4);
        let (status_send, statuses) = mpsc::channel(8);
        Ok((
            Self {
                socket,
                control,
                statuses: status_send,
                pending: std::collections::VecDeque::new(),
                peer_ready: false,
            },
            WssControl {
                outbound,
                statuses: Mutex::new(statuses),
            },
        ))
    }

    /// Whether the relay has reported the opposite role present.
    pub fn peer_is_ready(&self) -> bool {
        self.peer_ready
    }

    /// Waits until the relay reports the opposite role present, so the
    /// handshake deadline starts only when both peers exist. `limit` bounds
    /// the wait; `None` waits until the socket closes (the lease deadline
    /// enforced by the relay still bounds it). Binary records that arrive
    /// meanwhile are retained in order for the handshake.
    pub async fn wait_for_peer(&mut self, limit: Option<Duration>) -> Result<(), LinkError> {
        if self.peer_ready {
            return Ok(());
        }
        let wait = async {
            loop {
                match self.next_frame().await? {
                    Frame::Record(record) => self.pending.push_back(record),
                    Frame::Status(RelayStatus::PeerReady) => return Ok(()),
                    Frame::Status(_) => {}
                    Frame::Closed => return Err(LinkError::Closed),
                }
            }
        };
        match limit {
            Some(limit) => timeout(limit, wait)
                .await
                .map_err(|_| LinkError::PeerUnavailable)?,
            None => wait.await,
        }
    }

    /// Reads one WebSocket frame, forwarding relay status to the owner and
    /// sending queued control messages first.
    async fn next_frame(&mut self) -> Result<Frame, LinkError> {
        loop {
            let message = tokio::select! {
                Some(control) = self.control.recv() => {
                    self.socket.send(Message::Text(control.into())).await
                        .map_err(|error| LinkError::Io(error.to_string()))?;
                    continue;
                }
                message = self.socket.next() => message,
            };
            let Some(message) = message else {
                return Ok(Frame::Closed);
            };
            match message.map_err(|error| LinkError::Io(error.to_string()))? {
                Message::Binary(record) if record.len() <= MAX_RECORD_BYTES => {
                    return Ok(Frame::Record(record.to_vec()));
                }
                Message::Binary(_) => return Err(LinkError::Limit("oversized WebSocket record")),
                Message::Close(_) => return Ok(Frame::Closed),
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Text(status) => {
                    let event = parse_relay_status(status.as_str())?;
                    match event {
                        RelayStatus::PeerReady => self.peer_ready = true,
                        RelayStatus::PeerUnavailable => self.peer_ready = false,
                        RelayStatus::LeaseStarted { .. } => {}
                    }
                    let _ = self.statuses.try_send(event.clone());
                    return Ok(Frame::Status(event));
                }
                _ => return Err(LinkError::Authentication("non-binary relay frame".into())),
            }
        }
    }
}

enum Frame {
    Record(Vec<u8>),
    Status(RelayStatus),
    Closed,
}

fn parse_relay_status(status: &str) -> Result<RelayStatus, LinkError> {
    let value: serde_json::Value = serde_json::from_str(status)
        .map_err(|_| LinkError::Authentication("invalid relay control frame".into()))?;
    let event = match value.get("type").and_then(serde_json::Value::as_str) {
        Some("peer_ready") if value.as_object().is_some_and(|v| v.len() == 1) => {
            RelayStatus::PeerReady
        }
        Some("peer_unavailable") if value.as_object().is_some_and(|v| v.len() == 1) => {
            RelayStatus::PeerUnavailable
        }
        Some("lease_started") if value.as_object().is_some_and(|v| v.len() == 3) => {
            let lease_id = value
                .get("leaseId")
                .and_then(serde_json::Value::as_str)
                .filter(|v| (16..=96).contains(&v.len()))
                .ok_or_else(|| LinkError::Authentication("invalid relay lease".into()))?;
            let expires_at = value
                .get("expiresAt")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| LinkError::Authentication("invalid relay lease".into()))?;
            RelayStatus::LeaseStarted {
                lease_id: lease_id.to_owned(),
                expires_at,
            }
        }
        _ => {
            return Err(LinkError::Authentication(
                "unexpected relay control frame".into(),
            ))
        }
    };
    Ok(event)
}

impl WssControl {
    /// Forwards one control-plane-signed extension as an exact relay control
    /// message. The helper cannot synthesize authority because it cannot sign.
    pub async fn extend_lease(&self, claim: &str) -> Result<(), LinkError> {
        let value = serde_json::json!({ "type": "lease_extension", "claim": claim });
        self.outbound
            .send(value.to_string())
            .await
            .map_err(|_| LinkError::Closed)
    }

    /// Waits for the next relay hint or opaque redeemed-lease handle.
    pub async fn next_status(&self) -> Option<RelayStatus> {
        self.statuses.lock().await.recv().await
    }
}

#[async_trait]
impl RecordIo for WssRecordIo {
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
        if record.len() > MAX_RECORD_BYTES {
            return Err(LinkError::Limit("record is larger than 65535 bytes"));
        }
        self.socket
            .send(Message::Binary(record.into()))
            .await
            .map_err(|error| LinkError::Io(error.to_string()))
    }

    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some(record) = self.pending.pop_front() {
            return Ok(Some(record));
        }
        loop {
            match self.next_frame().await? {
                Frame::Record(record) => return Ok(Some(record)),
                Frame::Status(_) => continue,
                Frame::Closed => return Ok(None),
            }
        }
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        self.socket
            .close(None)
            .await
            .map_err(|error| LinkError::Io(error.to_string()))
    }
}

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

/// One authenticated logical stream.
pub struct LogicalStream {
    inner: tokio_util::compat::Compat<Stream>,
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
            // The select above may already have consumed the transport task's
            // completion; a finished JoinHandle must not be polled again.
            if !crypt_task.is_finished() {
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
            let _ = timeout(Duration::from_secs(2), task).await;
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
            _ = &mut *crypt_task => break,
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

async fn run_handshake<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
    role: LinkRole,
) -> Result<(), LinkError> {
    match role {
        LinkRole::Controller => {
            handshake_send(records, handshake).await?;
            handshake_receive(records, handshake).await?;
            handshake_send(records, handshake).await?;
        }
        LinkRole::Host => {
            handshake_receive(records, handshake).await?;
            handshake_send(records, handshake).await?;
            handshake_receive(records, handshake).await?;
        }
    }
    Ok(())
}

async fn handshake_send<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
) -> Result<(), LinkError> {
    let mut output = vec![0_u8; MAX_RECORD_BYTES];
    let written = handshake
        .write_message(&[], &mut output)
        .map_err(auth_error)?;
    records.send_record(output[..written].to_vec()).await
}

async fn handshake_receive<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
) -> Result<(), LinkError> {
    let record = records.recv_record().await?.ok_or(LinkError::Closed)?;
    let mut payload = vec![0_u8; MAX_RECORD_BYTES];
    handshake
        .read_message(&record, &mut payload)
        .map_err(auth_error)?;
    Ok(())
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

fn prologue(config: &LinkConfig) -> Result<Vec<u8>, LinkError> {
    let mut value = b"latch-remote-link\0v1\0".to_vec();
    value.extend_from_slice(match config.purpose {
        LinkPurpose::Enrollment => b"enrollment\0",
        LinkPurpose::Session => b"session\0",
    });
    value.extend_from_slice(b"controller->host\0");
    if config.purpose == LinkPurpose::Session {
        let remote = config
            .expected_remote_public_key
            .as_ref()
            .expect("validated");
        let (controller, host) = match config.role {
            LinkRole::Controller => (&config.local_public_key, remote),
            LinkRole::Host => (remote, &config.local_public_key),
        };
        value.extend_from_slice(controller);
        value.extend_from_slice(host);
    } else {
        value.extend_from_slice(
            config
                .enrollment_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        value.push(0);
        value.extend_from_slice(config.enrollment_secret.as_ref().expect("validated"));
    }
    Ok(value)
}

fn new_hello(grant_revision: u64) -> LinkHello {
    let mut nonce = [0_u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    LinkHello {
        r#type: "link_hello".into(),
        version: 1,
        nonce: hex(&nonce),
        grant_revision,
        limits: LinkLimits::default(),
    }
}

fn validate_hello(hello: &LinkHello) -> Result<(), LinkError> {
    if hello.r#type != "link_hello" || hello.version != 1 || hello.nonce.len() != 64 {
        return Err(LinkError::Authentication("invalid LinkHello".into()));
    }
    if hello.limits != LinkLimits::default() {
        return Err(LinkError::Limit("peer selected unsupported limits"));
    }
    Ok(())
}

async fn send_encrypted_json<R: RecordIo, T: Serialize>(
    records: &mut R,
    cipher: &mut snow::TransportState,
    value: &T,
) -> Result<(), LinkError> {
    let plaintext =
        serde_json::to_vec(value).map_err(|error| LinkError::Authentication(error.to_string()))?;
    let mut output = vec![0_u8; plaintext.len() + 16];
    let written = cipher
        .write_message(&plaintext, &mut output)
        .map_err(auth_error)?;
    output.truncate(written);
    records.send_record(output).await
}

async fn recv_encrypted_json<R: RecordIo, T: for<'de> Deserialize<'de>>(
    records: &mut R,
    cipher: &mut snow::TransportState,
) -> Result<T, LinkError> {
    let record = records.recv_record().await?.ok_or(LinkError::Closed)?;
    let mut plaintext = vec![0_u8; record.len()];
    let written = cipher
        .read_message(&record, &mut plaintext)
        .map_err(auth_error)?;
    serde_json::from_slice(&plaintext[..written])
        .map_err(|error| LinkError::Authentication(error.to_string()))
}

async fn write_header<T: AsyncWrite + Unpin>(
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

async fn read_header<T: AsyncRead + Unpin>(stream: &mut T) -> Result<OpenService, LinkError> {
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

fn auth_error(error: snow::Error) -> LinkError {
    LinkError::Authentication(error.to_string())
}

fn io_error(error: std::io::Error) -> LinkError {
    LinkError::Io(error.to_string())
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemoryRecords {
        send: mpsc::Sender<Vec<u8>>,
        receive: mpsc::Receiver<Vec<u8>>,
    }

    #[async_trait]
    impl RecordIo for MemoryRecords {
        async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
            self.send.send(record).await.map_err(|_| LinkError::Closed)
        }

        async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
            Ok(self.receive.recv().await)
        }

        async fn close(&mut self) -> Result<(), LinkError> {
            Ok(())
        }
    }

    fn pair() -> (MemoryRecords, MemoryRecords) {
        let (a_send, a_receive) = mpsc::channel(8);
        let (b_send, b_receive) = mpsc::channel(8);
        (
            MemoryRecords {
                send: a_send,
                receive: b_receive,
            },
            MemoryRecords {
                send: b_send,
                receive: a_receive,
            },
        )
    }

    fn keys() -> (snow::Keypair, snow::Keypair) {
        let params = NOISE_PATTERN.parse().unwrap();
        let builder = snow::Builder::new(params);
        (
            builder.generate_keypair().unwrap(),
            builder.generate_keypair().unwrap(),
        )
    }

    fn config(role: LinkRole, own: &[u8], own_public: &[u8], peer: &[u8]) -> LinkConfig {
        LinkConfig {
            purpose: LinkPurpose::Session,
            role,
            local_private_key: Zeroizing::new(own.to_vec()),
            local_public_key: own_public.to_vec(),
            expected_remote_public_key: Some(peer.to_vec()),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 3,
            timings: LinkTimings::default(),
        }
    }

    #[tokio::test]
    async fn authenticates_pins_and_multiplexes_gateway_stream() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let controller = SecureLink::establish(
            controller_io,
            config(
                LinkRole::Controller,
                &controller_keys.private,
                &controller_keys.public,
                &host_keys.public,
            ),
        );
        let host = SecureLink::establish(
            host_io,
            config(
                LinkRole::Host,
                &host_keys.private,
                &host_keys.public,
                &controller_keys.public,
            ),
        );
        let (controller, host) = tokio::try_join!(controller, host).unwrap();
        let opened = controller.open(Service::Gateway, 3);
        let accepted = host.accept(LinkPurpose::Session);
        let (mut opened, (header, mut accepted)) = tokio::try_join!(opened, accepted).unwrap();
        assert_eq!(header.service, Service::Gateway);
        opened.write_all(b"ping").await.unwrap();
        opened.flush().await.unwrap();
        let mut value = [0; 4];
        accepted.read_exact(&mut value).await.unwrap();
        assert_eq!(&value, b"ping");
        controller.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn a_blocked_stream_does_not_stall_an_unrelated_stream() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let controller = SecureLink::establish(
            controller_io,
            config(
                LinkRole::Controller,
                &controller_keys.private,
                &controller_keys.public,
                &host_keys.public,
            ),
        );
        let host = SecureLink::establish(
            host_io,
            config(
                LinkRole::Host,
                &host_keys.private,
                &host_keys.public,
                &controller_keys.public,
            ),
        );
        let (controller, host) = tokio::try_join!(controller, host).unwrap();

        let first = controller.open(Service::Gateway, 3);
        let first_peer = host.accept(LinkPurpose::Session);
        let (mut first, (_header, _blocked_peer)) = tokio::try_join!(first, first_peer).unwrap();
        let blocked = tokio::spawn(async move {
            first
                .write_all(&vec![0x5a; MAX_RECEIVE_WINDOW_BYTES * 2])
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second = controller.open(Service::Gateway, 3);
        let second_peer = host.accept(LinkPurpose::Session);
        let (mut second, (_header, mut second_peer)) =
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::try_join!(second, second_peer)
            })
            .await
            .expect("a blocked writer stalled stream admission")
            .unwrap();
        second.write_all(b"ping").await.unwrap();
        second.flush().await.unwrap();
        let mut value = [0_u8; 4];
        tokio::time::timeout(Duration::from_secs(2), second_peer.read_exact(&mut value))
            .await
            .expect("a blocked writer stalled another stream")
            .unwrap();
        assert_eq!(&value, b"ping");

        blocked.abort();
        let _ = blocked.await;
        controller.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn wrong_pin_fails_closed() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let (_, wrong) = keys();
        let controller = SecureLink::establish(
            controller_io,
            config(
                LinkRole::Controller,
                &controller_keys.private,
                &controller_keys.public,
                &wrong.public,
            ),
        );
        let host = SecureLink::establish(
            host_io,
            config(
                LinkRole::Host,
                &host_keys.private,
                &host_keys.public,
                &controller_keys.public,
            ),
        );
        let (controller, _) = tokio::join!(controller, host);
        assert!(matches!(controller, Err(LinkError::Authentication(_))));
    }

    #[tokio::test]
    async fn admission_secret_alone_cannot_complete_enrollment() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let make = |role, own: &[u8], secret: u8| LinkConfig {
            purpose: LinkPurpose::Enrollment,
            role,
            local_private_key: Zeroizing::new(own.to_vec()),
            local_public_key: if role == LinkRole::Controller {
                controller_keys.public.clone()
            } else {
                host_keys.public.clone()
            },
            expected_remote_public_key: if role == LinkRole::Controller {
                Some(host_keys.public.clone())
            } else {
                None
            },
            enrollment_id: Some("enr_fixture_00000001".into()),
            enrollment_secret: Some(Zeroizing::new(vec![secret; 32])),
            grant_revision: 0,
            timings: LinkTimings::default(),
        };
        let controller = SecureLink::establish(
            controller_io,
            make(LinkRole::Controller, &controller_keys.private, 1),
        );
        let host = SecureLink::establish(host_io, make(LinkRole::Host, &host_keys.private, 2));
        let (controller, host) = tokio::join!(controller, host);
        assert!(controller.is_err());
        assert!(host.is_err());
    }

    #[tokio::test]
    async fn enrollment_comparison_matches_and_commits_to_the_proposed_grant() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let make = |role, own: &[u8]| LinkConfig {
            purpose: LinkPurpose::Enrollment,
            role,
            local_private_key: Zeroizing::new(own.to_vec()),
            local_public_key: if role == LinkRole::Controller {
                controller_keys.public.clone()
            } else {
                host_keys.public.clone()
            },
            expected_remote_public_key: if role == LinkRole::Controller {
                Some(host_keys.public.clone())
            } else {
                None
            },
            enrollment_id: Some("enr_fixture_00000001".into()),
            enrollment_secret: Some(Zeroizing::new(vec![7; 32])),
            grant_revision: 0,
            timings: LinkTimings::default(),
        };
        let controller = SecureLink::establish(
            controller_io,
            make(LinkRole::Controller, &controller_keys.private),
        );
        let host = SecureLink::establish(host_io, make(LinkRole::Host, &host_keys.private));
        let (controller, host) = tokio::try_join!(controller, host).unwrap();
        let controller_code = controller
            .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "control")
            .unwrap();
        let host_code = host
            .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "control")
            .unwrap();
        assert_eq!(controller_code, host_code);
        assert_ne!(
            controller_code,
            controller
                .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "observe",)
                .unwrap()
        );
        controller.close().await;
        host.close().await;
    }

    /// A carrier that can silently drop everything it is asked to send, the
    /// way a NAT that forgot a mapping does.
    struct MutableRecords {
        inner: MemoryRecords,
        muted: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl RecordIo for MutableRecords {
        async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
            if self.muted.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(());
            }
            self.inner.send_record(record).await
        }

        async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
            self.inner.recv_record().await
        }

        async fn close(&mut self) -> Result<(), LinkError> {
            Ok(())
        }
    }

    fn fast_config(role: LinkRole, own: &[u8], own_public: &[u8], peer: &[u8]) -> LinkConfig {
        let mut config = config(role, own, own_public, peer);
        config.timings = LinkTimings {
            keepalive_interval: Duration::from_millis(60),
            dead_peer_timeout: Duration::from_millis(250),
        };
        config
    }

    #[tokio::test]
    async fn keepalives_keep_an_idle_link_alive_and_silence_kills_it() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let muted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let controller_io = MutableRecords {
            inner: controller_io,
            muted: muted.clone(),
        };
        let controller = SecureLink::establish(
            controller_io,
            fast_config(
                LinkRole::Controller,
                &controller_keys.private,
                &controller_keys.public,
                &host_keys.public,
            ),
        );
        let host = SecureLink::establish(
            host_io,
            fast_config(
                LinkRole::Host,
                &host_keys.private,
                &host_keys.public,
                &controller_keys.public,
            ),
        );
        let (controller, host) = tokio::try_join!(controller, host).unwrap();

        // Well past the dead-peer bound with no application traffic: the
        // keepalive records are what keep both ends alive.
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(!host.is_closed());
        assert!(!controller.is_closed());
        let opened = controller.open(Service::Gateway, 3);
        let accepted = host.accept(LinkPurpose::Session);
        let (mut opened, (_, mut accepted)) = tokio::try_join!(opened, accepted).unwrap();
        opened.write_all(b"alive").await.unwrap();
        opened.flush().await.unwrap();
        let mut value = [0; 5];
        accepted.read_exact(&mut value).await.unwrap();
        assert_eq!(&value, b"alive");

        // Now the controller's records vanish silently. The host must notice
        // within its dead-peer bound and every accept/read fails closed.
        muted.store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(2), host.closed())
            .await
            .expect("host did not detect the silent peer");
        assert!(host.is_closed());
        let mut rest = Vec::new();
        assert!(accepted.read_to_end(&mut rest).await.is_err() || rest.is_empty());
        assert!(host.accept(LinkPurpose::Session).await.is_err());
        controller.close().await;
    }

    #[tokio::test]
    async fn closed_resolves_after_a_local_close() {
        let (controller_io, host_io) = pair();
        let (controller_keys, host_keys) = keys();
        let controller = SecureLink::establish(
            controller_io,
            config(
                LinkRole::Controller,
                &controller_keys.private,
                &controller_keys.public,
                &host_keys.public,
            ),
        );
        let host = SecureLink::establish(
            host_io,
            config(
                LinkRole::Host,
                &host_keys.private,
                &host_keys.public,
                &controller_keys.public,
            ),
        );
        let (controller, host) = tokio::try_join!(controller, host).unwrap();
        assert!(!controller.is_closed());
        controller.close().await;
        tokio::time::timeout(Duration::from_secs(2), controller.closed())
            .await
            .unwrap();
        assert!(controller.is_closed());
        // The peer learns through the carrier ending, not through any hint.
        tokio::time::timeout(Duration::from_secs(2), host.closed())
            .await
            .expect("peer closure was not observed");
        host.close().await;
    }

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
