//! Composed webrtc-rs transport implementation.
//!
//! This deliberately stops below `RTCPeerConnection`: signaling remains the
//! structured Latch control-plane contract, while this module composes ICE,
//! DTLS, SCTP, and DCEP directly.

use std::net::{IpAddr, SocketAddr};
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

/// How often the agent runs its checks. The library default, restated so the
/// check budget below is derived from it rather than from a number that
/// happens to match.
const CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// How many checks a candidate pair gets before the agent writes it off.
///
/// The library's default is seven, which at the check interval is under two
/// seconds, and a pair the agent has written off is never checked again: as
/// the controlling side the phone does not revive one on the strength of a
/// later inbound check from the Mac. Two seconds is shorter than the
/// production hand-off. The phone starts checking the moment its offer is
/// accepted, while the Mac still has to collect the offer, authorize it, and
/// hand it to the helper — and until the Mac's agent has sent its first
/// check, the Mac's port-restricted NAT drops every check the phone sends to
/// the reflexive address. A pair is worth checking for as long as either
/// end is still waiting for the connection, so the budget covers the longer
/// of the two connect timeouts.
const MAX_BINDING_REQUESTS: u16 =
    (RESPONDER_CONNECT_TIMEOUT.as_millis() / CHECK_INTERVAL.as_millis()) as u16;

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

/// Whether a peer's candidate is worth pairing with from this endpoint.
///
/// A host candidate on a private, carrier-shared, or link-local address can
/// be reached only from the network it belongs to. Off that network it is
/// dead weight, and expensive dead weight when this end holds a relay: every
/// check the agent sends to it goes through the TURN client, which asks the
/// relay for a permission first, is refused for an unroutable peer, forgets
/// the refusal, and asks again on the next check — five refused requests a
/// second per address, for as long as the checks run. The one place such a
/// host is reachable is the network this endpoint is on itself, which is kept:
/// a phone that missed Bonjour on the Mac's Wi-Fi still gets a direct pair.
///
/// Reflexive and relay candidates are always kept; deciding whether they
/// work is what the checks are for.
fn reachable_from_here(candidate: &TransportCandidate, local_hosts: &[IpAddr]) -> bool {
    if candidate.candidate_type != "host" {
        return true;
    }
    let Ok(address) = candidate.address.parse::<SocketAddr>() else {
        return true;
    };
    let ip = address.ip();
    if globally_routable(ip) {
        return true;
    }
    local_hosts.iter().any(|local| same_network(*local, ip))
}

/// Routable across the internet, as far as the address alone can tell.
fn globally_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // Carrier-grade NAT shared space (100.64/10) and the
                // IETF protocol assignments block (192.0.0/24), which is
                // where a phone's IPv4 side of a 464XLAT setup lives.
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Link-local (fe80::/10) and unique local (fc00::/7).
                || first & 0xffc0 == 0xfe80
                || first & 0xfe00 == 0xfc00)
        }
    }
}

/// Whether two addresses are, by their shape, on one network: the same /24
/// for IPv4, the same /64 for IPv6, or both loopback.
fn same_network(local: IpAddr, remote: IpAddr) -> bool {
    match (local, remote) {
        (IpAddr::V4(local), IpAddr::V4(remote)) => {
            (local.is_loopback() && remote.is_loopback())
                || local.octets()[..3] == remote.octets()[..3]
        }
        (IpAddr::V6(local), IpAddr::V6(remote)) => {
            (local.is_loopback() && remote.is_loopback())
                || local.segments()[..4] == remote.segments()[..4]
        }
        _ => false,
    }
}

