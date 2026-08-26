//! The helper's ICE responder.
//!
//! The desktop app collects rendezvous offers from the control plane, checks
//! each peer against the local device store, and hands the survivors to this
//! agent. Reaching the agent authorizes nothing: an offer carries transport
//! parameters only, and the Noise handshake in the `latch` crate is still the
//! only thing that decides who the peer is and what it may do.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use latch::cli::remote_access::{
    candidate_lifetime_from_now, record_ice_answer, IceCandidateRecord, IceReadiness, PeerReader,
    PeerRoute, PeerStream, PeerTransport, PeerWriter, RemoteOffer, PROXY_IDLE_TIMEOUT,
};
use latch::session::paths::LatchHome;
use latch_transport::policy::IceServer;
use latch_transport::rtc::{
    IceCredentials, LocalDescription, RemoteDescription, Role, RtcConnection, RtcEndpoint,
    SelectedRoute, TestNetwork, TransportCandidate,
};
use tokio::sync::{mpsc, Mutex};

/// How many connected-but-unclaimed streams may queue before the helper drains
/// them. `serve_lan` accepts continuously, so this only absorbs the gap between
/// a data channel opening and the accept loop's next turn.
const ACCEPT_BACKLOG: usize = 4;

/// One ICE agent answering offers on behalf of this Mac.
///
/// An agent is gathered ahead of time and handed to the first offer that
/// arrives; a replacement is gathered in the background straight afterwards, so
/// answering one phone does not leave the next one with nothing to reach. The
/// credentials are fixed for the life of the helper because presence advertises
/// them, and a phone that read them at the start of a presence window must
/// still be able to authenticate its checks at the end of it.
pub struct IceResponder {
    inner: Arc<Responder>,
}

struct Responder {
    /// Only so an answer's outcome reaches the one audit trail. The helper
    /// reads no state and holds no identity through it.
    home: Option<LatchHome>,
    credentials: IceCredentials,
    servers: Vec<IceServer>,
    /// The gathered, unused agent. `None` while one is being answered or
    /// re-gathered, which is what makes a second concurrent offer a clean
    /// refusal rather than a silent drop.
    idle: Mutex<Option<RtcEndpoint>>,
    description: Mutex<Option<IceReadiness>>,
    accepted: mpsc::Sender<PeerStream>,
    incoming: Mutex<mpsc::Receiver<PeerStream>>,
    /// Set only by [`IceResponder::for_test`]. Real gathering excludes loopback
    /// on purpose, so a round-trip test needs an in-memory network instead.
    test_network: Option<Arc<TestNetwork>>,
}

impl IceResponder {
    /// Builds a responder with freshly minted short-term ICE credentials.
    ///
    /// `servers` are STUN URLs used for server-reflexive gathering. Passing
    /// none is valid and yields host candidates only — the LAN and tailnet
    /// case, where every usable address is already on an interface.
    pub fn new(home: LatchHome, servers: Vec<IceServer>) -> anyhow::Result<Self> {
        let (ufrag, password) = IceReadiness::generate_credentials()?;
        Ok(Self::with_credentials(
            Some(home),
            IceCredentials { ufrag, password },
            servers,
            None,
        ))
    }

    /// Test-support constructor gathering on an in-memory network.
    #[doc(hidden)]
    pub fn for_test(credentials: IceCredentials, network: Arc<TestNetwork>) -> Self {
        Self::with_credentials(None, credentials, Vec::new(), Some(network))
    }

    fn with_credentials(
        home: Option<LatchHome>,
        credentials: IceCredentials,
        servers: Vec<IceServer>,
        test_network: Option<Arc<TestNetwork>>,
    ) -> Self {
        let (accepted, incoming) = mpsc::channel(ACCEPT_BACKLOG);
        Self {
            inner: Arc::new(Responder {
                home,
                credentials,
                servers,
                idle: Mutex::new(None),
                description: Mutex::new(None),
                accepted,
                incoming: Mutex::new(incoming),
                test_network,
            }),
        }
    }
}

impl Responder {
    /// Gathers one agent and records the description presence should publish.
    async fn gather(&self) -> anyhow::Result<IceReadiness> {
        let (endpoint, local) = match &self.test_network {
            Some(network) => {
                RtcEndpoint::gather_on_test_network(
                    self.credentials.clone(),
                    &self.servers,
                    Arc::clone(network),
                )
                .await
            }
            None => RtcEndpoint::gather(self.credentials.clone(), &self.servers).await,
        }
        .context("ICE candidate gathering failed")?;
        let readiness = self.readiness(&local);
        if readiness.candidates.is_empty() {
            bail!("the ICE agent gathered no publishable candidate");
        }
        *self.idle.lock().await = Some(endpoint);
        *self.description.lock().await = Some(readiness.clone());
        Ok(readiness)
    }

    /// Converts a gathered description into the published shape, dropping
    /// anything the control plane would refuse. An unroutable candidate helps
    /// no peer and is not worth telling the directory about.
    fn readiness(&self, local: &LocalDescription) -> IceReadiness {
        let publishable = self.test_network.is_none();
        let expires_at = candidate_lifetime_from_now();
        let candidates = local
            .candidates
            .iter()
            .map(|candidate| record(candidate, expires_at))
            .filter(|candidate| !publishable || candidate.validate(unix_now()).is_ok())
            .collect();
        IceReadiness {
            ufrag: local.credentials.ufrag.clone(),
            password: local.credentials.password.clone(),
            candidates,
        }
    }
}

#[async_trait]
impl PeerTransport for IceResponder {
    async fn start(&self) -> anyhow::Result<IceReadiness> {
        self.inner.gather().await
    }

