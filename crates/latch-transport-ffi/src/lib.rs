//! Swift-facing API for Latch Remote Link.

use std::sync::Arc;

use latch_transport::link::{
    LanRecordIo, LinkConfig as CoreLinkConfig, LinkError, LinkPurpose as CoreLinkPurpose,
    LinkRole as CoreLinkRole, LinkTimings, LogicalStream, RelayStatus as CoreRelayStatus,
    SecureLink, Service as CoreLinkService, WssControl, WssRecordIo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

uniffi::setup_scaffolding!();

/// Swift-visible Remote Link failure.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TransportError {
    /// Invalid state transition.
    #[error("the transport is not ready for this operation")]
    InvalidState,
    /// The relay admitted this endpoint but the paired peer never arrived
    /// within the wait bound. The Mac is offline, asleep, or not yet re-admitted.
    #[error("the paired Mac is not connected to the relay")]
    PeerUnavailable,
    /// Peer authentication, pin, purpose, or protocol validation failed.
    /// Automatic retry must stop until pairing state changes.
    #[error("{message}")]
    Authentication {
        /// Human-readable failure.
        message: String,
    },
    /// A deadline passed: handshake, or silence past the dead-peer bound.
    #[error("the secure link timed out")]
    Timeout,
    /// Rust transport stack failure.
    #[error("{message}")]
    Failure {
        /// Human-readable failure.
        message: String,
    },
}

/// Content-free stage durations for one link establishment.
#[derive(Clone, Copy, Debug, Default, uniffi::Record)]
pub struct RemoteLinkStageTimings {
    /// Transport connect (TCP/TLS/WebSocket upgrade, or LAN TCP connect).
    pub connect_ms: u64,
    /// Time waiting for the relay to report the opposite role present.
    pub peer_wait_ms: u64,
    /// Noise XX plus LinkHello.
    pub authenticate_ms: u64,
}

/// Cryptographically bound Remote Link purpose.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum RemoteLinkPurpose {
    Enrollment,
    Session,
}

/// Fixed endpoint role; the controller initiates Noise and Yamux.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum RemoteLinkRole {
    Host,
    Controller,
}

/// Logical service allowed after authentication.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum RemoteLinkService {
    Gateway,
    Enrollment,
    Control,
}

/// Untrusted relay status surfaced to the endpoint owner.
#[derive(Clone, uniffi::Record)]
pub struct RemoteRelayEvent {
    /// `peer_ready`, `peer_unavailable`, or `lease_started`.
    pub kind: String,
    /// Present only for `lease_started`.
    pub lease_id: Option<String>,
    /// Present only for `lease_started`.
    pub expires_at: Option<u64>,
}

/// One authenticated, multiplexed WSS or LAN link owned by Rust.
#[derive(uniffi::Object)]
pub struct RemoteLink {
    link: Arc<SecureLink>,
    control: Option<Arc<WssControl>>,
    timings: RemoteLinkStageTimings,
}

