//! Composed webrtc-rs transport implementation.
//!
//! This deliberately stops below `RTCPeerConnection`: signaling remains the
//! structured Latch control-plane contract, while this module composes ICE,
//! DTLS, SCTP, and DCEP directly.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::sync::mpsc;
use webrtc_data::data_channel::{Config as DataChannelConfig, DataChannel};
use webrtc_dtls::config::{Config as DtlsConfig, ExtendedMasterSecretType};
use webrtc_dtls::conn::DTLSConn;
use webrtc_dtls::crypto::Certificate;
use webrtc_ice::agent::agent_config::AgentConfig;
use webrtc_ice::agent::Agent;
use webrtc_ice::candidate::candidate_base::unmarshal_candidate;
use webrtc_ice::candidate::{Candidate, CandidateType};
use webrtc_ice::mdns::MulticastDnsMode;
use webrtc_ice::network_type::supported_network_types;
use webrtc_ice::url::Url;
use webrtc_sctp::association::{Association, Config as SctpConfig};
use webrtc_util::Conn as ModernConn;
use webrtc_util_legacy::Conn as LegacyConn;

use crate::policy::{IceServer, SelectedPath};

const MAX_NOISE_RECORD: usize = u16::MAX as usize;
const DATA_CHANNEL_LABEL: &str = "latch-noise-v1";
/// How long the phone waits for a nominated pair.
///
/// This is the interactive end: a person is holding the phone watching a
/// spinner. Relay candidates are in the first attempt, so the wait does not
/// have to cover a direct-only attempt plus a relayed retry. What it does have
/// to cover is the other end learning that the attempt exists at all: the
/// phone's checks go nowhere until the Mac has collected the offer from the
/// control plane, handed it to its agent, and started answering. That is a
/// long-polled collection plus a helper drain, normally a second or two, but
/// on a slow cellular link with a TURN allocation in front of it the old
/// six-second budget was the difference between connected and failed.
const INITIATOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the Mac helper waits for the same pair.
///
/// The helper is answering in the background with nobody watching it, and
/// giving up before the phone does would turn a slow network into a failure
/// the phone reports as the Mac's.
const RESPONDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The in-memory network used by round-trip tests. See
/// [`RtcEndpoint::gather_on_test_network`].
#[doc(hidden)]
pub type TestNetwork = webrtc_util::vnet::net::Net;

/// How the nominated candidate pair actually reaches the peer.
///
/// [`SelectedPath`] answers the policy question — is this relayed or not —
/// and that is all the policy needs. Field verification needs one step more
/// resolution: a pair of host candidates means the two devices were on the
/// same network, while a server-reflexive pair means a hole was punched
/// through at least one NAT. Collapsing those two into "direct" makes a
/// cellular-to-home-NAT success indistinguishable from a phone sitting on the
/// same Wi-Fi, which is exactly the distinction the direct-versus-relay rate
/// is supposed to measure.
///
/// It is derived from candidate types only. No address, port, or interface
/// name is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedRoute {
    /// Both ends nominated a host candidate: same LAN, or a tunnel interface
    /// such as a tailnet that presents as one.
    Host,
    /// At least one end is server- or peer-reflexive: NAT traversal worked.
    Reflexive,
    /// At least one end is relayed: the bytes take the TURN detour.
    Relay,
}

impl SelectedRoute {
    /// Sentinel stored before any pair is nominated.
    const UNOBSERVED: u8 = 0;

    /// Classifies a nominated pair by its worse half.
    ///
    /// A pair is only as direct as its least direct end: one relayed
    /// candidate means every byte is relayed regardless of what the other end
    /// contributed.
    fn of(local: CandidateType, remote: CandidateType) -> Self {
        if local == CandidateType::Relay || remote == CandidateType::Relay {
            Self::Relay
        } else if local == CandidateType::Host && remote == CandidateType::Host {
            Self::Host
        } else {
            Self::Reflexive
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Host => 1,
            Self::Reflexive => 2,
            Self::Relay => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Host),
            2 => Some(Self::Reflexive),
            3 => Some(Self::Relay),
            _ => None,
        }
    }

    /// Stable slug for counters and the audit trail.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Reflexive => "reflexive",
            Self::Relay => "relay",
        }
    }
}

