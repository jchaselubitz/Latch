//! System-validated WSS relay carrier and its relay control side channel.

use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
#[cfg(feature = "test-ca")]
use tokio_tungstenite::{connect_async_tls_with_config, Connector};

use super::config::CLOSE_DRAIN_LIMIT;
use super::{
    LinkError, RecordIo, MAX_PENDING_RECORDS, MAX_RECEIVE_WINDOW_BYTES, MAX_RECORD_BYTES,
    RELAY_CONNECT_TIMEOUT, RELAY_INACTIVITY_TIMEOUT, RELAY_WRITE_TIMEOUT,
};

/// System-validated WSS carrier. Admission is only accepted in the
/// Authorization header and never appears in a URL or loggable query string.
pub struct WssRecordIo {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    control: mpsc::Receiver<String>,
    statuses: mpsc::Sender<RelayStatus>,
    /// Binary records that arrived while waiting for a relay status frame.
    pending: std::collections::VecDeque<Vec<u8>>,
    /// Total bytes held in `pending`, bounded by [`MAX_RECEIVE_WINDOW_BYTES`].
    pending_bytes: usize,
    /// Set once the relay reported the opposite role present.
    peer_ready: bool,
    /// Silence bound applied to every carrier read and write. `None` disables
    /// it; production always keeps a bound.
    inactivity: Option<Duration>,
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
        Self::connect_with(url, admission, None, false).await
    }

    /// Test-only system-TLS path with one explicit private CA. This method is
    /// absent from production builds, so shipping code cannot weaken trust.
    /// It is also the only path that accepts cleartext `ws://`, and then only
    /// to a loopback host.
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
        Self::connect_with(url, admission, Some(Connector::NativeTls(connector)), true).await
    }

    async fn connect_with(
        url: &str,
        admission: &str,
        #[cfg(feature = "test-ca")] connector: Option<Connector>,
        #[cfg(not(feature = "test-ca"))] _connector: Option<()>,
        allow_loopback_ws: bool,
    ) -> Result<(Self, WssControl), LinkError> {
        let mut request = url
            .into_client_request()
            .map_err(|_| LinkError::Configuration("invalid relay url"))?;
        // The admission bearer is attached only after the scheme is known to
        // be TLS, so a downgraded URL never carries it in cleartext.
        require_relay_scheme(request.uri(), allow_loopback_ws)?;
        let value = format!("Bearer {admission}")
            .parse()
            .map_err(|_| LinkError::Configuration("invalid admission header"))?;
        request.headers_mut().insert(AUTHORIZATION, value);
        #[cfg(feature = "test-ca")]
        let connection = timeout(RELAY_CONNECT_TIMEOUT, async {
            if connector.is_some() {
                connect_async_tls_with_config(request, None, false, connector).await
            } else {
                connect_async(request).await
            }
        })
        .await;
        #[cfg(not(feature = "test-ca"))]
        let connection = timeout(RELAY_CONNECT_TIMEOUT, connect_async(request)).await;
        let connection = connection.map_err(|_| LinkError::Timeout)?;
        let (socket, _) = connection.map_err(|error| LinkError::Io(error.to_string()))?;
        Ok(Self::from_socket(socket))
    }

    fn from_socket(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> (Self, WssControl) {
        let (outbound, control) = mpsc::channel(4);
        let (status_send, statuses) = mpsc::channel(8);
        (
            Self {
                socket,
                control,
                statuses: status_send,
                pending: std::collections::VecDeque::new(),
                pending_bytes: 0,
                peer_ready: false,
                inactivity: Some(RELAY_INACTIVITY_TIMEOUT),
            },
            WssControl {
                outbound,
                statuses: Mutex::new(statuses),
            },
        )
    }

    /// Whether the relay has reported the opposite role present.
    pub fn peer_is_ready(&self) -> bool {
        self.peer_ready
    }

    /// Overrides the carrier silence bound. Tests shorten it to exercise the
    /// same recovery path in milliseconds; `None` removes the bound entirely
    /// and is only for a carrier whose peer is known to send nothing.
    pub fn set_inactivity_timeout(&mut self, limit: Option<Duration>) {
        self.inactivity = limit;
    }

    /// Waits until the relay reports the opposite role present, so the
    /// handshake deadline starts only when both peers exist. `limit` bounds
    /// the total wait; `None` waits for as long as the relay keeps the carrier
    /// audibly alive. Either way the carrier silence bound applies, so a peer
    /// that never arrives is waited out indefinitely while a relay that goes
    /// silent fails with [`LinkError::Timeout`] instead of stranding the
    /// caller. Binary records that arrive meanwhile are retained in order for
    /// the handshake, up to [`MAX_PENDING_RECORDS`] records or
    /// [`MAX_RECEIVE_WINDOW_BYTES`] bytes; past either bound the carrier is
    /// closed and the wait fails with [`LinkError::Limit`].
    pub async fn wait_for_peer(&mut self, limit: Option<Duration>) -> Result<(), LinkError> {
        if self.peer_ready {
            return Ok(());
        }
        let wait = async {
            loop {
                match self.next_frame().await? {
                    Frame::Record(record) => self.retain_pending(record).await?,
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

    /// Holds one record that arrived before the opposite role, or closes the
    /// carrier if doing so would exceed the pre-handshake bound.
    async fn retain_pending(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
        if self.pending.len() >= MAX_PENDING_RECORDS
            || self.pending_bytes + record.len() > MAX_RECEIVE_WINDOW_BYTES
        {
            self.pending.clear();
            self.pending_bytes = 0;
            let _ = timeout(CLOSE_DRAIN_LIMIT, self.socket.close(None)).await;
            return Err(LinkError::Limit("pre-handshake buffer exceeded"));
        }
        self.pending_bytes += record.len();
        self.pending.push_back(record);
        Ok(())
    }

    /// Reads one WebSocket frame, forwarding relay status to the owner and
    /// sending queued control messages first.
    async fn next_frame(&mut self) -> Result<Frame, LinkError> {
        loop {
            // The bound is re-armed on every iteration, so any inbound traffic
            // counts as liveness: a relay ping the loop discards is still
            // proof the path delivers, and only total silence expires.
            let deadline = self
                .inactivity
                .map(|limit| tokio::time::Instant::now() + limit);
            let message = tokio::select! {
                Some(control) = self.control.recv() => {
                    let send = self.socket.send(Message::Text(control.into()));
                    match self.inactivity {
                        Some(_) => timeout(RELAY_WRITE_TIMEOUT, send)
                            .await
                            .map_err(|_| LinkError::Timeout)?,
                        None => send.await,
                    }
                    .map_err(|error| LinkError::Io(error.to_string()))?;
                    continue;
                }
                message = self.socket.next() => message,
                () = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if deadline.is_some() => return Err(LinkError::Timeout),
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

/// Relay carriers must be TLS. Cleartext is accepted only when the caller is
/// the test-only CA seam and the host is loopback.
fn require_relay_scheme(
    uri: &tokio_tungstenite::tungstenite::http::Uri,
    allow_loopback_ws: bool,
) -> Result<(), LinkError> {
    match uri.scheme_str() {
        Some("wss") => Ok(()),
        Some("ws") if allow_loopback_ws && uri.host().is_some_and(is_loopback_host) => Ok(()),
        _ => Err(LinkError::Configuration("relay url must use wss://")),
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
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
        let send = self.socket.send(Message::Binary(record.into()));
        match self.inactivity {
            Some(_) => timeout(RELAY_WRITE_TIMEOUT, send)
                .await
                .map_err(|_| LinkError::Timeout)?,
            None => send.await,
        }
        .map_err(|error| LinkError::Io(error.to_string()))
    }

    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some(record) = self.pending.pop_front() {
            self.pending_bytes -= record.len();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_connect_refuses_cleartext_before_any_socket() {
        // `.invalid` never resolves, so a DNS or connect attempt would surface
        // as Io or Timeout; only the scheme check yields Configuration.
        for url in [
            "ws://relay.invalid/v1/connect",
            "ws://127.0.0.1:9/v1/connect",
            "http://relay.invalid/v1/connect",
        ] {
            let started = std::time::Instant::now();
            let result = WssRecordIo::connect(url, "admission").await;
            assert!(
                matches!(
                    result,
                    Err(LinkError::Configuration("relay url must use wss://"))
                ),
                "{url}: {:?}",
                result.err()
            );
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }

    #[test]
    fn relay_scheme_allows_ws_only_to_loopback_through_the_test_seam() {
        let check = |url: &str, allow: bool| {
            let request = url.into_client_request().unwrap();
            require_relay_scheme(request.uri(), allow).is_ok()
        };
        assert!(check("wss://relay.example/v1/connect", false));
        assert!(!check("ws://127.0.0.1:8080/", false));
        assert!(check("ws://127.0.0.1:8080/", true));
        assert!(check("ws://localhost:8080/", true));
        assert!(check("ws://[::1]:8080/", true));
        assert!(!check("ws://relay.example/", true));
        assert!(!check("ws://10.0.0.5/", true));
        assert!(!check("ws://127.0.0.1.relay.example/", true));
    }

    /// Loopback relay carrier whose server side streams `records` binary
    /// frames of `record_len` bytes and never reports `peer_ready`.
    async fn flooding_relay(record_len: usize, records: usize) -> WssRecordIo {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            for _ in 0..records {
                let frame = Message::Binary(vec![7_u8; record_len].into());
                if socket.send(frame).await.is_err() {
                    return;
                }
            }
            // Hold the socket open silently until the client closes it.
            while let Some(Ok(_)) = socket.next().await {}
        });
        let tcp = TcpStream::connect(address).await.unwrap();
        let (socket, _) =
            tokio_tungstenite::client_async(format!("ws://{address}/"), MaybeTlsStream::Plain(tcp))
                .await
                .unwrap();
        WssRecordIo::from_socket(socket).0
    }

    #[tokio::test]
    async fn pre_handshake_flood_without_peer_ready_is_refused() {
        for record_len in [16, MAX_RECORD_BYTES] {
            let mut records = flooding_relay(record_len, MAX_PENDING_RECORDS * 4).await;
            let error = records
                .wait_for_peer(Some(Duration::from_secs(10)))
                .await
                .unwrap_err();
            assert!(
                matches!(error, LinkError::Limit("pre-handshake buffer exceeded")),
                "{record_len}: {error:?}"
            );
            // The overflow released what was held and closed the carrier.
            assert_eq!((records.pending.len(), records.pending_bytes), (0, 0));
            assert!(records.send_record(vec![0]).await.is_err());
        }
    }

    #[tokio::test]
    async fn pre_handshake_buffer_never_grows_past_its_bounds() {
        // Record bound: exactly MAX_PENDING_RECORDS are held, the next fails.
        let mut records = flooding_relay(1, 0).await;
        for _ in 0..MAX_PENDING_RECORDS {
            records.retain_pending(vec![1; 16]).await.unwrap();
        }
        assert_eq!(records.pending.len(), MAX_PENDING_RECORDS);
        assert!(matches!(
            records.retain_pending(vec![1; 16]).await,
            Err(LinkError::Limit(_))
        ));
        assert_eq!((records.pending.len(), records.pending_bytes), (0, 0));

        // Byte bound: independent of the record count.
        let mut records = flooding_relay(1, 0).await;
        let half = MAX_RECEIVE_WINDOW_BYTES / 2 + 1;
        records.retain_pending(vec![1; half]).await.unwrap();
        assert_eq!(records.pending_bytes, half);
        assert!(matches!(
            records.retain_pending(vec![1; half]).await,
            Err(LinkError::Limit(_))
        ));
        assert_eq!((records.pending.len(), records.pending_bytes), (0, 0));

        // Held bytes are released as the handshake drains them.
        let mut records = flooding_relay(1, 0).await;
        records.retain_pending(vec![1; 10]).await.unwrap();
        records.retain_pending(vec![2; 20]).await.unwrap();
        assert_eq!(records.recv_record().await.unwrap(), Some(vec![1; 10]));
        assert_eq!(records.pending_bytes, 20);
    }
}
