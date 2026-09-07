//! The helper's ICE responder.
//!
//! The desktop app collects rendezvous offers from the control plane, checks
//! each peer against the local device store, and hands the survivors to this
//! agent. Reaching the agent authorizes nothing: an offer carries transport
//! parameters only, and the Noise handshake in the `latch` crate is still the
//! only thing that decides who the peer is and what it may do.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use latch::cli::remote_access::{
    candidate_lifetime_from_now, load_relay_servers, record_ice_answer, IceAnswerOutcome,
    IceCandidateRecord, IceReadiness, PeerReader, PeerRoute, PeerStream, PeerTransport, PeerWriter,
    RemoteOffer, PROXY_IDLE_TIMEOUT,
};
use latch::session::paths::LatchHome;
use latch_transport::policy::IceServer;
use latch_transport::rtc::{
    IceCredentials, LocalDescription, RemoteDescription, Role, RtcConnection, RtcEndpoint,
    SelectedRoute, TestNetwork, TransportCandidate,
};
use tokio::sync::{mpsc, Mutex, Notify, Semaphore};

/// How many connected-but-unclaimed streams may queue before the helper drains
/// them. `serve_lan` accepts continuously, so this only absorbs the gap between
/// a data channel opening and the accept loop's next turn.
const ACCEPT_BACKLOG: usize = 4;

// Leave room inside the phone's 15-second check deadline for nomination.
const REPLACEMENT_WAIT: Duration = Duration::from_secs(8);

/// How old an unused agent may get before it is gathered again.
///
/// An agent gathered at launch describes the network the Mac was on at
/// launch. A laptop that has since moved between Wi-Fi networks, joined or
/// left a tailnet, or had its NAT mapping expire would otherwise keep
/// publishing addresses that reach nothing until the helper is restarted,
/// and a phone dials each of those before it falls through to ICE. Two
/// minutes keeps presence honest without churning ports under a phone that
/// read them a moment ago.
const REGATHER_AFTER: Duration = Duration::from_secs(120);

/// How often the age of the idle agent is checked.
const REGATHER_CHECK_INTERVAL: Duration = Duration::from_secs(15);

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
    /// re-gathered. Offers wait briefly for its replacement in background tasks.
    idle: Mutex<Option<IdleAgent>>,
    idle_ready: Notify,
    offer_slots: Arc<Semaphore>,
    /// See [`REGATHER_AFTER`]; overridable so a test need not wait minutes.
    regather_after: Duration,
    description: Mutex<Option<IceReadiness>>,
    accepted: mpsc::Sender<PeerStream>,
    incoming: Mutex<mpsc::Receiver<PeerStream>>,
    /// Set only by [`IceResponder::for_test`]. Real gathering excludes loopback
    /// on purpose, so a round-trip test needs an in-memory network instead.
    test_network: Option<Arc<TestNetwork>>,
}

/// A gathered agent waiting for an offer, and when it gathered.
struct IdleAgent {
    endpoint: RtcEndpoint,
    gathered_at: Instant,
}

impl IceResponder {
    /// Builds a responder with freshly minted short-term ICE credentials.
    ///
    /// `servers` are STUN URLs used for server-reflexive gathering. Passing
    /// none is valid and yields host candidates only — the LAN and tailnet
    /// case, where every usable address is already on an interface. Relays
    /// are not passed here: the desktop app records the ones the control
    /// plane issued, and each gather reads them fresh (see
    /// [`Responder::servers_for_gather`]).
    pub fn new(home: LatchHome, servers: Vec<IceServer>) -> anyhow::Result<Self> {
        let (ufrag, password) = IceReadiness::generate_credentials()?;
        Ok(Self::with_credentials(
            Some(home),
            IceCredentials { ufrag, password },
            servers,
            None,
            REGATHER_AFTER,
        ))
    }

    /// Test-support constructor gathering on an in-memory network.
    #[doc(hidden)]
    pub fn for_test(credentials: IceCredentials, network: Arc<TestNetwork>) -> Self {
        Self::with_credentials(None, credentials, Vec::new(), Some(network), REGATHER_AFTER)
    }

    /// Test-support constructor that re-gathers an idle agent after `after`.
    #[doc(hidden)]
    pub fn for_test_regathering_after(
        credentials: IceCredentials,
        network: Arc<TestNetwork>,
        after: Duration,
    ) -> Self {
        Self::with_credentials(None, credentials, Vec::new(), Some(network), after)
    }