/// ICE role and corresponding DTLS/SCTP role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Phone-initiated controlling endpoint.
    Initiator,
    /// Mac helper controlled endpoint.
    Responder,
}

/// ICE credentials exchanged through the structured control-plane contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IceCredentials {
    /// Username fragment.
    pub ufrag: String,
    /// Password.
    pub password: String,
}

/// Structured ICE candidate matching the control-plane representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportCandidate {
    /// `host`, `srflx`, `prflx`, or `relay`.
    pub candidate_type: String,
    /// ICE pair-ordering priority.
    pub priority: u32,
    /// Candidate foundation.
    pub foundation: String,
    /// RTP component (one for the data transport).
    pub component: u16,
    /// `udp` or `tcp`.
    pub protocol: String,
    /// IP literal and port.
    pub address: String,
    /// Related IP literal for reflexive/relay candidates.
    pub related_address: Option<String>,
    /// Related port.
    pub related_port: Option<u16>,
    /// ICE TCP type when protocol is TCP.
    pub tcp_type: Option<String>,
}

impl TransportCandidate {
    fn to_sdp(&self) -> Result<String, RtcError> {
        let address: SocketAddr = self
            .address
            .parse()
            .map_err(|_| RtcError::InvalidCandidate(self.address.clone()))?;
        let mut line = format!(
            "{} {} {} {} {} {} typ {}",
            self.foundation,
            self.component,
            self.protocol,
            self.priority,
            address.ip(),
            address.port(),
            self.candidate_type
        );
        if let (Some(related_address), Some(related_port)) =
            (&self.related_address, self.related_port)
        {
            if related_address.parse::<std::net::IpAddr>().is_err() {
                return Err(RtcError::InvalidCandidate(related_address.clone()));
            }
            line.push_str(&format!(" raddr {related_address} rport {related_port}"));
        }
        if let Some(tcp_type) = &self.tcp_type {
            line.push_str(&format!(" tcptype {tcp_type}"));
        }
        Ok(line)
    }

    fn from_webrtc(candidate: &dyn Candidate) -> Self {
        let candidate_type = candidate.candidate_type().to_string();
        let related = candidate.related_address();
        Self {
            candidate_type,
            priority: candidate.priority(),
            foundation: candidate.foundation(),
            component: candidate.component(),
            protocol: candidate.network_type().network_short().to_owned(),
            address: SocketAddr::new(
                candidate.address().parse().expect("webrtc candidate IP"),
                candidate.port(),
            )
            .to_string(),
            related_address: related
                .as_ref()
                .map(|value| value.address.clone())
                .filter(|value| !value.is_empty()),
            related_port: related.map(|value| value.port).filter(|port| *port != 0),
            tcp_type: {
                let value = candidate.tcp_type().to_string();
                (value != "unspecified").then_some(value)
            },
        }
    }
}

/// Local description published before connectivity checks begin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalDescription {
    /// Agent-lifetime credentials.
    pub credentials: IceCredentials,
    /// Gathered host/server-reflexive (and, only on retry, relay) candidates.
    pub candidates: Vec<TransportCandidate>,
}

/// Peer description received from presence/rendezvous.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDescription {
    /// Peer ICE credentials.
    pub credentials: IceCredentials,
    /// Peer candidates.
    pub candidates: Vec<TransportCandidate>,
}

