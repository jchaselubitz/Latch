//! Composed fault injection on the real WSS path: a relay that admits sockets
//! by role, reports peer presence, replaces roles, and can be killed
//! underneath a link. Every case runs the shared Rust core end to end
//! (native TLS, WebSocket, Noise XX, Yamux) through the Mac proxy to a
//! gateway socket.
// The tungstenite handshake callback carries its error response by value.
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use latch::cli::remote_access::{
    authorize_enrollment, proxy_authenticated_stream_for_test, remote_link_identity, set_enabled,
    DevicePermission,
};
use latch::session::paths::LatchHome;
use latch_transport::link::{
    LinkConfig, LinkError, LinkPurpose, LinkRole, LinkTimings, SecureLink, Service, WssRecordIo,
};
use openssl::{pkcs12::Pkcs12, pkey::PKey, stack::Stack, x509::X509};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_native_tls::TlsAcceptor;
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message};
use zeroize::Zeroizing;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

fn decode(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}

fn test_certificates(subject: &str) -> (TlsAcceptor, String) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // Distinct subject names: see remote_link_composed.rs.
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Latch recovery-test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params = CertificateParams::new(vec![subject.to_owned()]).unwrap();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, subject);
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
    let leaf_x509 = X509::from_pem(leaf.pem().as_bytes()).unwrap();
    let ca_x509 = X509::from_pem(ca.pem().as_bytes()).unwrap();
    let pkey = PKey::private_key_from_pem(leaf_key.serialize_pem().as_bytes()).unwrap();
    let mut chain = Stack::new().unwrap();
    chain.push(ca_x509).unwrap();
    let mut bundle = Pkcs12::builder();
    bundle
        .name("latch-recovery-test")
        .pkey(&pkey)
        .cert(&leaf_x509)
        .ca(chain);
    let der = bundle.build2("latch-test").unwrap().to_der().unwrap();
    let identity = native_tls::Identity::from_pkcs12(&der, "latch-test").unwrap();
    (
        TlsAcceptor::from(native_tls::TlsAcceptor::new(identity).unwrap()),
        ca.pem(),
    )
}

