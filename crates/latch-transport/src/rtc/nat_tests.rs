//! NAT traversal behaviour against simulated networks.
//!
//! These are the closest this repository can get to the field rows in
//! `docs/REMOTE_ACCESS_PHASE_4.md` without a phone on a carrier: a virtual
//! WAN, two LANs behind configurable NATs, and a real TURN server speaking the
//! real protocol to the real ICE agent. What they establish is that the
//! transport picks the right path for a given NAT pair and that the resulting
//! channel carries records — not that a particular carrier or hotel network
//! behaves like the simulation. The physical rows stay physical rows.
//!
//! Two topologies matter:
//!
//! * **Port-restricted cone on both sides.** Mappings are endpoint-independent,
//!   so each side's server-reflexive candidate is the address the other can
//!   actually reach. Hole punching works and the nominated pair must be
//!   reflexive. This is the home-NAT-to-cellular case.
//! * **Symmetric on both sides.** Mappings are endpoint-address-and-port
//!   dependent, so the reflexive candidate learned from the TURN server names a
//!   mapping that is useless to the peer. Hole punching cannot work, and the
//!   only pair that can be nominated is the relayed one.
//!
//! The second is the one that has to be simulated rather than reasoned about:
//! "relay wins when direct cannot" was previously an assertion about policy
//! code, and policy code is not what fails on a symmetric NAT.

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;

use tokio::sync::Mutex;
use webrtc_util::vnet::nat::{EndpointDependencyType, NatType};
use webrtc_util::vnet::net::{Net, NetConfig};
use webrtc_util::vnet::router::{Nic, Router, RouterConfig};

use super::*;

const TURN_SERVER_IP: &str = "1.2.3.4";
const TURN_SERVER_PORT: u16 = 3478;
const TURN_REALM: &str = "latch.test";
const TURN_USER: &str = "latch";
const TURN_PASSWORD: &str = "latch-turn-password";

/// A phone-side LAN and a Mac-side LAN, each behind its own NAT.
struct SimulatedInternet {
    wan: Arc<Mutex<Router>>,
    phone: Arc<Net>,
    mac: Arc<Net>,
    turn: turn::server::Server,
}

impl SimulatedInternet {
    async fn shutdown(self) {
        let _ = self.turn.close().await;
        let _ = self.wan.lock().await.stop().await;
    }
}

/// Endpoint-independent mapping, endpoint-address-and-port filtering: the
/// common consumer router, and the one a direct connection is expected to
/// survive.
fn port_restricted_cone() -> NatType {
    NatType {
        mapping_behavior: EndpointDependencyType::EndpointIndependent,
        filtering_behavior: EndpointDependencyType::EndpointAddrPortDependent,
        hair_pining: false,
        port_preservation: false,
        mapping_life_time: Duration::from_secs(30),
        ..Default::default()
    }
}

/// A new external port per destination, which is what makes a peer's knowledge
/// of the reflexive candidate worthless and forces the relay.
fn symmetric() -> NatType {
    NatType {
        mapping_behavior: EndpointDependencyType::EndpointAddrPortDependent,
        filtering_behavior: EndpointDependencyType::EndpointAddrPortDependent,
        hair_pining: false,
        port_preservation: false,
        mapping_life_time: Duration::from_secs(30),
        ..Default::default()
    }
}

async fn build_internet(
    phone_nat: NatType,
    mac_nat: NatType,
) -> Result<SimulatedInternet, Box<dyn std::error::Error>> {
    let wan = Arc::new(Mutex::new(Router::new(RouterConfig {
        cidr: "0.0.0.0/0".to_owned(),
        ..Default::default()
    })?));

    let turn_net = Arc::new(Net::new(Some(NetConfig {
        static_ips: vec![TURN_SERVER_IP.to_owned()],
        ..Default::default()
    })));
    attach_net(&turn_net, &wan).await?;

    let phone = attach_lan(
        &wan,
        "192.168.10.0/24",
        "192.168.10.2",
        "27.1.1.1",
        phone_nat,
    )
    .await?;
    let mac = attach_lan(&wan, "192.168.20.0/24", "192.168.20.2", "28.1.1.1", mac_nat).await?;

    wan.lock().await.start().await?;
    let turn = start_turn(turn_net).await?;

    Ok(SimulatedInternet {
        wan,
        phone,
        mac,
        turn,
    })
}