/// Errors from the RTC stack.
#[derive(Debug, Error)]
pub enum RtcError {
    /// A signaling candidate could not be reconstructed safely.
    #[error("invalid ICE candidate: {0}")]
    InvalidCandidate(String),
    /// An ICE server URL was not usable.
    #[error("invalid ICE server: {0}")]
    InvalidIceServer(String),
    /// The webrtc-rs stack rejected an operation.
    #[error("WebRTC transport failed: {0}")]
    Stack(String),
    /// A received data-channel message exceeded the Noise record limit.
    #[error("received data-channel message exceeds the Noise record limit")]
    RecordTooLarge,
}

/// An ICE agent after candidate gathering and before peer connection.
pub struct RtcEndpoint {
    agent: Agent,
    selected_path: Arc<AtomicU8>,
}

impl RtcEndpoint {
    /// Gathers candidates. Passing TURN servers here is valid only for the
    /// policy-controlled post-direct-failure retry.
    pub async fn gather(
        credentials: IceCredentials,
        servers: &[IceServer],
    ) -> Result<(Self, LocalDescription), RtcError> {
        Self::gather_with_network(credentials, servers, false, None).await
    }

    /// Test-support gathering on an in-memory network.
    ///
    /// A round-trip test must not depend on the machine running it having a
    /// routable interface, and loopback candidates are excluded from real
    /// gathering on purpose. This is the only way to exercise the full
    /// ICE/DTLS/SCTP path deterministically; nothing in production calls it.
    #[doc(hidden)]
    pub async fn gather_on_test_network(
        credentials: IceCredentials,
        servers: &[IceServer],
        net: Arc<TestNetwork>,
    ) -> Result<(Self, LocalDescription), RtcError> {
        Self::gather_with_network(credentials, servers, true, Some(net)).await
    }

    async fn gather_with_network(
        credentials: IceCredentials,
        servers: &[IceServer],
        include_loopback: bool,
        net: Option<Arc<webrtc_util::vnet::net::Net>>,
    ) -> Result<(Self, LocalDescription), RtcError> {
        let urls = servers
            .iter()
            .map(|server| {
                let mut url = Url::parse_url(&server.url)
                    .map_err(|error| RtcError::InvalidIceServer(error.to_string()))?;
                url.username.clone_from(&server.username);
                url.password.clone_from(&server.credential);
                Ok(url)
            })
            .collect::<Result<Vec<_>, RtcError>>()?;
        let include_relay = servers.iter().any(IceServer::is_turn);
        let agent = Agent::new(AgentConfig {
            urls,
            local_ufrag: credentials.ufrag.clone(),
            local_pwd: credentials.password.clone(),
            multicast_dns_mode: MulticastDnsMode::Disabled,
            // AgentConfig's derived Default leaves this empty. An empty list
            // completes gathering successfully with zero candidates, which
            // looks healthy to signaling but can never establish a path.
            network_types: supported_network_types(),
            candidate_types: if include_relay {
                vec![
                    CandidateType::Host,
                    CandidateType::ServerReflexive,
                    CandidateType::Relay,
                ]
            } else {
                vec![CandidateType::Host, CandidateType::ServerReflexive]
            },
            include_loopback,
            net,
            ..Default::default()
        })
        .await
        .map_err(stack)?;

        let (candidate_tx, mut candidate_rx) = mpsc::channel(16);
        agent.on_candidate(Box::new(move |candidate| {
            let candidate_tx = candidate_tx.clone();
            Box::pin(async move {
                let _ = candidate_tx.send(candidate).await;
            })
        }));
        let selected_path = Arc::new(AtomicU8::new(SelectedRoute::UNOBSERVED));
        agent.on_selected_candidate_pair_change(Box::new({
            let selected_path = Arc::clone(&selected_path);
            move |local, remote| {
                let route = SelectedRoute::of(local.candidate_type(), remote.candidate_type());
                selected_path.store(route.code(), Ordering::Release);
                Box::pin(async {})
            }
        }));
        agent.gather_candidates().map_err(stack)?;

        let mut candidates = Vec::new();
        while let Some(candidate) = candidate_rx.recv().await {
            let Some(candidate) = candidate else { break };
            candidates.push(TransportCandidate::from_webrtc(candidate.as_ref()));
        }
        let (local_ufrag, local_password) = agent.get_local_user_credentials().await;
        Ok((
            Self {
                agent,
                selected_path,
            },
            LocalDescription {
                credentials: IceCredentials {
                    ufrag: local_ufrag,
                    password: local_password,
                },
                candidates,
            },
        ))
    }

