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
    direct_failed: AtomicBool,
}

#[uniffi::export(async_runtime = "tokio")]
impl RemoteTransport {
    /// Gathers local candidates. TURN URLs are passed only for a policy-approved retry.
    #[uniffi::constructor]
    pub async fn gather(
        credentials: IceCredentials,
        servers: Vec<IceServer>,
    ) -> Result<Arc<Self>, TransportError> {
        if servers
            .iter()
            .any(|server| CoreIceServer::from(server.clone()).is_turn())
        {
            return Err(TransportError::Failure {
                message: "TURN credentials are withheld until a direct attempt fails".into(),
            });
        }
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
            direct_failed: AtomicBool::new(false),
        }))
    }

    /// Returns the payload to publish in presence/rendezvous.
    pub fn local_description(&self) -> LocalDescription {
        self.local
            .read()
            .expect("local description lock poisoned")
            .clone()
    }

    /// Whether a transport-level direct attempt failed and relay may be issued.
    pub fn relay_retry_allowed(&self) -> bool {
        self.direct_failed.load(Ordering::Acquire)
    }

    /// Re-gathers with TURN after this object's direct connection failed.
    pub async fn retry_with_relay(
        &self,
        servers: Vec<IceServer>,
    ) -> Result<LocalDescription, TransportError> {
        if !self.direct_failed.load(Ordering::Acquire) {
            return Err(TransportError::Failure {
                message: "relay retry requires a recorded direct failure".into(),
            });
        }
        if !servers
            .iter()
            .any(|server| CoreIceServer::from(server.clone()).is_turn())
        {
            return Err(TransportError::Failure {
                message: "relay retry requires at least one TURN server".into(),
            });
        }
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
                    self.direct_failed.store(true, Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn swift_boundary_withholds_turn_on_initial_gather() {
        let result = RemoteTransport::gather(
            IceCredentials {
                ufrag: "direct-first".into(),
                password: "password-with-more-than-128-bits".into(),
            },
            vec![IceServer {
                url: "turn:relay.example:3478".into(),
                username: "user".into(),
                credential: "credential".into(),
            }],
        )
        .await;
        let Err(error) = result else {
            panic!("initial gather accepted TURN credentials");
        };
        assert_eq!(
            error.to_string(),
            "TURN credentials are withheld until a direct attempt fails"
        );
    }
}
