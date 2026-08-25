//! Generated Swift-facing API for `latch-transport`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use latch_transport::policy::{IceServer as CoreIceServer, SelectedPath as CoreSelectedPath};
use latch_transport::rtc::{
    IceCredentials as CoreCredentials, LocalDescription as CoreLocalDescription,
    RemoteDescription as CoreRemoteDescription, Role as CoreRole, RtcConnection, RtcEndpoint,
    RtcError, TransportCandidate as CoreCandidate,
};
use tokio::sync::Mutex;

uniffi::setup_scaffolding!();

/// STUN or TURN service supplied by the control plane.
#[derive(Clone, uniffi::Record)]
pub struct IceServer {
    /// Service URL.
    pub url: String,
    /// TURN username (empty for STUN).
    pub username: String,
    /// TURN credential (empty for STUN).
    pub credential: String,
}

/// ICE username fragment and password.
#[derive(Clone, uniffi::Record)]
pub struct IceCredentials {
    /// Username fragment.
    pub ufrag: String,
    /// Password.
    pub password: String,
}

/// Structured candidate exchanged through Latch signaling.
#[derive(Clone, uniffi::Record)]
pub struct TransportCandidate {
    /// Candidate type.
    pub candidate_type: String,
    /// Candidate priority.
    pub priority: u32,
    /// Foundation.
    pub foundation: String,
    /// Component number.
    pub component: u16,
    /// UDP or TCP.
    pub protocol: String,
    /// IP literal and port.
    pub address: String,
    /// Related IP literal.
    pub related_address: Option<String>,
    /// Related port.
    pub related_port: Option<u16>,
    /// TCP candidate type.
    pub tcp_type: Option<String>,
}

/// Gathered local signaling payload.
#[derive(Clone, uniffi::Record)]
pub struct LocalDescription {
    /// Agent-lifetime credentials.
    pub credentials: IceCredentials,
    /// Gathered candidates.
    pub candidates: Vec<TransportCandidate>,
}

/// Remote signaling payload.
#[derive(Clone, uniffi::Record)]
pub struct RemoteDescription {
    /// Peer credentials.
    pub credentials: IceCredentials,
    /// Peer candidates.
    pub candidates: Vec<TransportCandidate>,
}

/// Which side starts DCEP.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum TransportRole {
    /// Phone/controller.
    Initiator,
    /// Mac/helper.
    Responder,
}

/// Nominated ICE path.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum SelectedPath {
    /// Host or reflexive pair.
    Direct,
    /// TURN relay pair.
    Relay,
}

/// Swift-visible transport failure.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TransportError {
    /// Invalid state transition.
    #[error("the transport is not ready for this operation")]
    InvalidState,
    /// Rust transport stack failure.
    #[error("{message}")]
    Failure {
        /// Human-readable failure.
        message: String,
    },
}

/// Owned endpoint/connection handle generated into the XCFramework's Swift module.
#[derive(uniffi::Object)]
pub struct RemoteTransport {
    endpoint: Mutex<Option<RtcEndpoint>>,
    connection: Mutex<Option<Arc<RtcConnection>>>,
    local: RwLock<LocalDescription>,
    credentials: IceCredentials,
    connectivity_failure: AtomicBool,
}

#[uniffi::export(async_runtime = "tokio")]
impl RemoteTransport {
    /// Gathers local candidates from every supplied server.
    ///
    /// TURN URLs are accepted here: relay candidates are gathered alongside
    /// host and reflexive ones, and ICE's pair priority still nominates a
    /// direct pair whenever one completes. A caller with no relay credentials
    /// — an account with relay disabled, or a credential service that is
    /// down — passes STUN alone and gathers direct-only.
    #[uniffi::constructor]
    pub async fn gather(
        credentials: IceCredentials,
        servers: Vec<IceServer>,
    ) -> Result<Arc<Self>, TransportError> {
        let (endpoint, local) = RtcEndpoint::gather(
            credentials.clone().into(),
            &servers.into_iter().map(Into::into).collect::<Vec<_>>(),
        )
        .await
        .map_err(failure)?;
        Ok(Arc::new(Self {
            endpoint: Mutex::new(Some(endpoint)),
            connection: Mutex::new(None),
            local: RwLock::new(local.into()),
            credentials,
            connectivity_failure: AtomicBool::new(false),
        }))
    }

    /// Returns the payload to publish in presence/rendezvous.
    pub fn local_description(&self) -> LocalDescription {
        self.local
            .read()
            .expect("local description lock poisoned")
            .clone()
    }

    /// Whether the failed attempt was a transport failure worth retrying.
    ///
    /// This no longer gates relay issuance — it reports whether ICE itself
    /// failed, so a caller whose first attempt ran without TURN can tell a
    /// connectivity failure from a misuse of this object.
    pub fn connectivity_failed(&self) -> bool {
        self.connectivity_failure.load(Ordering::Acquire)
    }

    /// Re-gathers with TURN after an attempt that had no relay servers.
    pub async fn retry_with_relay(
        &self,
        servers: Vec<IceServer>,
    ) -> Result<LocalDescription, TransportError> {
        require_turn(&servers)?;
        let (endpoint, local) = RtcEndpoint::gather(
            self.credentials.clone().into(),
            &servers.into_iter().map(Into::into).collect::<Vec<_>>(),
        )
        .await
        .map_err(failure)?;
        let local: LocalDescription = local.into();
        *self.endpoint.lock().await = Some(endpoint);
        *self.local.write().expect("local description lock poisoned") = local.clone();
        Ok(local)
    }