async fn attach_lan(
    wan: &Arc<Mutex<Router>>,
    cidr: &str,
    private_ip: &str,
    public_ip: &str,
    nat: NatType,
) -> Result<Arc<Net>, Box<dyn std::error::Error>> {
    let lan = Arc::new(Mutex::new(Router::new(RouterConfig {
        cidr: cidr.to_owned(),
        static_ips: vec![public_ip.to_owned()],
        nat_type: Some(nat),
        ..Default::default()
    })?));
    let net = Arc::new(Net::new(Some(NetConfig {
        static_ips: vec![private_ip.to_owned()],
        ..Default::default()
    })));
    attach_net(&net, &lan).await?;
    {
        let mut parent = wan.lock().await;
        parent.add_router(Arc::clone(&lan)).await?;
    }
    lan.lock().await.set_router(Arc::clone(wan)).await?;
    Ok(net)
}

async fn attach_net(
    net: &Arc<Net>,
    router: &Arc<Mutex<Router>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let nic = net.get_nic()?;
    router.lock().await.add_net(Arc::clone(&nic)).await?;
    nic.lock().await.set_router(Arc::clone(router)).await?;
    Ok(())
}

struct StaticCredential(HashMap<String, Vec<u8>>);

impl turn::auth::AuthHandler for StaticCredential {
    fn auth_handle(
        &self,
        username: &str,
        _realm: &str,
        _source: SocketAddr,
    ) -> Result<Vec<u8>, turn::Error> {
        self.0
            .get(username)
            .cloned()
            .ok_or_else(|| turn::Error::Other("unknown TURN user".to_owned()))
    }
}