    /// Releases the agent and its sockets without ever having connected.
    ///
    /// An endpoint that is replaced before an offer reaches it — the helper
    /// re-gathers so presence stays current — must close rather than drop:
    /// the agent owns sockets and a gathering task that only `close` ends.
    pub async fn close(self) -> Result<(), RtcError> {
        self.agent.close().await.map_err(stack)
    }

    /// Establishes ICE, DTLS, SCTP, and one reliable ordered data channel.
    pub async fn connect(
        self,
        remote: RemoteDescription,
        role: Role,
    ) -> Result<RtcConnection, RtcError> {
        for candidate in &remote.candidates {
            let parsed: Arc<dyn Candidate + Send + Sync> =
                Arc::new(unmarshal_candidate(&candidate.to_sdp()?).map_err(stack)?);
            self.agent.add_remote_candidate(&parsed).map_err(stack)?;
        }

        let (_cancel_tx, cancel_rx) = mpsc::channel(1);
        let ice: Arc<dyn ModernConn + Send + Sync> = match role {
            Role::Initiator => tokio::time::timeout(
                INITIATOR_CONNECT_TIMEOUT,
                self.agent.dial(
                    cancel_rx,
                    remote.credentials.ufrag,
                    remote.credentials.password,
                ),
            )
            .await
            .map_err(|_| RtcError::Stack("ICE connectivity checks timed out".into()))?
            .map_err(stack)?,
            Role::Responder => tokio::time::timeout(
                RESPONDER_CONNECT_TIMEOUT,
                self.agent.accept(
                    cancel_rx,
                    remote.credentials.ufrag,
                    remote.credentials.password,
                ),
            )
            .await
            .map_err(|_| RtcError::Stack("ICE connectivity checks timed out".into()))?
            .map_err(stack)?,
        };
        let selected_path = self.selected_path;

        let legacy_ice: Arc<dyn LegacyConn + Send + Sync> = Arc::new(ModernToLegacy(ice));
        let legacy_dtls: Arc<dyn LegacyConn + Send + Sync> = Arc::new(
            DTLSConn::new(
                legacy_ice,
                DtlsConfig {
                    certificates: vec![Certificate::generate_self_signed(vec![
                        "latch-transport".to_owned()
                    ])
                    .map_err(stack)?],
                    // DTLS is transport encryption only. Noise authenticates the
                    // pairing-record pin immediately above this channel.
                    insecure_skip_verify: true,
                    extended_master_secret: ExtendedMasterSecretType::Require,
                    ..Default::default()
                },
                role == Role::Initiator,
                None,
            )
            .await
            .map_err(stack)?,
        );
        let dtls: Arc<dyn ModernConn + Send + Sync> = Arc::new(LegacyToModern(legacy_dtls));
        let association = Arc::new(match role {
            Role::Initiator => Association::client(sctp_config(dtls, "latch-initiator"))
                .await
                .map_err(stack)?,
            Role::Responder => Association::server(sctp_config(dtls, "latch-responder"))
                .await
                .map_err(stack)?,
        });
        let config = DataChannelConfig {
            negotiated: false,
            label: DATA_CHANNEL_LABEL.to_owned(),
            max_message_size: MAX_NOISE_RECORD as u32,
            ..Default::default()
        };
        let channel = match role {
            Role::Initiator => DataChannel::dial(&association, 0, config)
                .await
                .map_err(stack)?,
            Role::Responder => DataChannel::accept::<DataChannel>(&association, config, &[])
                .await
                .map_err(stack)?,
        };
        Ok(RtcConnection {
            agent: self.agent,
            association,
            channel: Arc::new(channel),
            selected_path,
        })
    }
}

