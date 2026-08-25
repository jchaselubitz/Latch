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
    let servers = turn_servers();
    let (phone, mac) = tokio::join!(
        RtcEndpoint::gather_with_network(
            credentials("phone"),
            &servers,
            false,
            Some(Arc::clone(&internet.phone)),
        ),
        RtcEndpoint::gather_with_network(
            credentials("mac"),
            &servers,
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
        mac_endpoint.connect(
            RemoteDescription {
                credentials: phone_description.credentials,
                candidates: phone_description.candidates,
            },
            Role::Responder,
        )
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