    fn with_credentials(
        home: Option<LatchHome>,
        credentials: IceCredentials,
        servers: Vec<IceServer>,
        test_network: Option<Arc<TestNetwork>>,
        regather_after: Duration,
    ) -> Self {
        let (accepted, incoming) = mpsc::channel(ACCEPT_BACKLOG);
        Self {
            inner: Arc::new(Responder {
                home,
                credentials,
                servers,
                idle: Mutex::new(None),
                idle_ready: Notify::new(),
                offer_slots: Arc::new(Semaphore::new(ACCEPT_BACKLOG)),
                regather_after,
                description: Mutex::new(None),
                accepted,
                incoming: Mutex::new(incoming),
                test_network,
            }),
        }
    }

    /// Keeps the idle agent no older than the re-gather threshold.
    ///
    /// The replacement is gathered before the old agent is released, so an
    /// offer that arrives mid-refresh still finds an agent to answer it rather
    /// than a gap. The loop ends with the responder: it holds the only other
    /// reference, and a helper that is shutting down has nothing to refresh.
    fn spawn_refresh(&self) {
        let inner = Arc::clone(&self.inner);
        let interval = REGATHER_CHECK_INTERVAL
            .min(inner.regather_after / 2)
            .max(Duration::from_millis(50));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if Arc::strong_count(&inner) == 1 {
                    return;
                }
                if inner.idle_is_stale().await {
                    let _ = inner.gather().await;
                }
            }
        });
    }
}

impl Responder {
    /// STUN from the launch arguments plus whatever relay the desktop app
    /// has recorded since.
    ///
    /// Read at every gather, so a refreshed credential reaches the next agent
    /// without a relaunch and an expired one simply drops out. The relay is
    /// what makes this Mac reachable from a phone whose traffic the Mac's NAT
    /// refuses on the reflexive address: with a relay candidate of the Mac's
    /// own, the phone's packets arrive on the Mac's outbound TURN flow.
    fn servers_for_gather(&self) -> Vec<IceServer> {
        let mut servers = self.servers.clone();
        if let Some(home) = &self.home {
            servers.extend(load_relay_servers(home).into_iter().map(|relay| IceServer {
                url: relay.url,
                username: relay.username,
                credential: relay.credential,
            }));
        }
        servers
    }

    /// Gathers one agent and records the description presence should publish.
    async fn gather(&self) -> anyhow::Result<IceReadiness> {
        let servers = self.servers_for_gather();
        let (endpoint, local) = match &self.test_network {
            Some(network) => {
                RtcEndpoint::gather_on_test_network(
                    self.credentials.clone(),
                    &servers,
                    Arc::clone(network),
                )
                .await
            }
            None => RtcEndpoint::gather(self.credentials.clone(), &servers).await,
        }
        .context("ICE candidate gathering failed")?;
        let readiness = self.readiness(&local);
        if readiness.candidates.is_empty() {
            let _ = endpoint.close().await;
            bail!("the ICE agent gathered no publishable candidate");
        }
        let replaced = self.idle.lock().await.replace(IdleAgent {
            endpoint,
            gathered_at: Instant::now(),
        });
        *self.description.lock().await = Some(readiness.clone());
        self.idle_ready.notify_waiters();
        if let Some(previous) = replaced {
            // Its ports are no longer what presence advertises. A phone that
            // read them a moment ago still connects: the fresh agent answers
            // with the same credentials from new ports, which the phone
            // learns as peer-reflexive candidates from its checks.
            let _ = previous.endpoint.close().await;
        }
        Ok(readiness)
    }

    async fn take_idle(&self) -> anyhow::Result<RtcEndpoint> {
        tokio::time::timeout(REPLACEMENT_WAIT, async {
            loop {
                // Register before checking the slot, so a completed gather
                // cannot notify between the empty check and subscription.
                let ready = self.idle_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                if let Some(idle) = self.idle.lock().await.take() {
                    return idle.endpoint;
                }
                ready.await;
            }
        })
        .await
        .context("ICE replacement agent did not become ready before its deadline")
    }

    /// Whether the unused agent has outlived the re-gather threshold. An
    /// answering or re-gathering responder has no idle agent and is never
    /// stale: its replacement is already on the way.
    async fn idle_is_stale(&self) -> bool {
        self.idle
            .lock()
            .await
            .as_ref()
            .is_some_and(|idle| idle.gathered_at.elapsed() >= self.regather_after)
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
        log::info!(target: "latch_remote", "responder lifecycle=offer-handover-v1");
        let readiness = self.inner.gather().await?;
        self.spawn_refresh();
        Ok(readiness)
    }