    /// Establishes the reliable ordered channel.
    pub async fn connect(
        &self,
        remote: RemoteDescription,
        role: TransportRole,
    ) -> Result<SelectedPath, TransportError> {
        let endpoint = self
            .endpoint
            .lock()
            .await
            .take()
            .ok_or(TransportError::InvalidState)?;
        let connection = match endpoint.connect(remote.into(), role.into()).await {
            Ok(connection) => connection,
            Err(error) => {
                if matches!(&error, RtcError::Stack(_)) {
                    self.connectivity_failure.store(true, Ordering::Release);
                }
                return Err(failure(error));
            }
        };
        let path = connection.selected_path().into();
        *self.connection.lock().await = Some(Arc::new(connection));
        Ok(path)
    }

    /// Writes one Noise ciphertext record.
    pub async fn write(&self, record: Vec<u8>) -> Result<(), TransportError> {
        let connection = self
            .connection
            .lock()
            .await
            .clone()
            .ok_or(TransportError::InvalidState)?;
        connection.write(&record).await.map_err(failure)
    }

    /// Reads one Noise ciphertext record.
    pub async fn read(&self) -> Result<Vec<u8>, TransportError> {
        let connection = self
            .connection
            .lock()
            .await
            .clone()
            .ok_or(TransportError::InvalidState)?;
        connection.read().await.map_err(failure)
    }

    /// Returns the currently nominated ICE path, including migrations.
    pub async fn selected_path(&self) -> Result<SelectedPath, TransportError> {
        let connection = self
            .connection
            .lock()
            .await
            .clone()
            .ok_or(TransportError::InvalidState)?;
        Ok(connection.selected_path().into())
    }

    /// Closes the channel and ICE agent.
    pub async fn close(&self) -> Result<(), TransportError> {
        let connection = self.connection.lock().await.take();
        if let Some(connection) = connection {
            connection.close().await.map_err(failure)?;
        }
        Ok(())
    }
}

fn failure(error: impl std::fmt::Display) -> TransportError {
    TransportError::Failure {
        message: error.to_string(),
    }
}

impl From<IceServer> for CoreIceServer {
    fn from(value: IceServer) -> Self {
        Self {
            url: value.url,
            username: value.username,
            credential: value.credential,
        }
    }
}
impl From<IceCredentials> for CoreCredentials {
    fn from(value: IceCredentials) -> Self {
        Self {
            ufrag: value.ufrag,
            password: value.password,
        }
    }
}
impl From<CoreCredentials> for IceCredentials {
    fn from(value: CoreCredentials) -> Self {
        Self {
            ufrag: value.ufrag,
            password: value.password,
        }
    }
}
impl From<TransportCandidate> for CoreCandidate {
    fn from(value: TransportCandidate) -> Self {
        Self {
            candidate_type: value.candidate_type,
            priority: value.priority,
            foundation: value.foundation,
            component: value.component,
            protocol: value.protocol,
            address: value.address,
            related_address: value.related_address,
            related_port: value.related_port,
            tcp_type: value.tcp_type,
        }
    }
}
impl From<CoreCandidate> for TransportCandidate {
    fn from(value: CoreCandidate) -> Self {
        Self {
            candidate_type: value.candidate_type,
            priority: value.priority,
            foundation: value.foundation,
            component: value.component,
            protocol: value.protocol,
            address: value.address,
            related_address: value.related_address,
            related_port: value.related_port,
            tcp_type: value.tcp_type,
        }
    }
}
impl From<CoreLocalDescription> for LocalDescription {
    fn from(value: CoreLocalDescription) -> Self {
        Self {
            credentials: value.credentials.into(),
            candidates: value.candidates.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<RemoteDescription> for CoreRemoteDescription {
    fn from(value: RemoteDescription) -> Self {
        Self {
            credentials: value.credentials.into(),
            candidates: value.candidates.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<TransportRole> for CoreRole {
    fn from(value: TransportRole) -> Self {
        match value {
            TransportRole::Initiator => Self::Initiator,
            TransportRole::Responder => Self::Responder,
        }
    }
}
impl From<CoreSelectedPath> for SelectedPath {
    fn from(value: CoreSelectedPath) -> Self {
        match value {
            CoreSelectedPath::Direct => Self::Direct,
            CoreSelectedPath::Relay => Self::Relay,
        }
    }
}

/// A relay attempt has to carry a relay server; STUN alone cannot allocate one.
fn require_turn(servers: &[IceServer]) -> Result<(), TransportError> {
    servers
        .iter()
        .any(|server| CoreIceServer::from(server.clone()).is_turn())
        .then_some(())
        .ok_or_else(|| TransportError::Failure {
            message: "a relay attempt requires at least one TURN server".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn swift_boundary_gathers_with_turn_from_the_first_attempt() {
        let transport = RemoteTransport::gather(
            IceCredentials {
                ufrag: "preferdirect".into(),
                password: "password-with-more-than-128-bits".into(),
            },
            vec![IceServer {
                // Unresolvable on purpose: the assertion is that a relay URL
                // is accepted and gathered alongside host candidates, not
                // that this fictional server allocates anything.
                url: "turn:relay.invalid:3478?transport=udp".into(),
                username: "user".into(),
                credential: "credential".into(),
            }],
        )
        .await
        .expect("initial gather refused TURN credentials");
        assert!(!transport.connectivity_failed());
    }

    #[test]
    fn a_relay_attempt_without_a_turn_server_is_refused_at_the_boundary() {
        let error = require_turn(&[IceServer {
            url: "stun:stun.example:3478".into(),
            username: String::new(),
            credential: String::new(),
        }])
        .expect_err("a STUN-only list was accepted as a relay attempt");
        assert_eq!(
            error.to_string(),
            "a relay attempt requires at least one TURN server"
        );
        assert!(require_turn(&[IceServer {
            url: "turns:relay.example:5349?transport=tcp".into(),
            username: "user".into(),
            credential: "credential".into(),
        }])
        .is_ok());
    }
}