    async fn offer(&self, offer: RemoteOffer) -> anyhow::Result<()> {
        let remote = remote_description(&offer);
        let endpoint = self
            .inner
            .idle
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("the ICE agent is already answering an offer"))?;
        let accepted = self.inner.accepted.clone();
        let home = self.inner.home.clone();
        // Connecting waits on the peer's connectivity checks, and gathering the
        // replacement waits on STUN. Neither may hold up the helper's accept
        // loop, so both run detached. A failed attempt is simply an offer that
        // produced no stream; the phone retries with a fresh one.
        tokio::spawn(async move {
            let connection = endpoint.connect(remote, Role::Responder).await;
            if let Some(home) = home {
                // A failed answer is the denominator of the connect rate, so
                // it is recorded as deliberately as a successful one.
                let _ = record_ice_answer(&home, connection.is_ok());
            }
            if let Ok(connection) = connection {
                let _ = accepted.send(peer_stream(connection)).await;
            }
        });
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _ = inner.gather().await;
        });
        Ok(())
    }

    async fn local_description(&self) -> Option<IceReadiness> {
        self.inner.description.lock().await.clone()
    }

    async fn accept(&self) -> Option<PeerStream> {
        // `recv` is cancel-safe and the lock is released with the future, so
        // losing a `select!` race never consumes an accepted stream.
        self.inner.incoming.lock().await.recv().await
    }
}

/// Bridges one connected data channel onto the `latch` crate's peer contract.
///
/// SCTP already preserves message boundaries, so a Noise record maps to exactly
/// one data-channel message and needs no length prefix of its own.
fn peer_stream(connection: RtcConnection) -> PeerStream {
    let route = peer_route(connection.selected_route());
    let channel = Arc::new(CloseOnDrop(Arc::new(connection)));
    PeerStream {
        reader: Box::new(RtcPeerReader(Arc::clone(&channel))),
        writer: Box::new(RtcPeerWriter(channel)),
        route,
    }
}

/// Maps the nominated pair onto the route the audit trail counts.
///
/// A connected channel always has a nominated pair, so `None` here means the
/// observation was lost rather than that the connection was routeless. It is
/// counted as unknown instead of being folded into direct: a silent
/// instrumentation gap that flatters the direct rate is worse than a visible
/// one.
fn peer_route(route: Option<SelectedRoute>) -> PeerRoute {
    match route {
        Some(SelectedRoute::Host) => PeerRoute::DirectHost,
        Some(SelectedRoute::Reflexive) => PeerRoute::DirectReflexive,
        Some(SelectedRoute::Relay) => PeerRoute::Relay,
        None => PeerRoute::Unknown,
    }
}

/// Closes ICE, SCTP, and the data channel once both halves are gone.
///
/// `proxy_connection` drops its halves when the device loses its pairing or its
/// grant, so this is what turns that decision into a closed network path rather
/// than a stream nobody is reading.
struct CloseOnDrop(Arc<RtcConnection>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let connection = Arc::clone(&self.0);
        tokio::spawn(async move {
            let _ = connection.close().await;
        });
    }
}

struct RtcPeerReader(Arc<CloseOnDrop>);
struct RtcPeerWriter(Arc<CloseOnDrop>);

#[async_trait]
impl PeerReader for RtcPeerReader {
    async fn read_record(&mut self) -> anyhow::Result<Vec<u8>> {
        tokio::time::timeout(PROXY_IDLE_TIMEOUT, self.0 .0.read())
            .await
            .map_err(|_| anyhow!("remote connection idle timeout"))?
            .map_err(|error| anyhow!("{error}"))
    }
}

#[async_trait]
impl PeerWriter for RtcPeerWriter {
    async fn write_record(&mut self, record: &[u8]) -> anyhow::Result<()> {
        self.0
             .0
            .write(record)
            .await
            .map_err(|error| anyhow!("{error}"))
    }
}

/// Rebuilds the peer's ICE description from an approved offer.
///
/// The offer's bounds — identifier shape, credential alphabet, candidate count,
/// routability, and lifetime — are enforced by `RemoteOffer::validate` where an
/// offer enters the process: once when the desktop app records one, and again
/// when the helper drains it. Re-deciding them here would only invite the two
/// checks to drift apart.
fn remote_description(offer: &RemoteOffer) -> RemoteDescription {
    RemoteDescription {
        credentials: IceCredentials {
            ufrag: offer.ice_ufrag.clone(),
            password: offer.ice_pwd.clone(),
        },
        candidates: offer.candidates.iter().map(transport_candidate).collect(),
    }
}

/// Converts a published candidate into the transport's representation.
fn transport_candidate(candidate: &IceCandidateRecord) -> TransportCandidate {
    TransportCandidate {
        candidate_type: candidate.candidate_type.clone(),
        priority: candidate.priority,
        foundation: candidate.foundation.clone(),
        component: candidate.component,
        protocol: candidate.protocol.clone(),
        address: candidate.address.clone(),
        related_address: candidate.related_address.clone(),
        related_port: candidate.related_port,
        tcp_type: candidate.tcp_type.clone(),
    }
}

fn record(candidate: &TransportCandidate, expires_at: u64) -> IceCandidateRecord {
    IceCandidateRecord {
        candidate_type: candidate.candidate_type.clone(),
        priority: candidate.priority,
        foundation: candidate.foundation.clone(),
        component: candidate.component,
        protocol: candidate.protocol.clone(),
        address: candidate.address.clone(),
        related_address: candidate.related_address.clone(),
        related_port: candidate.related_port,
        tcp_type: candidate.tcp_type.clone(),
        expires_at,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