async fn start_turn(net: Arc<Net>) -> Result<turn::server::Server, Box<dyn std::error::Error>> {
    let conn = net
        .bind(SocketAddr::from_str(&format!(
            "{TURN_SERVER_IP}:{TURN_SERVER_PORT}"
        ))?)
        .await?;
    let mut credentials = HashMap::new();
    credentials.insert(
        TURN_USER.to_owned(),
        turn::auth::generate_auth_key(TURN_USER, TURN_REALM, TURN_PASSWORD),
    );
    let server = turn::server::Server::new(turn::server::config::ServerConfig {
        conn_configs: vec![turn::server::config::ConnConfig {
            conn,
            relay_addr_generator: Box::new(
                turn::relay::relay_static::RelayAddressGeneratorStatic {
                    relay_address: IpAddr::from_str(TURN_SERVER_IP)?,
                    address: "0.0.0.0".to_owned(),
                    net,
                },
            ),
        }],
        realm: TURN_REALM.to_owned(),
        auth_handler: Arc::new(StaticCredential(credentials)),
        channel_bind_timeout: Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await?;
    Ok(server)
}

fn turn_servers() -> Vec<IceServer> {
    vec![IceServer {
        url: format!("turn:{TURN_SERVER_IP}:{TURN_SERVER_PORT}?transport=udp"),
        username: TURN_USER.to_owned(),
        credential: TURN_PASSWORD.to_owned(),
    }]
}

/// What the Mac's helper is actually launched with: STUN for a reflexive
/// candidate and nothing that could allocate a relay. The TURN server answers
/// plain binding requests too, so it doubles as the STUN server here.
fn stun_servers() -> Vec<IceServer> {
    vec![IceServer {
        url: format!("stun:{TURN_SERVER_IP}:{TURN_SERVER_PORT}"),
        username: String::new(),
        credential: String::new(),
    }]
}

fn credentials(ufrag: &str) -> IceCredentials {
    IceCredentials {
        ufrag: ufrag.to_owned(),
        // ICE requires at least 128 bits of password entropy; these are fixed
        // rather than random so a failure reproduces byte for byte.
        password: format!("{ufrag}-password-with-more-than-128-bits"),
    }
}

/// Gathers both ends, connects them, and reports the route each nominated.
///
/// Loopback is excluded exactly as it is in production. Including it would let
/// each agent pair against `127.0.0.1` on its own virtual stack, which is not
/// a path any phone has and would make the NAT under test irrelevant.
async fn connect_across(
    internet: &SimulatedInternet,
) -> (
    Option<SelectedRoute>,
    Option<SelectedRoute>,
    RtcConnection,
    RtcConnection,
) {
    connect_across_with(internet, &turn_servers(), &turn_servers(), Duration::ZERO).await
}

/// `connect_across` with each side's own server list and a Mac that starts
/// answering `mac_delay` after the phone has started its checks.
///
/// The delay is the production hand-off: the phone runs its checks the moment
/// its offer is accepted, while the Mac still has to collect the offer from
/// the control plane, authorize it, hand it to the helper, and have the
/// helper drain it — and until the Mac's agent sends its first check, the
/// Mac's NAT drops everything the phone sends to the reflexive address.
async fn connect_across_with(
    internet: &SimulatedInternet,
    phone_servers: &[IceServer],
    mac_servers: &[IceServer],
    mac_delay: Duration,
) -> (
    Option<SelectedRoute>,
    Option<SelectedRoute>,
    RtcConnection,
    RtcConnection,
) {
    let (phone, mac) = tokio::join!(
        RtcEndpoint::gather_with_network(
            credentials("phone"),
            phone_servers,
            false,
            Some(Arc::clone(&internet.phone)),
        ),
        RtcEndpoint::gather_with_network(
            credentials("mac"),
            mac_servers,
            false,
            Some(Arc::clone(&internet.mac)),
        )
    );
    let (phone_endpoint, phone_description) = phone.expect("the phone gathers candidates");
    let (mac_endpoint, mac_description) = mac.expect("the Mac gathers candidates");
    assert!(
        phone_description
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_type == "relay"),
        "a TURN server in the list must produce a relay candidate"
    );

    let (phone, mac) = tokio::join!(
        phone_endpoint.connect(
            RemoteDescription {
                credentials: mac_description.credentials,
                candidates: mac_description.candidates,
            },
            Role::Initiator,
        ),
        async {
            tokio::time::sleep(mac_delay).await;
            mac_endpoint
                .connect(
                    RemoteDescription {
                        credentials: phone_description.credentials,
                        candidates: phone_description.candidates,
                    },
                    Role::Responder,
                )
                .await
        }
    );
    let phone = phone.expect("the phone completes ICE, DTLS, and SCTP");
    let mac = mac.expect("the Mac completes ICE, DTLS, and SCTP");
    (phone.selected_route(), mac.selected_route(), phone, mac)
}

async fn assert_records_round_trip(phone: &RtcConnection, mac: &RtcConnection) {
    phone
        .write(b"phone noise ciphertext")
        .await
        .expect("the phone writes a record");
    assert_eq!(
        mac.read().await.expect("the Mac reads it"),
        b"phone noise ciphertext"
    );
    mac.write(b"mac noise ciphertext")
        .await
        .expect("the Mac writes a record");
    assert_eq!(
        phone.read().await.expect("the phone reads it"),
        b"mac noise ciphertext"
    );
}

#[tokio::test]
async fn cone_nats_on_both_sides_nominate_a_reflexive_pair() {
    let internet = build_internet(port_restricted_cone(), port_restricted_cone())
        .await
        .expect("the simulated internet starts");
    let (phone_route, mac_route, phone, mac) = connect_across(&internet).await;

    assert_eq!(
        phone_route,
        Some(SelectedRoute::Reflexive),
        "hole punching through cone NATs must produce a direct, reflexive pair"
    );
    assert_eq!(mac_route, Some(SelectedRoute::Reflexive));
    assert_records_round_trip(&phone, &mac).await;

    let _ = tokio::join!(phone.close(), mac.close());
    internet.shutdown().await;
}

#[tokio::test]
async fn symmetric_nats_on_both_sides_fall_to_the_relay() {
    let internet = build_internet(symmetric(), symmetric())
        .await
        .expect("the simulated internet starts");
    let (phone_route, mac_route, phone, mac) = connect_across(&internet).await;

    assert_eq!(
        phone_route,
        Some(SelectedRoute::Relay),
        "a symmetric NAT on both sides leaves the relayed pair as the only one \
         that can be nominated"
    );
    assert_eq!(mac_route, Some(SelectedRoute::Relay));
    assert_eq!(phone.selected_path(), SelectedPath::Relay);
    assert_records_round_trip(&phone, &mac).await;

    let _ = tokio::join!(phone.close(), mac.close());
    internet.shutdown().await;
}

/// The field failure: a phone on a carrier, a Mac on a home router, and a
/// Mac that starts answering a second or two after the phone started
/// checking.
///
/// The phone's checks reach the Mac's reflexive address only once the Mac's
/// own first check has opened its port-restricted NAT, which cannot happen
/// before the Mac has the offer. An ICE agent that gives each pair a fixed
/// handful of checks and then abandons it — and, as the controlling side,
/// never revives a pair on the strength of the Mac's later inbound check —
/// has already written every pair off by then. The Mac's check succeeds, the
/// phone answers it, and both then wait for a nomination that the phone will
/// never send: a timeout on both ends with a working path between them.
#[tokio::test]
async fn a_mac_that_answers_late_is_still_reached_before_the_phone_gives_up() {
    let internet = build_internet(symmetric(), port_restricted_cone())
        .await
        .expect("the simulated internet starts");
    let (phone_route, mac_route, phone, mac) = connect_across_with(
        &internet,
        &turn_servers(),
        &stun_servers(),
        // Past the budget an agent with the library's default of seven
        // checks at 200ms spends on a pair, with room for the timing to
        // land either side of it.
        Duration::from_millis(2_500),
    )
    .await;

    assert_eq!(
        phone_route,
        Some(SelectedRoute::Relay),
        "a symmetric NAT on the phone leaves the relayed pair as the one it can nominate"
    );
    assert_eq!(mac_route, Some(SelectedRoute::Relay));
    assert_records_round_trip(&phone, &mac).await;

    let _ = tokio::join!(phone.close(), mac.close());
    internet.shutdown().await;
}

/// A Mac whose router allocates a new external port per destination, with
/// no relay of its own: the phone's only route is its relay, and the only
/// Mac address that answers there is the one the Mac's own check comes from,
/// which the phone has to learn as a peer-reflexive candidate.
#[tokio::test]
async fn a_symmetric_mac_with_stun_only_is_reached_through_the_phones_relay() {
    let internet = build_internet(symmetric(), symmetric())
        .await
        .expect("the simulated internet starts");
    let (phone_route, mac_route, phone, mac) = connect_across_with(
        &internet,
        &turn_servers(),
        &stun_servers(),
        Duration::from_millis(500),
    )
    .await;

    assert_eq!(phone_route, Some(SelectedRoute::Relay));
    assert_eq!(mac_route, Some(SelectedRoute::Relay));
    assert_records_round_trip(&phone, &mac).await;

    let _ = tokio::join!(phone.close(), mac.close());
    internet.shutdown().await;
}

/// The same Mac against a phone on a cone NAT: hole punching is possible
/// from the Mac's side only, and the phone must find the Mac's
/// per-destination port from the Mac's inbound check.
#[tokio::test]
async fn a_symmetric_mac_with_stun_only_is_reached_directly_from_a_cone_phone() {
    let internet = build_internet(port_restricted_cone(), symmetric())
        .await
        .expect("the simulated internet starts");
    let (phone_route, mac_route, phone, mac) = connect_across_with(
        &internet,
        &turn_servers(),
        &stun_servers(),
        Duration::from_millis(500),
    )
    .await;

    assert_ne!(phone_route, None);
    assert_ne!(mac_route, None);
    assert_records_round_trip(&phone, &mac).await;
    let _ = tokio::join!(phone.close(), mac.close());
    internet.shutdown().await;
}

/// A TURN allocation can succeed even though later permission transactions
/// stop receiving replies. It must not park checks on unrelated direct paths.
#[tokio::test]
async fn a_stalled_turn_permission_does_not_block_direct_nomination() {
    stalled_permission_case(
        port_restricted_cone(),
        port_restricted_cone(),
        SelectedRoute::Reflexive,
    )
    .await;
}

#[tokio::test]
async fn a_stalled_turn_permission_does_not_block_a_healthy_remote_relay() {
    stalled_permission_case(symmetric(), symmetric(), SelectedRoute::Relay).await;
}

async fn stalled_permission_case(phone_nat: NatType, mac_nat: NatType, expected: SelectedRoute) {
    let internet = build_internet(phone_nat, mac_nat).await.unwrap();
    let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    internet
        .wan
        .lock()
        .await
        .add_chunk_filter(Box::new({
            let dropped = Arc::clone(&dropped);
            move |chunk| {
                let bytes = chunk.user_data();
                let permission = chunk.source_addr().ip().to_string() == "27.1.1.1"
                    && chunk.destination_addr().ip().to_string() == TURN_SERVER_IP
                    && bytes.starts_with(&[0, 8]); // STUN CreatePermission request
                if permission {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                !permission
            }
        }))
        .await;
    let connected = tokio::time::timeout(
        Duration::from_secs(6),
        connect_across_with(
            &internet,
            &turn_servers(),
            &turn_servers(),
            Duration::from_millis(600),
        ),
    )
    .await;
    assert!(
        dropped.load(Ordering::Relaxed) > 0,
        "test did not stall any permission request"
    );
    let (phone_route, mac_route, phone, mac) =
        connected.expect("one stalled TURN request blocked healthy direct checks and nomination");
    assert_eq!(phone_route, Some(expected));
    assert_eq!(mac_route, Some(expected));
    assert_records_round_trip(&phone, &mac).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        let _ = tokio::join!(phone.close(), mac.close());
        internet.shutdown().await;
    })
    .await
    .expect("stalled permission blocked shutdown");
}

