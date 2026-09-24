//! The helper's LAN acceptor authenticates connections concurrently and
//! bounds each handshake, so a connection that never speaks cannot keep the
//! paired phone off the LAN path.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use latch_remote::link::{
    run_lan_acceptor, LanLink, LAN_AUTHENTICATION_LIMIT, LAN_HANDSHAKE_PERMITS,
};
use latch_transport::link::{
    LanRecordIo, LinkConfig, LinkPurpose, LinkRole, LinkTimings, SecureLink,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

fn keypair() -> snow::Keypair {
    snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
        .generate_keypair()
        .unwrap()
}

fn config(role: LinkRole, local: &snow::Keypair, remote: &[u8]) -> LinkConfig {
    LinkConfig {
        purpose: LinkPurpose::Session,
        role,
        local_private_key: Zeroizing::new(local.private.clone()),
        local_public_key: local.public.clone(),
        expected_remote_public_key: Some(remote.to_vec()),
        enrollment_id: None,
        enrollment_secret: None,
        grant_revision: 1,
        timings: LinkTimings::default(),
    }
}

struct Host {
    address: SocketAddr,
    links: mpsc::Receiver<LanLink>,
    acceptor: JoinHandle<()>,
    host: snow::Keypair,
    phone: snow::Keypair,
}

async fn host() -> Host {
    let host = keypair();
    let phone = keypair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (host_private, host_public, phone_public) = (
        host.private.clone(),
        host.public.clone(),
        phone.public.clone(),
    );
    let link_config: Arc<dyn Fn() -> LinkConfig + Send + Sync> = Arc::new(move || LinkConfig {
        purpose: LinkPurpose::Session,
        role: LinkRole::Host,
        local_private_key: Zeroizing::new(host_private.clone()),
        local_public_key: host_public.clone(),
        expected_remote_public_key: Some(phone_public.clone()),
        enrollment_id: None,
        enrollment_secret: None,
        grant_revision: 1,
        timings: LinkTimings::default(),
    });
    let (links_tx, links) = mpsc::channel(2);
    let acceptor = tokio::spawn(run_lan_acceptor(listener, link_config, links_tx));
    Host {
        address,
        links,
        acceptor,
        host,
        phone,
    }
}

impl Host {
    async fn phone_connects(&self, as_keys: &snow::Keypair) -> Result<Arc<SecureLink>, String> {
        let stream = TcpStream::connect(self.address).await.unwrap();
        SecureLink::establish(
            LanRecordIo::new(stream),
            config(LinkRole::Controller, as_keys, &self.host.public),
        )
        .await
        .map_err(|error| error.to_string())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_connection_does_not_delay_the_paired_phone() {
    let mut host = host().await;
    // Connected, never sends a byte.
    let _silent = TcpStream::connect(host.address).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    let phone = host.phone_connects(&host.phone).await.unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(3), host.links.recv())
        .await
        .expect("the phone authenticates while the silent connection is pending")
        .unwrap();
    // Before the fix the acceptor sat on the silent connection for the whole
    // 10 s authentication bound; now the phone never waits on it.
    assert!(
        started.elapsed() < LAN_AUTHENTICATION_LIMIT,
        "phone waited {:?}",
        started.elapsed()
    );
    assert!(!accepted.link.is_closed());
    phone.close().await;
    accepted.link.close().await;
    host.acceptor.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_connections_filling_every_permit_are_timed_out_and_the_phone_gets_in() {
    let mut host = host().await;
    let mut silent = Vec::new();
    for _ in 0..LAN_HANDSHAKE_PERMITS {
        silent.push(TcpStream::connect(host.address).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    let phone_config = config(LinkRole::Controller, &host.phone, &host.host.public);
    let connecting = tokio::spawn({
        let address = host.address;
        async move {
            let stream = TcpStream::connect(address).await.unwrap();
            SecureLink::establish(LanRecordIo::new(stream), phone_config).await
        }
    });
    let accepted = tokio::time::timeout(
        LAN_AUTHENTICATION_LIMIT + Duration::from_secs(2),
        host.links.recv(),
    )
    .await
    .expect("a timed-out permit is released to the waiting phone")
    .unwrap();
    // The phone had to wait for a permit, which only a handshake timeout frees.
    assert!(started.elapsed() >= LAN_AUTHENTICATION_LIMIT - Duration::from_millis(200));
    let phone = connecting.await.unwrap().unwrap();
    phone.close().await;
    accepted.link.close().await;
    host.acceptor.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpinned_peer_is_refused_and_the_paired_phone_still_connects() {
    let mut host = host().await;
    let stranger = keypair();
    assert!(host.phone_connects(&stranger).await.is_err());

    let phone = host.phone_connects(&host.phone).await.unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(3), host.links.recv())
        .await
        .unwrap()
        .unwrap();
    // Only the paired phone's link was forwarded.
    assert!(host.links.try_recv().is_err());
    phone.close().await;
    accepted.link.close().await;
    host.acceptor.abort();
}