/// What the harness relay may be told to do to a live room.
#[derive(Clone, Copy)]
enum Fault {
    /// Drop both sockets abruptly, like a relay restart.
    KillRoom,
    /// Set the host socket aside without closing it and never touch it again:
    /// the stranded-path case. No FIN, no RST, no close frame, no traffic --
    /// exactly what an endpoint sees when the network silently stops
    /// delivering a connection it still believes is ESTABLISHED.
    StrandHost,
    /// Keep the host socket in the room but send it nothing except WebSocket
    /// pings, as the production relay does every fifteen seconds while a room
    /// waits. This is a healthy idle carrier, not a stranded one.
    PingHost,
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_native_tls::TlsStream<tokio::net::TcpStream>>;

/// A relay with the production room semantics this objective depends on: one
/// socket per role admitted by bearer prefix, `lease_started` on admission,
/// `peer_ready` to both once both are present, `peer_unavailable` when a
/// record arrives for an absent peer, and role replacement that closes the
/// old socket. It accepts sockets forever so reconnection can be exercised.
struct HarnessRelay {
    url: String,
    ca: String,
    faults: mpsc::Sender<Fault>,
    /// Admitted sockets in order, by bearer.
    admitted: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HarnessRelay {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (acceptor, ca) = test_certificates("localhost");
        let (faults, mut fault_rx) = mpsc::channel::<Fault>(4);
        let admitted = Arc::new(Mutex::new(Vec::new()));
        let admitted_log = admitted.clone();
        let task = tokio::spawn(async move {
            let (sockets_tx, mut sockets_rx) = mpsc::channel::<(String, Socket)>(8);
            let admitted_log = admitted_log.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((tcp, _)) = listener.accept().await else {
                        break;
                    };
                    let acceptor = acceptor.clone();
                    let sockets_tx = sockets_tx.clone();
                    let admitted_log = admitted_log.clone();
                    tokio::spawn(async move {
                        let Ok(tls) = acceptor.accept(tcp).await else {
                            return;
                        };
                        let bearer = Arc::new(Mutex::new(String::new()));
                        let captured = bearer.clone();
                        let Ok(socket) = accept_hdr_async(
                            tls,
                            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                  response| {
                                let authorization = request
                                    .headers()
                                    .get("authorization")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .trim_start_matches("Bearer ")
                                    .to_owned();
                                *captured.lock().unwrap() = authorization;
                                Ok(response)
                            },
                        )
                        .await
                        else {
                            return;
                        };
                        let bearer = bearer.lock().unwrap().clone();
                        admitted_log.lock().unwrap().push(bearer.clone());
                        let _ = sockets_tx.send((bearer, socket)).await;
                    });
                }
            });
            // Room state: at most one host and one controller.
            let mut host: Option<(String, Socket)> = None;
            let mut controller: Option<(String, Socket)> = None;
            let mut lease = 0_u32;
            // Sockets held open and never polled again. Dropping them would
            // close the TCP connection, which is the one thing a stranded
            // path never does.
            let mut stranded: Vec<Socket> = Vec::new();
            let mut ping_interval: Option<Duration> = None;
            loop {
                let ping = ping_interval;
                tokio::select! {
                    Some((bearer, mut socket)) = sockets_rx.recv() => {
                        let is_host = bearer.starts_with("host");
                        lease += 1;
                        socket.send(Message::Text(format!(
                            r#"{{"type":"lease_started","leaseId":"lease_harness_{lease:08}","expiresAt":4102444800}}"#
                        ).into())).await.unwrap();
                        let slot = if is_host { &mut host } else { &mut controller };
                        if let Some((_, mut previous)) = slot.take() {
                            // Role replacement closes the old socket of that
                            // role and, as in production, the whole room.
                            let _ = previous.close(None).await;
                            let other = if is_host { &mut controller } else { &mut host };
                            if let Some((_, mut other)) = other.take() {
                                let _ = other.close(None).await;
                            }
                        }
                        let slot = if is_host { &mut host } else { &mut controller };
                        *slot = Some((bearer, socket));
                        if let (Some((_, host)), Some((_, controller))) = (host.as_mut(), controller.as_mut()) {
                            host.send(Message::Text(r#"{"type":"peer_ready"}"#.into())).await.unwrap();
                            controller.send(Message::Text(r#"{"type":"peer_ready"}"#.into())).await.unwrap();
                        }
                    }
                    Some(fault) = fault_rx.recv() => match fault {
                        Fault::KillRoom => {
                            // Abrupt: drop without a close frame.
                            host = None;
                            controller = None;
                        }
                        Fault::StrandHost => {
                            if let Some((_, socket)) = host.take() {
                                stranded.push(socket);
                            }
                        }
                        Fault::PingHost => ping_interval = Some(Duration::from_millis(50)),
                    },
                    () = async {
                        match ping {
                            Some(interval) => tokio::time::sleep(interval).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some((_, socket)) = host.as_mut() {
                            let _ = socket.send(Message::Ping(Vec::new().into())).await;
                        }
                    }
                    message = async {
                        match host.as_mut() {
                            Some((_, socket)) => socket.next().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match message {
                            Some(Ok(Message::Binary(bytes))) => {
                                if let Some((_, peer)) = controller.as_mut() {
                                    let _ = peer.send(Message::Binary(bytes)).await;
                                } else if let Some((_, own)) = host.as_mut() {
                                    let _ = own.send(Message::Text(r#"{"type":"peer_unavailable"}"#.into())).await;
                                }
                            }
                            Some(Ok(_)) => {}
                            None | Some(Err(_)) => {
                                host = None;
                                if let Some((_, mut other)) = controller.take() { let _ = other.close(None).await; }
                            }
                        }
                    }
                    message = async {
                        match controller.as_mut() {
                            Some((_, socket)) => socket.next().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match message {
                            Some(Ok(Message::Binary(bytes))) => {
                                if let Some((_, peer)) = host.as_mut() {
                                    let _ = peer.send(Message::Binary(bytes)).await;
                                } else if let Some((_, own)) = controller.as_mut() {
                                    let _ = own.send(Message::Text(r#"{"type":"peer_unavailable"}"#.into())).await;
                                }
                            }
                            Some(Ok(_)) => {}
                            None | Some(Err(_)) => {
                                controller = None;
                                if let Some((_, mut other)) = host.take() { let _ = other.close(None).await; }
                            }
                        }
                    }
                }
            }
        });
        Self {
            url: format!("wss://localhost:{}/v1/connect", address.port()),
            ca,
            faults,
            admitted,
            task,
        }
    }
}

impl Drop for HarnessRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Endpoints {
    home: LatchHome,
    _directory: tempfile::TempDir,
    controller_keys: snow::Keypair,
    host_private: Vec<u8>,
    host_public: Vec<u8>,
}

fn endpoints() -> Endpoints {
    let directory = tempfile::tempdir().unwrap();
    let home = LatchHome::new(directory.path());
    set_enabled(&home, true).unwrap();
    let controller_keys = snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
        .generate_keypair()
        .unwrap();
    authorize_enrollment(
        &home,
        &format!("enr_{}", "1".repeat(32)),
        &hex(&controller_keys.public),
        "Recovery phone",
        DevicePermission::Control,
        &format!("dev_{}", "2".repeat(32)),
    )
    .unwrap();
    let identity = remote_link_identity(&home).unwrap();
    Endpoints {
        home,
        _directory: directory,
        controller_keys,
        host_private: decode(&identity.private_key),
        host_public: decode(&identity.public_key),
    }
}

fn fast() -> LinkTimings {
    LinkTimings {
        keepalive_interval: Duration::from_millis(100),
        dead_peer_timeout: Duration::from_millis(600),
    }
}

impl Endpoints {
    fn host_config(&self) -> LinkConfig {
        LinkConfig {
            purpose: LinkPurpose::Session,
            role: LinkRole::Host,
            local_private_key: Zeroizing::new(self.host_private.clone()),
            local_public_key: self.host_public.clone(),
            expected_remote_public_key: Some(self.controller_keys.public.clone()),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 1,
            timings: fast(),
        }
    }

    fn controller_config(&self) -> LinkConfig {
        LinkConfig {
            purpose: LinkPurpose::Session,
            role: LinkRole::Controller,
            local_private_key: Zeroizing::new(self.controller_keys.private.clone()),
            local_public_key: self.controller_keys.public.clone(),
            expected_remote_public_key: Some(self.host_public.clone()),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 1,
            timings: fast(),
        }
    }
}

/// Connects both roles through the relay the way the helper and the phone
/// do: each waits for the relay's peer report before its handshake deadline
/// starts, so whichever arrives first simply waits.
async fn establish_pair(
    relay: &HarnessRelay,
    endpoints: &Endpoints,
    host_admission: &str,
    controller_admission: &str,
) -> (Arc<SecureLink>, Arc<SecureLink>) {
    let (mut host_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, host_admission, relay.ca.as_bytes())
            .await
            .unwrap();
    // The host is alone in the room: with the production 10-second handshake
    // deadline this would once have expired. Now it waits for the peer.
    assert!(!host_records.peer_is_ready());
    let host_wait = tokio::spawn(async move {
        host_records.wait_for_peer(None).await.unwrap();
        host_records
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !host_wait.is_finished(),
        "the host must keep waiting until the phone joins"
    );
    let (mut controller_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, controller_admission, relay.ca.as_bytes())
            .await
            .unwrap();
    controller_records
        .wait_for_peer(Some(Duration::from_secs(5)))
        .await
        .unwrap();
    let host_records = host_wait.await.unwrap();
    assert!(host_records.peer_is_ready());
    let host = SecureLink::establish(host_records, endpoints.host_config());
    let controller = SecureLink::establish(controller_records, endpoints.controller_config());
    tokio::try_join!(host, controller).unwrap()
}

/// A gateway that answers one request with a bounded body after a delay,
/// so response completion can be observed rather than assumed.
async fn slow_gateway(body: &'static str, delay: Duration) -> std::net::SocketAddr {
    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = gateway.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = gateway.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|value| value == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                tokio::time::sleep(delay).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                // The body arrives in two pieces with a pause, so a proxy that
                // closed on the first EOF-looking boundary would truncate it.
                let (first, second) = body.split_at(body.len() / 2);
                stream.write_all(first.as_bytes()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
                stream.write_all(second.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    address
}

fn serve_streams(
    host: Arc<SecureLink>,
    endpoints_home: LatchHome,
    controller_key: String,
    gateway_address: std::net::SocketAddr,
) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let mut served = 0;
        while let Ok((header, stream)) = host.accept(LinkPurpose::Session).await {
            assert_eq!(header.service, Service::Gateway);
            let home = endpoints_home.clone();
            let key = controller_key.clone();
            tokio::spawn(async move {
                let _ = proxy_authenticated_stream_for_test(
                    &home,
                    stream,
                    &key,
                    1,
                    "recovery-gateway-token",
                    gateway_address,
                )
                .await;
            });
            served += 1;
        }
        served
    })
}

async fn request(controller: &SecureLink) -> Result<String, String> {
    let mut stream = controller
        .open(Service::Gateway, 1)
        .await
        .map_err(|error| format!("open: {error}"))?;
    stream
        .write_all(b"GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\n")
        .await
        .map_err(|error| format!("write: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("flush: {error}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|error| format!("read: {error}"))?;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_loss_closes_both_ends_and_a_fresh_admission_reconnects_without_gateway_restart() {
    let relay = HarnessRelay::start().await;
    let endpoints = endpoints();
    let body = "x".repeat(40_000);
    let body: &'static str = Box::leak(body.into_boxed_str());
    let gateway_address = slow_gateway(body, Duration::from_millis(150)).await;
    let controller_key = hex(&endpoints.controller_keys.public);

    let (host, controller) = establish_pair(&relay, &endpoints, "host-1", "controller-1").await;
    let serving = serve_streams(
        host.clone(),
        endpoints.home.clone(),
        controller_key.clone(),
        gateway_address,
    );

    // A complete response arrives before the logical stream ends.
    let response = request(&controller)
        .await
        .expect("first request on the first pair");
    assert!(
        response.ends_with(body),
        "the final response must be delivered in full before EOF"
    );

    // Relay restart: both sockets vanish without a close frame. Both links
    // notice within the dead-peer bound and every accept/read fails closed.
    relay.faults.send(Fault::KillRoom).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), controller.closed())
        .await
        .expect("controller did not observe relay loss");
    tokio::time::timeout(Duration::from_secs(3), host.closed())
        .await
        .expect("host did not observe relay loss");
    assert!(
        request(&controller).await.is_err(),
        "a dead link must not serve requests"
    );
    let served_before = tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .expect("host accept loop did not end with the link")
        .unwrap();
    assert_eq!(served_before, 1);

    // Fresh admissions for both roles reconnect through the same gateway
    // (the same loopback address, no restart) and serve again.
    let (host, controller) = establish_pair(&relay, &endpoints, "host-2", "controller-2").await;
    let serving = serve_streams(
        host.clone(),
        endpoints.home.clone(),
        controller_key,
        gateway_address,
    );
    let response = request(&controller)
        .await
        .expect("request on the re-admitted pair");
    assert!(response.ends_with(body));
    assert_eq!(
        relay.admitted.lock().unwrap().clone(),
        vec!["host-1", "controller-1", "host-2", "controller-2"]
    );
    controller.close().await;
    host.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(3), serving).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_controller_admission_replaces_the_old_link_and_the_old_stream_dies_with_it() {
    let relay = HarnessRelay::start().await;
    let endpoints = endpoints();
    let gateway_address = slow_gateway("replacement-ok", Duration::from_millis(50)).await;
    let controller_key = hex(&endpoints.controller_keys.public);

    let (host, controller) = establish_pair(&relay, &endpoints, "host-1", "controller-1").await;
    let _serving = serve_streams(
        host.clone(),
        endpoints.home.clone(),
        controller_key.clone(),
        gateway_address,
    );
    let mut lingering = controller.open(Service::Gateway, 1).await.unwrap();

    // The phone comes back on a new socket (foreground after a silent loss)
    // while the old one is still admitted. The relay replaces the role and
    // closes the room; the host reconnects with its own fresh admission and
    // the new controller authenticates against it. The old stream is dead,
    // not silently joined to the replacement.
    let (mut new_controller_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, "controller-2", relay.ca.as_bytes())
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(3), controller.closed())
        .await
        .expect("the replaced controller link did not close");
    tokio::time::timeout(Duration::from_secs(3), host.closed())
        .await
        .expect("the host link did not close on replacement");
    let mut leftover = Vec::new();
    assert!(
        lingering.read_to_end(&mut leftover).await.is_err() || leftover.is_empty(),
        "bytes from the replaced generation must not reach the old stream"
    );

    let (mut host_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, "host-2", relay.ca.as_bytes())
            .await
            .unwrap();
    host_records.wait_for_peer(None).await.unwrap();
    new_controller_records
        .wait_for_peer(Some(Duration::from_secs(5)))
        .await
        .unwrap();
    let host = SecureLink::establish(host_records, endpoints.host_config());
    let new_controller =
        SecureLink::establish(new_controller_records, endpoints.controller_config());
    let (host, new_controller) = tokio::try_join!(host, new_controller).unwrap();
    let serving = serve_streams(
        host.clone(),
        endpoints.home.clone(),
        controller_key,
        gateway_address,
    );
    let response = request(&new_controller)
        .await
        .expect("request on the replacement link");
    assert!(response.ends_with("replacement-ok"));
    new_controller.close().await;
    host.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(3), serving).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_controller_alone_in_the_room_learns_the_mac_is_offline_within_its_bound() {
    let relay = HarnessRelay::start().await;
    let (mut controller_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, "controller-only", relay.ca.as_bytes())
            .await
            .unwrap();
    let started = std::time::Instant::now();
    let outcome = controller_records
        .wait_for_peer(Some(Duration::from_millis(400)))
        .await;
    assert!(matches!(
        outcome,
        Err(latch_transport::link::LinkError::PeerUnavailable)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The reported cellular failure: the helper's relay socket stops delivering
/// while it waits for the phone, and no close frame can reach it to say so.
/// The wait must end on a local bound and hand the caller back to the
/// admission loop, and a fresh admission must then pair normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silently_stranded_relay_socket_ends_its_wait_and_a_fresh_admission_pairs() {
    let relay = HarnessRelay::start().await;
    let endpoints = endpoints();
    let (mut host_records, _control) =
        WssRecordIo::connect_with_test_ca(&relay.url, "host-stranded", relay.ca.as_bytes())
            .await
            .unwrap();
    // The production bound is forty-five seconds of relay silence; the same
    // code path is exercised here in milliseconds.
    host_records.set_inactivity_timeout(Some(Duration::from_millis(400)));
    relay.faults.send(Fault::StrandHost).await.unwrap();
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), host_records.wait_for_peer(None))
        .await
        .expect("the pre-authentication wait never returned");
    assert!(
        matches!(outcome, Err(LinkError::Timeout)),
        "a stranded carrier must end the wait with a timeout, got {outcome:?}"
    );
    // Bounded, not merely eventual: well inside the ten-second rescue above
    // even when the whole suite is competing for cores.
    assert!(started.elapsed() < Duration::from_secs(5));

    // Recovery is a fresh admission on a new socket, which is what the helper
    // asks Desktop for. The phone then reaches the Mac through it.
    let controller_key = hex(&endpoints.controller_keys.public);
    let gateway_address = slow_gateway("recovered-ok", Duration::from_millis(10)).await;
    let (host, controller) =
        establish_pair(&relay, &endpoints, "host-fresh", "controller-fresh").await;
    let serving = serve_streams(
        host.clone(),
        endpoints.home.clone(),
        controller_key,
        gateway_address,
    );
    let response = request(&controller)
        .await
        .expect("request over the re-admitted link");
    assert!(response.ends_with("recovered-ok"));
    controller.close().await;
    host.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(3), serving).await;
}

/// The other half of the bound: an idle host whose relay is merely quiet --
/// no phone, but pings still arriving -- must keep the socket it has. A bound
/// that counted "no peer yet" as silence would churn admissions all day.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_host_that_still_hears_relay_pings_keeps_waiting_on_the_same_socket() {
    let relay = HarnessRelay::start().await;
    let endpoints = endpoints();
    let (mut host_records, _control) =
        WssRecordIo::connect_with_test_ca(&relay.url, "host-idle", relay.ca.as_bytes())
            .await
            .unwrap();
    // Twenty-four ping periods, so only genuine silence can expire this: a
    // loaded scheduler delaying a ping must not be mistaken for a dead path.
    host_records.set_inactivity_timeout(Some(Duration::from_millis(1200)));
    relay.faults.send(Fault::PingHost).await.unwrap();
    let waiting = tokio::spawn(async move {
        host_records
            .wait_for_peer(None)
            .await
            .map(|()| host_records)
    });
    // Past the bound twice over: pings alone must hold the wait open.
    tokio::time::sleep(Duration::from_millis(2600)).await;
    assert!(
        !waiting.is_finished(),
        "relay pings are proof of liveness; the wait must not expire"
    );

    // And the socket is still usable: the phone joins and authenticates on it.
    let (mut controller_records, _) =
        WssRecordIo::connect_with_test_ca(&relay.url, "controller-idle", relay.ca.as_bytes())
            .await
            .unwrap();
    controller_records
        .wait_for_peer(Some(Duration::from_secs(15)))
        .await
        .expect("the phone was not reported to the room");
    let host_records = tokio::time::timeout(Duration::from_secs(15), waiting)
        .await
        .expect("the host wait did not resolve once the phone arrived")
        .unwrap()
        .expect("the idle host socket was discarded");
    let host = SecureLink::establish(host_records, endpoints.host_config());
    let controller = SecureLink::establish(controller_records, endpoints.controller_config());
    let (host, controller) = tokio::try_join!(host, controller).unwrap();
    controller.close().await;
    host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_link_from_a_foreign_thread_joins_every_task() {
    let relay = HarnessRelay::start().await;
    let endpoints = endpoints();
    let (host, controller) = establish_pair(&relay, &endpoints, "host-1", "controller-1").await;
    let opened = controller.open(Service::Gateway, 1).await.unwrap();
    // A blocked writer on one stream must not delay teardown started from a
    // plain OS thread, which is how Swift's runtime reaches the FFI.
    let blocked = tokio::spawn(async move {
        let mut opened = opened;
        let _ = opened.write_all(&vec![0x5a; 16 * 1024 * 1024]).await;
    });
    let handle = tokio::runtime::Handle::current();
    let joined = std::thread::spawn(move || {
        handle.block_on(async move {
            let started = std::time::Instant::now();
            controller.close().await;
            started.elapsed()
        })
    })
    .join()
    .unwrap();
    assert!(
        joined < Duration::from_secs(5),
        "close must be bounded even with a blocked writer"
    );
    blocked.abort();
    let _ = blocked.await;
    tokio::time::timeout(Duration::from_secs(3), host.closed())
        .await
        .expect("peer closure was not observed");
    host.close().await;
}