    async fn offer(&self, offer: RemoteOffer) -> anyhow::Result<()> {
        let remote = remote_description(&offer);
        let attempt = latch_transport::diagnostics::fingerprint(&offer.ice_ufrag);
        log::info!(target: "latch_remote", "request={} attempt={attempt} offer received", offer.request_id);
        let permit = Arc::clone(&self.inner.offer_slots)
            .try_acquire_owned()
            .map_err(|_| anyhow!("the ICE offer backlog is full"))?;
        let inner = Arc::clone(&self.inner);
        let accepted = self.inner.accepted.clone();
        let home = self.inner.home.clone();
        // Connecting waits on the peer's connectivity checks, and gathering the
        // replacement waits on STUN. Neither may hold up the helper's accept
        // loop, so both run detached. A failed attempt is simply an offer that
        // produced no stream; the phone retries with a fresh one.
        tokio::spawn(async move {
            let _permit = permit;
            let waiting_since = Instant::now();
            let endpoint = match inner.take_idle().await {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    log::warn!(target: "latch_remote", "attempt={attempt} replacement wait failed: {error}");
                    if let Some(home) = &home {
                        let _ = record_ice_answer(home, IceAnswerOutcome::Failed("replacement"));
                    }
                    return;
                }
            };
            log::info!(target: "latch_remote", "attempt={attempt} replacement wait completed elapsed_ms={}", waiting_since.elapsed().as_millis());
            tokio::spawn(async move {
                if let Err(error) = inner.gather().await {
                    log::warn!(target: "latch_remote", "replacement gathering failed: {error}");
                }
            });
            let connection = endpoint.connect(remote, Role::Responder).await;
            match &connection {
                Ok(connection) => log::info!(
                    target: "latch_remote",
                    "attempt={attempt} answered an offer: connected, route {:?}",
                    connection.selected_route()
                ),
                Err(error) => {
                    log::warn!(target: "latch_remote", "attempt={attempt} answered an offer: {error}")
                }
            }
            if let Some(home) = home {
                // A failed answer is the denominator of the connect rate, so
                // it is recorded as deliberately as a successful one.
                let _ = record_ice_answer(
                    &home,
                    match &connection {
                        Ok(_) => IceAnswerOutcome::Connected,
                        Err(error) => IceAnswerOutcome::Failed(error.stage()),
                    },
                );
            }
            if let Ok(connection) = connection {
                let _ = accepted.send(peer_stream(connection)).await;
            }
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
    async fn finish(&mut self) -> anyhow::Result<()> {
        self.0 .0.drain().await.map_err(|error| anyhow!("{error}"))
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_offer_during_regather_waits_without_blocking_accepts() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let network = Arc::new(TestNetwork::new(Some(Default::default())));
            let mac_credentials = IceCredentials {
                ufrag: "mac-gap-test".into(),
                password: "mac-gap-test-password-with-enough-entropy".into(),
            };
            let responder = IceResponder::for_test(mac_credentials.clone(), network.clone());
            let published = responder.start().await.unwrap();
            // The old agent's addresses are still published while its
            // replacement is gathering. The phone has already read them.
            let consumed = responder.inner.idle.lock().await.take().unwrap();
            let (phone, description) = RtcEndpoint::gather_on_test_network(
                IceCredentials {
                    ufrag: "phone-gap-test".into(),
                    password: "phone-gap-test-password-with-enough-entropy".into(),
                },
                &[],
                network,
            )
            .await
            .unwrap();
            let offer = RemoteOffer {
                request_id: "a".repeat(32),
                peer_device_id: "b".repeat(32),
                ice_ufrag: description.credentials.ufrag,
                ice_pwd: description.credentials.password,
                candidates: description
                    .candidates
                    .iter()
                    .map(|c| record(c, unix_now() + 60))
                    .collect(),
                expires_at: unix_now() + 60,
            };
            tokio::time::timeout(Duration::from_millis(100), responder.offer(offer))
                .await
                .expect("offer handling must not block the helper's accept loop")
                .expect("an offer in the replacement gap must be retained");
            let dialing = tokio::spawn(async move {
                phone
                    .connect(
                        RemoteDescription {
                            credentials: mac_credentials,
                            candidates: published
                                .candidates
                                .iter()
                                .map(transport_candidate)
                                .collect(),
                        },
                        Role::Initiator,
                    )
                    .await
                    .unwrap()
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
            responder.inner.gather().await.unwrap();
            consumed.endpoint.close().await.unwrap();
            let mut peer = responder.accept().await.unwrap();
            let phone = dialing.await.unwrap();
            phone.write(b"request after handover").await.unwrap();
            assert_eq!(
                peer.reader.read_record().await.unwrap(),
                b"request after handover"
            );
            peer.writer
                .write_record(b"response after handover")
                .await
                .unwrap();
            assert_eq!(phone.read().await.unwrap(), b"response after handover");
            phone.close().await.unwrap();
        })
        .await
        .expect("replacement agent connects before the phone deadline");
    }
}