/// Content-free description of a candidate list for the diagnostics log:
/// type, transport, and address family only, never an address or a port.
fn summarize(candidates: &[TransportCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            let family = if candidate.address.starts_with('[') {
                "v6"
            } else {
                "v4"
            };
            format!(
                "{}/{}/{}",
                candidate.candidate_type, candidate.protocol, family
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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

impl RtcError {
    /// Which stage of a connect this error belongs to, as a short slug fit
    /// for the audit trail: `candidate`, `server`, `timeout`, `ice`, `dtls`,
    /// `sctp`, `channel`, or `record`. A connect that fails in two seconds
    /// and one that fails in thirty are different failures, and the audit
    /// trail is where that difference has to be legible.
    pub fn stage(&self) -> &'static str {
        match self {
            Self::InvalidCandidate(_) => "candidate",
            Self::InvalidIceServer(_) => "server",
            Self::RecordTooLarge => "record",
            Self::Stack(message) => {
                if message.contains("timed out") {
                    "timeout"
                } else if let Some(stage) = message.strip_prefix('[') {
                    match stage.split(']').next().unwrap_or_default() {
                        "dtls" => "dtls",
                        "sctp" => "sctp",
                        "channel" => "channel",
                        _ => "ice",
                    }
                } else {
                    "ice"
                }
            }
        }
    }
}

/// Labels a stack error with the stage that produced it.
fn staged<E: std::fmt::Display>(stage: &'static str) -> impl Fn(E) -> RtcError {
    move |error| RtcError::Stack(format!("[{stage}] {error}"))
}

/// The ICE library keeps background tasks alive until explicitly closed.
/// Keep that responsibility through gathering, connection errors, cancellation,
/// and finally the connected channel. Dropping a Rust future must release its
/// sockets and TURN allocations, not leave checks running behind a retry.
struct OwnedAgent(Arc<Agent>, tokio::runtime::Handle);

impl std::ops::Deref for OwnedAgent {
    type Target = Agent;
    fn deref(&self) -> &Agent {
        &self.0
    }
}

impl Drop for OwnedAgent {
    fn drop(&mut self) {
        let agent = Arc::clone(&self.0);
        self.1.spawn(async move {
            let _ = agent.close().await;
        });
    }
}

struct AttemptTrace {
    id: String,
    finished: bool,
}
impl Drop for AttemptTrace {
    fn drop(&mut self) {
        if !self.finished {
            log::info!(target: "latch_transport", "attempt={} transport cancelled", self.id);
        }
    }
}

/// An ICE agent after candidate gathering and before peer connection.
pub struct RtcEndpoint {
    agent: OwnedAgent,
    selected_path: Arc<AtomicU8>,
    local_ufrag: String,
}

impl RtcEndpoint {
    /// Gathers candidates. A TURN server in `servers` adds a relay candidate;
    /// whether one may be passed is decided where the credential is issued.
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
            check_interval: CHECK_INTERVAL,
            max_binding_requests: Some(MAX_BINDING_REQUESTS),
            ..Default::default()
        })
        .await
        .map_err(stack)?;

        let agent = OwnedAgent(Arc::new(agent), tokio::runtime::Handle::current());
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
        log::info!(
            target: "latch_transport",
            "gathered {} against {} server(s): {}",
            candidates.len(),
            servers.len(),
            summarize(&candidates)
        );
        Ok((
            Self {
                agent,
                selected_path,
                local_ufrag: local_ufrag.clone(),
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
        let mut trace = AttemptTrace {
            id: crate::diagnostics::fingerprint(match role {
                Role::Initiator => &self.local_ufrag,
                Role::Responder => &remote.credentials.ufrag,
            }),
            finished: false,
        };
        let result = self.connect_inner(remote, role).await;
        trace.finished = true;
        match &result {
            Ok(_) => {
                log::info!(target: "latch_transport", "attempt={} transport connected", trace.id)
            }
            Err(error) => {
                log::warn!(target: "latch_transport", "attempt={} transport failed stage={}", trace.id, error.stage())
            }
        }
        result
    }

    async fn connect_inner(
        self,
        remote: RemoteDescription,
        role: Role,
    ) -> Result<RtcConnection, RtcError> {
        let attempt = crate::diagnostics::fingerprint(match role {
            Role::Initiator => &self.local_ufrag,
            Role::Responder => &remote.credentials.ufrag,
        });
        log::info!(target: "latch_transport", "attempt={attempt} connect role={role:?} local_credential={} remote_credential={}",
            crate::diagnostics::fingerprint(&self.local_ufrag),
            crate::diagnostics::fingerprint(&remote.credentials.ufrag));
        log::info!(
            target: "latch_transport",
            "connecting as {role:?} to {} remote candidate(s): {}",
            remote.candidates.len(),
            summarize(&remote.candidates)
        );
        let local_hosts: Vec<IpAddr> = self
            .agent
            .get_local_candidates()
            .await
            .map_err(stack)?
            .iter()
            .filter(|candidate| candidate.candidate_type() == CandidateType::Host)
            .filter_map(|candidate| candidate.address().parse().ok())
            .collect();
        let usable: Vec<&TransportCandidate> = remote
            .candidates
            .iter()
            .filter(|candidate| reachable_from_here(candidate, &local_hosts))
            .collect();
        // A peer that published nothing reachable is still worth the checks
        // it implies rather than an immediate refusal: the filter exists to
        // spare the relay, not to second-guess the other end.
        let usable = if usable.is_empty() {
            remote.candidates.iter().collect()
        } else {
            usable
        };
        for candidate in usable {
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

        log::info!(
            target: "latch_transport",
            "attempt={attempt} ICE nominated a pair as {role:?}: {}",
            match SelectedRoute::from_code(selected_path.load(Ordering::Acquire)) {
                Some(route) => format!("{route:?}"),
                None => "route not yet observed".to_owned(),
            }
        );
        let legacy_ice: Arc<dyn LegacyConn + Send + Sync> = Arc::new(ModernToLegacy(ice));
        let legacy_dtls: Arc<dyn LegacyConn + Send + Sync> = Arc::new(
            DTLSConn::new(
                legacy_ice,
                DtlsConfig {
                    certificates: vec![Certificate::generate_self_signed(vec![
                        "latch-transport".to_owned()
                    ])
                    .map_err(staged("dtls"))?],
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
            .map_err(staged("dtls"))?,
        );
        let dtls: Arc<dyn ModernConn + Send + Sync> = Arc::new(LegacyToModern(legacy_dtls));
        let association = Arc::new(match role {
            Role::Initiator => Association::client(sctp_config(dtls, "latch-initiator"))
                .await
                .map_err(staged("sctp"))?,
            Role::Responder => Association::server(sctp_config(dtls, "latch-responder"))
                .await
                .map_err(staged("sctp"))?,
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
                .map_err(staged("channel"))?,
            Role::Responder => DataChannel::accept::<DataChannel>(&association, config, &[])
                .await
                .map_err(staged("channel"))?,
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
    agent: OwnedAgent,
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
    /// This is immediate cleanup; successful response writers must drain first.
    pub async fn close(&self) -> Result<(), RtcError> {
        // An upper-layer close failure must not skip socket/allocation cleanup.
        let channel = self.channel.close().await.map_err(stack);
        let association = self.association.close().await.map_err(stack);
        let agent = self.agent.close().await.map_err(stack);
        channel.and(association).and(agent)
    }

    /// Waits for the peer to acknowledge queued records before normal teardown.
    /// SCTP write completion only enqueues bytes. Closing the association at
    /// that point can discard the final HTTP response. The bound also ensures
    /// a vanished peer cannot hold cleanup indefinitely. Cancellation remains
    /// safe: callers can drop this future and use immediate `close`.
    pub async fn drain(&self) -> Result<(), RtcError> {
        log::info!(target: "latch_transport", "response drain started queued_bytes={}", self.channel.buffered_amount());
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.channel.buffered_amount() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| RtcError::Stack("[channel] response delivery timed out".into()))?;
        log::info!(target: "latch_transport", "response drain completed");
        Ok(())
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

    #[test]
    fn a_peers_private_host_is_paired_only_from_its_own_network() {
        fn host(address: &str) -> TransportCandidate {
            TransportCandidate {
                candidate_type: "host".into(),
                priority: 1,
                foundation: "1".into(),
                component: 1,
                protocol: "udp".into(),
                address: address.into(),
                related_address: None,
                related_port: None,
                tcp_type: None,
            }
        }
        let on_lan: Vec<IpAddr> = vec!["192.168.1.20".parse().unwrap()];
        let elsewhere: Vec<IpAddr> = vec!["10.9.8.7".parse().unwrap()];

        // A phone on this Mac's Wi-Fi is a direct pair; the same phone seen
        // from anywhere else is not, and a carrier's shared or 464XLAT
        // address never is.
        assert!(reachable_from_here(&host("192.168.1.30:5000"), &on_lan));
        assert!(!reachable_from_here(&host("192.168.1.30:5000"), &elsewhere));
        assert!(!reachable_from_here(&host("100.101.46.8:5000"), &on_lan));
        assert!(!reachable_from_here(&host("192.0.0.4:5000"), &on_lan));
        assert!(!reachable_from_here(
            &host("[fd7a:115c:a1e0::1]:5000"),
            &on_lan
        ));
        assert!(!reachable_from_here(&host("[fe80::1]:5000"), &on_lan));
        // A public address of either family is worth a check.
        assert!(reachable_from_here(&host("1.1.1.1:5000"), &elsewhere));
        assert!(reachable_from_here(&host("[2001:db8::9]:5000"), &elsewhere));
        // Reflexive and relay candidates are never filtered.
        let mut relay = host("10.0.0.1:1");
        relay.candidate_type = "relay".into();
        assert!(reachable_from_here(&relay, &elsewhere));
        // Loopback pairs with loopback, which is what the in-memory tests do.
        let loopback: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap()];
        assert!(reachable_from_here(&host("127.0.0.1:5000"), &loopback));
        assert!(!reachable_from_here(&host("127.0.0.1:5000"), &on_lan));
    }

    #[test]
    fn a_connect_error_names_the_stage_it_failed_in() {
        assert_eq!(RtcError::InvalidCandidate("x".into()).stage(), "candidate");
        assert_eq!(RtcError::InvalidIceServer("x".into()).stage(), "server");
        assert_eq!(RtcError::RecordTooLarge.stage(), "record");
        assert_eq!(
            RtcError::Stack("ICE connectivity checks timed out".into()).stage(),
            "timeout"
        );
        assert_eq!(staged("dtls")("handshake failed").stage(), "dtls");
        assert_eq!(staged("sctp")("abort").stage(), "sctp");
        assert_eq!(staged("channel")("closed").stage(), "channel");
        assert_eq!(stack("no candidate pairs").stage(), "ice");
    }

    async fn cleanup_endpoint() -> (RtcEndpoint, Arc<Agent>) {
        let (endpoint, _) = RtcEndpoint::gather_on_test_network(
            IceCredentials {
                ufrag: "cleanup-ufrag".into(),
                password: "cleanup-password-with-more-than-128-bits".into(),
            },
            &[],
            Arc::new(TestNetwork::new(Some(Default::default()))),
        )
        .await
        .unwrap();
        let observer = Arc::clone(&endpoint.agent.0);
        assert!(!observer.get_local_candidates().await.unwrap().is_empty());
        (endpoint, observer)
    }

    async fn assert_agent_closed(agent: &Agent) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !agent.get_local_candidates().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("abandoned agent retained its candidates and sockets");
    }

    fn unreachable_peer() -> RemoteDescription {
        RemoteDescription {
            credentials: IceCredentials {
                ufrag: "missing-peer".into(),
                password: "missing-password-with-more-than-128-bits".into(),
            },
            candidates: vec![],
        }
    }

    #[tokio::test]
    async fn dropping_a_gathered_endpoint_closes_its_sockets() {
        let (endpoint, observer) = cleanup_endpoint().await;
        // UniFFI may destroy its last handle on a Swift thread outside Tokio.
        std::thread::spawn(move || drop(endpoint)).join().unwrap();
        assert_agent_closed(&observer).await;
    }

    #[tokio::test]
    async fn cancelling_connect_closes_the_checking_agent() {
        let (endpoint, observer) = cleanup_endpoint().await;
        let (tx, mut rx) = mpsc::channel(4);
        observer.on_connection_state_change(Box::new(move |state| {
            let tx = tx.clone();
            Box::pin(async move {
                let _ = tx.send(state).await;
            })
        }));
        let pending = tokio::spawn(endpoint.connect(unreachable_peer(), Role::Initiator));
        tokio::time::timeout(Duration::from_secs(2), async {
            while rx.recv().await != Some(webrtc_ice::state::ConnectionState::Checking) {}
        })
        .await
        .unwrap();
        pending.abort();
        assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
        assert_agent_closed(&observer).await;
    }

    #[tokio::test]
    async fn an_ice_timeout_closes_the_agent_before_a_retry() {
        let (endpoint, observer) = cleanup_endpoint().await;
        let result = endpoint.connect(unreachable_peer(), Role::Initiator).await;
        assert!(
            matches!(result, Err(RtcError::Stack(message)) if message == "ICE connectivity checks timed out")
        );
        assert_agent_closed(&observer).await;
    }

    #[tokio::test]
    async fn an_invalid_candidate_closes_the_agent() {
        let (endpoint, observer) = cleanup_endpoint().await;
        let mut remote = unreachable_peer();
        remote.candidates.push(TransportCandidate {
            candidate_type: "host".into(),
            priority: 1,
            foundation: "1".into(),
            component: 1,
            protocol: "udp".into(),
            address: "invalid".into(),
            related_address: None,
            related_port: None,
            tcp_type: None,
        });
        assert!(matches!(
            endpoint.connect(remote, Role::Initiator).await,
            Err(RtcError::InvalidCandidate(_))
        ));
        assert_agent_closed(&observer).await;
    }

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