/// A cellular client can reach TURN over IPv6 even though its allocated
/// peer-facing relay address is IPv4. Cloudflare uses exactly this split.
#[tokio::test]
async fn an_ipv6_turn_server_supplies_an_ipv4_relay() {
    let net = Arc::new(Net::new(None));
    let listener = net.bind("[::1]:0".parse().unwrap()).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut keys = HashMap::new();
    keys.insert(
        TURN_USER.to_owned(),
        turn::auth::generate_auth_key(TURN_USER, TURN_REALM, TURN_PASSWORD),
    );
    let server = turn::server::Server::new(turn::server::config::ServerConfig {
        conn_configs: vec![turn::server::config::ConnConfig {
            conn: listener,
            relay_addr_generator: Box::new(
                turn::relay::relay_static::RelayAddressGeneratorStatic {
                    relay_address: "127.0.0.1".parse().unwrap(),
                    address: "127.0.0.1".into(),
                    net: Arc::clone(&net),
                },
            ),
        }],
        realm: TURN_REALM.into(),
        auth_handler: Arc::new(StaticCredential(keys)),
        channel_bind_timeout: Duration::ZERO,
        alloc_close_notify: None,
    })
    .await
    .unwrap();
    let servers = [IceServer {
        url: format!("turn:[::1]:{port}?transport=udp"),
        username: TURN_USER.into(),
        credential: TURN_PASSWORD.into(),
    }];
    let (endpoint, local) = tokio::time::timeout(
        Duration::from_secs(10),
        RtcEndpoint::gather_with_network(
            credentials("ipv6-turn-client"),
            &servers,
            true,
            Some(net),
        ),
    )
    .await
    .expect("IPv6 TURN gathering finishes")
    .unwrap();
    let has_ipv4_relay = local
        .candidates
        .iter()
        .any(|c| c.candidate_type == "relay" && c.address.parse::<SocketAddr>().unwrap().is_ipv4());
    assert!(
        has_ipv4_relay,
        "IPv6 TURN allocation did not yield an IPv4 relay"
    );
    let relay = endpoint
        .agent
        .get_local_candidates()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.candidate_type() == CandidateType::Relay)
        .unwrap();
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target: Arc<dyn Candidate + Send + Sync> = Arc::new(
        webrtc_ice::candidate::candidate_host::CandidateHostConfig {
            base_config: webrtc_ice::candidate::candidate_base::CandidateBaseConfig {
                network: "udp".into(),
                address: "127.0.0.1".into(),
                port: peer.local_addr().unwrap().port(),
                component: 1,
                ..Default::default()
            },
            ..Default::default()
        }
        .new_candidate_host()
        .unwrap(),
    );
    relay
        .write_to(b"IPv6 client to IPv4 peer", target.as_ref())
        .await
        .unwrap();
    let mut packet = [0; 128];
    let (n, source) = tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut packet))
        .await
        .expect("relay forwards data across address families")
        .unwrap();
    assert_eq!(&packet[..n], b"IPv6 client to IPv4 peer");
    assert_eq!(source, relay.addr());
    endpoint.close().await.unwrap();
    server.close().await.unwrap();
    assert!(
        has_ipv4_relay,
        "an IPv6-only TURN listener must supply a usable IPv4 relay candidate"
    );
}

#[tokio::test]
async fn tcp_turn_urls_do_not_wait_for_udp_stun_responses() {
    // This UDP port intentionally never responds. A TCP/TLS URL must not
    // generate plaintext UDP STUN probes or charge their five-second timeout.
    let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let servers = [IceServer {
        url: format!("turns:127.0.0.1:{port}?transport=tcp"),
        username: TURN_USER.into(),
        credential: TURN_PASSWORD.into(),
    }];
    let (endpoint, _) = tokio::time::timeout(
        Duration::from_secs(2),
        RtcEndpoint::gather_with_network(credentials("no-udp-probe"), &servers, true, None),
    )
    .await
    .expect("TCP/TLS URLs must not wait for UDP responses")
    .unwrap();
    endpoint.close().await.unwrap();
}