/// Connected reliable ordered byte-record surface consumed by Noise.
pub struct RtcConnection {
    agent: Agent,
    association: Arc<Association>,
    channel: Arc<DataChannel>,
    selected_path: Arc<AtomicU8>,
}

impl RtcConnection {
    /// Selected ICE path.
    pub fn selected_path(&self) -> SelectedPath {
        match self.selected_route() {
            Some(SelectedRoute::Relay) => SelectedPath::Relay,
            _ => SelectedPath::Direct,
        }
    }

    /// The nominated pair's route, at the granularity metrics need.
    ///
    /// `None` means no pair was ever nominated on this agent, which a
    /// connected channel should not produce but a torn-down one can.
    pub fn selected_route(&self) -> Option<SelectedRoute> {
        SelectedRoute::from_code(self.selected_path.load(Ordering::Acquire))
    }

    /// Writes one Noise ciphertext record.
    pub async fn write(&self, record: &[u8]) -> Result<(), RtcError> {
        if record.len() > MAX_NOISE_RECORD {
            return Err(RtcError::RecordTooLarge);
        }
        self.channel
            .write(&Bytes::copy_from_slice(record))
            .await
            .map_err(stack)?;
        Ok(())
    }

    /// Reads one Noise ciphertext record.
    pub async fn read(&self) -> Result<Vec<u8>, RtcError> {
        let mut buffer = vec![0; MAX_NOISE_RECORD];
        let length = self.channel.read(&mut buffer).await.map_err(stack)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// Closes the data channel, SCTP association, and ICE agent.
    pub async fn close(&self) -> Result<(), RtcError> {
        self.channel.close().await.map_err(stack)?;
        self.association.close().await.map_err(stack)?;
        self.agent.close().await.map_err(stack)
    }
}

fn sctp_config(connection: Arc<dyn ModernConn + Send + Sync>, name: &str) -> SctpConfig {
    SctpConfig {
        net_conn: connection,
        max_receive_buffer_size: 0,
        max_message_size: MAX_NOISE_RECORD as u32,
        mtu: 0,
        name: name.to_owned(),
        local_port: 5000,
        remote_port: 5000,
    }
}

fn stack(error: impl std::fmt::Display) -> RtcError {
    RtcError::Stack(error.to_string())
}

/// The legacy async DTLS crate and the maintained ICE/SCTP crates currently
/// publish identical `Conn` traits through different `webrtc-util` versions.
/// These adapters are deliberately byte-for-byte pass-through; they add no
/// buffering, addressing, or trust decision.
struct ModernToLegacy(Arc<dyn ModernConn + Send + Sync>);

#[async_trait]
impl LegacyConn for ModernToLegacy {
    async fn connect(&self, address: SocketAddr) -> Result<(), webrtc_util_legacy::Error> {
        self.0.connect(address).await.map_err(legacy_error)
    }
    async fn recv(&self, buffer: &mut [u8]) -> Result<usize, webrtc_util_legacy::Error> {
        self.0.recv(buffer).await.map_err(legacy_error)
    }
    async fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> Result<(usize, SocketAddr), webrtc_util_legacy::Error> {
        self.0.recv_from(buffer).await.map_err(legacy_error)
    }
    async fn send(&self, buffer: &[u8]) -> Result<usize, webrtc_util_legacy::Error> {
        self.0.send(buffer).await.map_err(legacy_error)
    }
    async fn send_to(
        &self,
        buffer: &[u8],
        target: SocketAddr,
    ) -> Result<usize, webrtc_util_legacy::Error> {
        self.0.send_to(buffer, target).await.map_err(legacy_error)
    }
    fn local_addr(&self) -> Result<SocketAddr, webrtc_util_legacy::Error> {
        self.0.local_addr().map_err(legacy_error)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        self.0.remote_addr()
    }
    async fn close(&self) -> Result<(), webrtc_util_legacy::Error> {
        self.0.close().await.map_err(legacy_error)
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

struct LegacyToModern(Arc<dyn LegacyConn + Send + Sync>);

#[async_trait]
impl ModernConn for LegacyToModern {
    async fn connect(&self, address: SocketAddr) -> Result<(), webrtc_util::Error> {
        self.0.connect(address).await.map_err(modern_error)
    }
    async fn recv(&self, buffer: &mut [u8]) -> Result<usize, webrtc_util::Error> {
        self.0.recv(buffer).await.map_err(modern_error)
    }
    async fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> Result<(usize, SocketAddr), webrtc_util::Error> {
        self.0.recv_from(buffer).await.map_err(modern_error)
    }
    async fn send(&self, buffer: &[u8]) -> Result<usize, webrtc_util::Error> {
        self.0.send(buffer).await.map_err(modern_error)
    }
    async fn send_to(
        &self,
        buffer: &[u8],
        target: SocketAddr,
    ) -> Result<usize, webrtc_util::Error> {
        self.0.send_to(buffer, target).await.map_err(modern_error)
    }
    fn local_addr(&self) -> Result<SocketAddr, webrtc_util::Error> {
        self.0.local_addr().map_err(modern_error)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        self.0.remote_addr()
    }
    async fn close(&self) -> Result<(), webrtc_util::Error> {
        self.0.close().await.map_err(modern_error)
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

fn legacy_error(error: impl std::fmt::Display) -> webrtc_util_legacy::Error {
    webrtc_util_legacy::Error::Other(error.to_string())
}

fn modern_error(error: impl std::fmt::Display) -> webrtc_util::Error {
    webrtc_util::Error::Other(error.to_string())
}

#[cfg(test)]
mod nat_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_candidates_carry_a_reliable_ordered_record() {
        let left_credentials = IceCredentials {
            ufrag: "left-ufrag".into(),
            password: "left-password-with-more-than-128-bits".into(),
        };
        let right_credentials = IceCredentials {
            ufrag: "right-ufrag".into(),
            password: "right-password-with-more-than-128-bits".into(),
        };
        // Do not depend on a CI machine having a routable interface. The
        // in-memory network supplies two loopback-capable endpoints while
        // exercising the same candidate gathering and full ICE/DTLS/SCTP
        // stack production uses.
        let network = Arc::new(webrtc_util::vnet::net::Net::new(Some(Default::default())));
        let (left, right) = tokio::join!(
            RtcEndpoint::gather_with_network(
                left_credentials,
                &[],
                true,
                Some(Arc::clone(&network))
            ),
            RtcEndpoint::gather_with_network(right_credentials, &[], true, Some(network))
        );
        let (left_endpoint, left_description) = left.expect("left gathers host candidates");
        let (right_endpoint, right_description) = right.expect("right gathers host candidates");
        assert!(!left_description.candidates.is_empty());
        assert!(!right_description.candidates.is_empty());

        let (left, right) = tokio::join!(
            left_endpoint.connect(
                RemoteDescription {
                    credentials: right_description.credentials,
                    candidates: right_description.candidates,
                },
                Role::Initiator,
            ),
            right_endpoint.connect(
                RemoteDescription {
                    credentials: left_description.credentials,
                    candidates: left_description.candidates,
                },
                Role::Responder,
            )
        );
        let left = left.expect("initiator connects");
        let right = right.expect("responder connects");
        left.write(b"noise ciphertext")
            .await
            .expect("record writes");
        assert_eq!(
            right.read().await.expect("record reads"),
            b"noise ciphertext"
        );
        let _ = tokio::join!(left.close(), right.close());
    }
}