/// One independently closable logical service stream on a shared Remote Link.
#[derive(uniffi::Object)]
pub struct RemoteStream {
    stream: Mutex<Option<LogicalStream>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl RemoteLink {
    /// Opens WSS, waits for the relay to report the peer present (up to
    /// `peer_wait_ms`; zero waits until the relay closes the socket),
    /// authenticates Noise XX, checks the exact peer pin, and starts Yamux.
    /// The 10-second handshake deadline starts only once both peers exist.
    #[uniffi::constructor]
    #[allow(clippy::too_many_arguments)]
    pub async fn connect_wss(
        url: String,
        admission: String,
        purpose: RemoteLinkPurpose,
        role: RemoteLinkRole,
        local_private_key: Vec<u8>,
        local_public_key: Vec<u8>,
        expected_remote_public_key: Option<Vec<u8>>,
        enrollment_id: Option<String>,
        enrollment_secret: Option<Vec<u8>>,
        grant_revision: u64,
        peer_wait_ms: u64,
    ) -> Result<Arc<Self>, TransportError> {
        let started = std::time::Instant::now();
        let (mut records, control) = WssRecordIo::connect_with_control(&url, &admission)
            .await
            .map_err(failure)?;
        let connect_ms = started.elapsed().as_millis() as u64;
        let limit = (peer_wait_ms > 0).then(|| std::time::Duration::from_millis(peer_wait_ms));
        records.wait_for_peer(limit).await.map_err(failure)?;
        let peer_wait_ms = started.elapsed().as_millis() as u64 - connect_ms;
        let link = SecureLink::establish(
            records,
            CoreLinkConfig {
                purpose: purpose.into(),
                role: role.into(),
                local_private_key: Zeroizing::new(local_private_key),
                local_public_key,
                expected_remote_public_key,
                enrollment_id,
                enrollment_secret: enrollment_secret.map(Zeroizing::new),
                grant_revision,
                timings: LinkTimings::default(),
            },
        )
        .await
        .map_err(failure)?;
        let timings = RemoteLinkStageTimings {
            connect_ms,
            peer_wait_ms,
            authenticate_ms: link.authenticate_ms,
        };
        Ok(Arc::new(Self {
            link,
            control: Some(Arc::new(control)),
            timings,
        }))
    }

    /// Connects the same authenticated protocol over bounded LAN framing.
    #[uniffi::constructor]
    #[allow(clippy::too_many_arguments)]
    pub async fn connect_lan(
        host: String,
        port: u16,
        purpose: RemoteLinkPurpose,
        role: RemoteLinkRole,
        local_private_key: Vec<u8>,
        local_public_key: Vec<u8>,
        expected_remote_public_key: Option<Vec<u8>>,
        enrollment_id: Option<String>,
        enrollment_secret: Option<Vec<u8>>,
        grant_revision: u64,
    ) -> Result<Arc<Self>, TransportError> {
        if host.is_empty() || host.len() > 255 || port == 0 {
            return Err(TransportError::Failure {
                message: "invalid LAN target".into(),
            });
        }
        let started = std::time::Instant::now();
        let stream = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|error| TransportError::Failure {
            message: error.to_string(),
        })?;
        let connect_ms = started.elapsed().as_millis() as u64;
        let link = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            SecureLink::establish(
                LanRecordIo::new(stream),
                CoreLinkConfig {
                    purpose: purpose.into(),
                    role: role.into(),
                    local_private_key: Zeroizing::new(local_private_key),
                    local_public_key,
                    expected_remote_public_key,
                    enrollment_id,
                    enrollment_secret: enrollment_secret.map(Zeroizing::new),
                    grant_revision,
                    timings: LinkTimings::default(),
                },
            ),
        )
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(failure)?;
        let timings = RemoteLinkStageTimings {
            connect_ms,
            peer_wait_ms: 0,
            authenticate_ms: link.authenticate_ms,
        };
        Ok(Arc::new(Self {
            link,
            control: None,
            timings,
        }))
    }

    /// Content-free establishment stage timings for diagnostics.
    pub fn stage_timings(&self) -> RemoteLinkStageTimings {
        self.timings
    }

    /// Resolves once the link has stopped for any reason. Owners await this
    /// to schedule reconnection without holding an open stream.
    pub async fn wait_closed(&self) {
        self.link.closed().await;
    }

    /// Whether the link has already stopped.
    pub fn is_closed(&self) -> bool {
        self.link.is_closed()
    }

    /// Opens one bounded service stream on the shared encrypted link.
    pub async fn open_service(
        &self,
        service: RemoteLinkService,
        grant_revision: u64,
    ) -> Result<Arc<RemoteStream>, TransportError> {
        let stream = self
            .link
            .open(service.into(), grant_revision)
            .await
            .map_err(failure)?;
        Ok(Arc::new(RemoteStream {
            stream: Mutex::new(Some(stream)),
        }))
    }

    /// Waits for an untrusted relay hint or the opaque redeemed lease handle.
    pub async fn next_relay_event(&self) -> Option<RemoteRelayEvent> {
        let control = self.control.as_ref()?;
        control.next_status().await.map(|event| match event {
            CoreRelayStatus::PeerReady => RemoteRelayEvent {
                kind: "peer_ready".into(),
                lease_id: None,
                expires_at: None,
            },
            CoreRelayStatus::PeerUnavailable => RemoteRelayEvent {
                kind: "peer_unavailable".into(),
                lease_id: None,
                expires_at: None,
            },
            CoreRelayStatus::LeaseStarted {
                lease_id,
                expires_at,
            } => RemoteRelayEvent {
                kind: "lease_started".into(),
                lease_id: Some(lease_id),
                expires_at: Some(expires_at),
            },
        })
    }

    /// Forwards a control-plane-signed extension over the WSS control channel.
    pub async fn extend_lease(&self, claim: String) -> Result<(), TransportError> {
        self.control
            .as_ref()
            .ok_or(TransportError::InvalidState)?
            .extend_lease(&claim)
            .await
            .map_err(failure)
    }

    /// Returns the owner comparison code for an authenticated enrollment
    /// transcript and exact proposed grant.
    pub fn enrollment_comparison(
        &self,
        enrollment_id: String,
        controller_public_key: Vec<u8>,
        permission: String,
    ) -> Result<String, TransportError> {
        self.link
            .enrollment_comparison(&enrollment_id, &controller_public_key, &permission)
            .map_err(failure)
    }

    /// Cancels and joins all link-owned work.
    pub async fn close(&self) -> Result<(), TransportError> {
        self.link.close().await;
        Ok(())
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RemoteStream {
    /// Writes application bytes to this logical stream.
    pub async fn write(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        let mut stream = self.stream.lock().await;
        stream
            .as_mut()
            .ok_or(TransportError::InvalidState)?
            .write_all(&bytes)
            .await
            .map_err(io_failure)
    }

    /// Reads at most 16 KiB; an empty result is normal EOF.
    pub async fn read(&self) -> Result<Vec<u8>, TransportError> {
        let mut stream = self.stream.lock().await;
        let stream = stream.as_mut().ok_or(TransportError::InvalidState)?;
        let mut bytes = vec![0_u8; 16 * 1024];
        let read = stream.read(&mut bytes).await.map_err(io_failure)?;
        bytes.truncate(read);
        Ok(bytes)
    }

    /// Gracefully closes only this logical stream, retaining the shared link.
    pub async fn close(&self) -> Result<(), TransportError> {
        if let Some(mut stream) = self.stream.lock().await.take() {
            stream.shutdown().await.map_err(io_failure)?;
        }
        Ok(())
    }
}

impl From<RemoteLinkPurpose> for CoreLinkPurpose {
    fn from(value: RemoteLinkPurpose) -> Self {
        match value {
            RemoteLinkPurpose::Enrollment => Self::Enrollment,
            RemoteLinkPurpose::Session => Self::Session,
        }
    }
}

impl From<RemoteLinkRole> for CoreLinkRole {
    fn from(value: RemoteLinkRole) -> Self {
        match value {
            RemoteLinkRole::Host => Self::Host,
            RemoteLinkRole::Controller => Self::Controller,
        }
    }
}

impl From<RemoteLinkService> for CoreLinkService {
    fn from(value: RemoteLinkService) -> Self {
        match value {
            RemoteLinkService::Gateway => Self::Gateway,
            RemoteLinkService::Enrollment => Self::Enrollment,
            RemoteLinkService::Control => Self::Control,
        }
    }
}

fn io_failure(error: std::io::Error) -> TransportError {
    TransportError::Failure {
        message: error.to_string(),
    }
}

fn failure(error: LinkError) -> TransportError {
    match error {
        LinkError::PeerUnavailable => TransportError::PeerUnavailable,
        LinkError::Authentication(message) => TransportError::Authentication { message },
        LinkError::Timeout => TransportError::Timeout,
        other => TransportError::Failure {
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use latch_transport::link::{LinkConfig, LinkError, RecordIo};
    use tokio::sync::mpsc;

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
        let (left_send, left_receive) = mpsc::channel(8);
        let (right_send, right_receive) = mpsc::channel(8);
        (
            MemoryRecords {
                send: left_send,
                receive: right_receive,
            },
            MemoryRecords {
                send: right_send,
                receive: left_receive,
            },
        )
    }

    fn config(role: CoreLinkRole, keys: &snow::Keypair, peer: &snow::Keypair) -> LinkConfig {
        LinkConfig {
            purpose: CoreLinkPurpose::Session,
            role,
            local_private_key: Zeroizing::new(keys.private.clone()),
            local_public_key: keys.public.clone(),
            expected_remote_public_key: Some(peer.public.clone()),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 1,
            timings: LinkTimings::default(),
        }
    }

    #[tokio::test]
    async fn invalid_lan_target_fails_before_using_key_material() {
        let result = RemoteLink::connect_lan(
            String::new(),
            0,
            RemoteLinkPurpose::Session,
            RemoteLinkRole::Controller,
            vec![],
            vec![],
            None,
            None,
            None,
            1,
        )
        .await;
        let error = match result {
            Ok(_) => panic!("invalid target was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "invalid LAN target");
    }

    #[tokio::test]
    async fn ffi_stream_objects_share_one_multiplexed_link() {
        let builder = snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap());
        let controller_keys = builder.generate_keypair().unwrap();
        let host_keys = builder.generate_keypair().unwrap();
        let (controller_io, host_io) = pair();
        let controller = SecureLink::establish(
            controller_io,
            config(CoreLinkRole::Controller, &controller_keys, &host_keys),
        );
        let host = SecureLink::establish(
            host_io,
            config(CoreLinkRole::Host, &host_keys, &controller_keys),
        );
        let (controller, host) = tokio::try_join!(controller, host).unwrap();
        let ffi = RemoteLink {
            link: controller,
            control: None,
            timings: RemoteLinkStageTimings::default(),
        };

        let first = ffi.open_service(RemoteLinkService::Gateway, 1);
        let first_peer = host.accept(CoreLinkPurpose::Session);
        let (first, first_peer) = tokio::join!(first, first_peer);
        let first = first.unwrap();
        let (_header, _first_peer) = first_peer.unwrap();
        first.close().await.unwrap();

        let second = ffi.open_service(RemoteLinkService::Gateway, 1);
        let second_peer = host.accept(CoreLinkPurpose::Session);
        let (second, second_peer) = tokio::join!(second, second_peer);
        let second = second.unwrap();
        let (_header, mut second_peer) = second_peer.unwrap();
        second.write(b"multiplexed".to_vec()).await.unwrap();
        let mut received = [0_u8; 11];
        second_peer.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"multiplexed");

        second.close().await.unwrap();
        // Peer closure is observable without an open stream, which is what
        // the app-scoped owner uses to schedule reconnection.
        assert!(!ffi.is_closed());
        host.close().await;
        tokio::time::timeout(std::time::Duration::from_secs(2), ffi.wait_closed())
            .await
            .expect("peer closure was not observed by the FFI owner");
        assert!(ffi.is_closed());
        ffi.close().await.unwrap();
    }
}
