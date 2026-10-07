use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use super::handshake::NOISE_PATTERN;
use super::*;

struct MemoryRecords {
    send: mpsc::Sender<Vec<u8>>,
    receive: mpsc::Receiver<Vec<u8>>,
}

#[async_trait]
impl RecordIo for MemoryRecords {
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
        self.send.send(record).await.map_err(|_| LinkError::Closed)
    }

    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        Ok(self.receive.recv().await)
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        Ok(())
    }
}

fn pair() -> (MemoryRecords, MemoryRecords) {
    let (a_send, a_receive) = mpsc::channel(8);
    let (b_send, b_receive) = mpsc::channel(8);
    (
        MemoryRecords {
            send: a_send,
            receive: b_receive,
        },
        MemoryRecords {
            send: b_send,
            receive: a_receive,
        },
    )
}

fn keys() -> (snow::Keypair, snow::Keypair) {
    let params = NOISE_PATTERN.parse().unwrap();
    let builder = snow::Builder::new(params);
    (
        builder.generate_keypair().unwrap(),
        builder.generate_keypair().unwrap(),
    )
}

fn config(role: LinkRole, own: &[u8], own_public: &[u8], peer: &[u8]) -> LinkConfig {
    LinkConfig {
        purpose: LinkPurpose::Session,
        role,
        local_private_key: Zeroizing::new(own.to_vec()),
        local_public_key: own_public.to_vec(),
        expected_remote_public_key: Some(peer.to_vec()),
        enrollment_id: None,
        enrollment_secret: None,
        grant_revision: 3,
        timings: LinkTimings::default(),
    }
}

#[tokio::test]
async fn authenticates_pins_and_multiplexes_gateway_stream() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let controller = SecureLink::establish(
        controller_io,
        config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &host_keys.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, host) = tokio::try_join!(controller, host).unwrap();
    let opened = controller.open(Service::Gateway, 3);
    let accepted = host.accept(LinkPurpose::Session);
    let (mut opened, (header, mut accepted)) = tokio::try_join!(opened, accepted).unwrap();
    assert_eq!(header.service, Service::Gateway);
    opened.write_all(b"ping").await.unwrap();
    opened.flush().await.unwrap();
    let mut value = [0; 4];
    accepted.read_exact(&mut value).await.unwrap();
    assert_eq!(&value, b"ping");
    controller.close().await;
    host.close().await;
}

#[tokio::test]
async fn a_blocked_stream_does_not_stall_an_unrelated_stream() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let controller = SecureLink::establish(
        controller_io,
        config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &host_keys.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, host) = tokio::try_join!(controller, host).unwrap();

    let first = controller.open(Service::Gateway, 3);
    let first_peer = host.accept(LinkPurpose::Session);
    let (mut first, (_header, _blocked_peer)) = tokio::try_join!(first, first_peer).unwrap();
    let blocked = tokio::spawn(async move {
        first
            .write_all(&vec![0x5a; MAX_RECEIVE_WINDOW_BYTES * 2])
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second = controller.open(Service::Gateway, 3);
    let second_peer = host.accept(LinkPurpose::Session);
    let (mut second, (_header, mut second_peer)) =
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::try_join!(second, second_peer)
        })
        .await
        .expect("a blocked writer stalled stream admission")
        .unwrap();
    second.write_all(b"ping").await.unwrap();
    second.flush().await.unwrap();
    let mut value = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(2), second_peer.read_exact(&mut value))
        .await
        .expect("a blocked writer stalled another stream")
        .unwrap();
    assert_eq!(&value, b"ping");

    blocked.abort();
    let _ = blocked.await;
    controller.close().await;
    host.close().await;
}

#[tokio::test]
async fn wrong_pin_fails_closed() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let (_, wrong) = keys();
    let controller = SecureLink::establish(
        controller_io,
        config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &wrong.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, _) = tokio::join!(controller, host);
    assert!(matches!(controller, Err(LinkError::Authentication(_))));
}

#[tokio::test]
async fn admission_secret_alone_cannot_complete_enrollment() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let make = |role, own: &[u8], secret: u8| LinkConfig {
        purpose: LinkPurpose::Enrollment,
        role,
        local_private_key: Zeroizing::new(own.to_vec()),
        local_public_key: if role == LinkRole::Controller {
            controller_keys.public.clone()
        } else {
            host_keys.public.clone()
        },
        expected_remote_public_key: if role == LinkRole::Controller {
            Some(host_keys.public.clone())
        } else {
            None
        },
        enrollment_id: Some("enr_fixture_00000001".into()),
        enrollment_secret: Some(Zeroizing::new(vec![secret; 32])),
        grant_revision: 0,
        timings: LinkTimings::default(),
    };
    let controller = SecureLink::establish(
        controller_io,
        make(LinkRole::Controller, &controller_keys.private, 1),
    );
    let host = SecureLink::establish(host_io, make(LinkRole::Host, &host_keys.private, 2));
    let (controller, host) = tokio::join!(controller, host);
    assert!(controller.is_err());
    assert!(host.is_err());
}

#[tokio::test]
async fn enrollment_comparison_matches_and_commits_to_the_proposed_grant() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let make = |role, own: &[u8]| LinkConfig {
        purpose: LinkPurpose::Enrollment,
        role,
        local_private_key: Zeroizing::new(own.to_vec()),
        local_public_key: if role == LinkRole::Controller {
            controller_keys.public.clone()
        } else {
            host_keys.public.clone()
        },
        expected_remote_public_key: if role == LinkRole::Controller {
            Some(host_keys.public.clone())
        } else {
            None
        },
        enrollment_id: Some("enr_fixture_00000001".into()),
        enrollment_secret: Some(Zeroizing::new(vec![7; 32])),
        grant_revision: 0,
        timings: LinkTimings::default(),
    };
    let controller = SecureLink::establish(
        controller_io,
        make(LinkRole::Controller, &controller_keys.private),
    );
    let host = SecureLink::establish(host_io, make(LinkRole::Host, &host_keys.private));
    let (controller, host) = tokio::try_join!(controller, host).unwrap();
    let controller_code = controller
        .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "control")
        .unwrap();
    let host_code = host
        .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "control")
        .unwrap();
    assert_eq!(controller_code, host_code);
    assert_ne!(
        controller_code,
        controller
            .enrollment_comparison("enr_fixture_00000001", &controller_keys.public, "observe",)
            .unwrap()
    );
    controller.close().await;
    host.close().await;
}

/// A carrier that can silently drop everything it is asked to send, the
/// way a NAT that forgot a mapping does.
struct MutableRecords {
    inner: MemoryRecords,
    muted: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl RecordIo for MutableRecords {
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError> {
        if self.muted.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        self.inner.send_record(record).await
    }

    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        self.inner.recv_record().await
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        Ok(())
    }
}

fn fast_config(role: LinkRole, own: &[u8], own_public: &[u8], peer: &[u8]) -> LinkConfig {
    let mut config = config(role, own, own_public, peer);
    config.timings = LinkTimings {
        keepalive_interval: Duration::from_millis(60),
        dead_peer_timeout: Duration::from_millis(250),
    };
    config
}

#[tokio::test]
async fn keepalives_keep_an_idle_link_alive_and_silence_kills_it() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let muted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let controller_io = MutableRecords {
        inner: controller_io,
        muted: muted.clone(),
    };
    let controller = SecureLink::establish(
        controller_io,
        fast_config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &host_keys.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        fast_config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, host) = tokio::try_join!(controller, host).unwrap();

    // Well past the dead-peer bound with no application traffic: the
    // keepalive records are what keep both ends alive.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!host.is_closed());
    assert!(!controller.is_closed());
    let opened = controller.open(Service::Gateway, 3);
    let accepted = host.accept(LinkPurpose::Session);
    let (mut opened, (_, mut accepted)) = tokio::try_join!(opened, accepted).unwrap();
    opened.write_all(b"alive").await.unwrap();
    opened.flush().await.unwrap();
    let mut value = [0; 5];
    accepted.read_exact(&mut value).await.unwrap();
    assert_eq!(&value, b"alive");

    // Now the controller's records vanish silently. The host must notice
    // within its dead-peer bound and every accept/read fails closed.
    muted.store(true, std::sync::atomic::Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(2), host.closed())
        .await
        .expect("host did not detect the silent peer");
    assert!(host.is_closed());
    let mut rest = Vec::new();
    assert!(accepted.read_to_end(&mut rest).await.is_err() || rest.is_empty());
    assert!(host.accept(LinkPurpose::Session).await.is_err());
    controller.close().await;
}

#[tokio::test]
async fn final_bytes_written_before_close_arrive_before_eof() {
    // The enrollment helper writes its encrypted receipt, shuts the
    // stream, and closes the link in the same breath; the phone must read
    // the receipt and then EOF. In the field it read only EOF, because
    // the host aborted its transport task before the frames were sent
    // and the controller dropped its connection the instant the carrier
    // ended. Both directions are exercised.
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let controller = SecureLink::establish(
        controller_io,
        config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &host_keys.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, host) = tokio::try_join!(controller, host).unwrap();
    let opened = controller.open(Service::Gateway, 3);
    let accepted = host.accept(LinkPurpose::Session);
    let (mut opened, (_, mut accepted)) = tokio::try_join!(opened, accepted).unwrap();

    let payload: Vec<u8> = (0..40_000_u32).map(|value| (value % 251) as u8).collect();
    accepted.write_all(&payload).await.unwrap();
    accepted.shutdown().await.unwrap();
    host.close().await;

    let mut received = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), opened.read_to_end(&mut received))
        .await
        .expect("controller read did not finish");
    assert_eq!(
        received.len(),
        payload.len(),
        "final bytes were lost at close"
    );
    assert_eq!(received, payload);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), controller.closed())
            .await
            .is_ok()
    );
    controller.close().await;
}

#[tokio::test]
async fn closed_resolves_after_a_local_close() {
    let (controller_io, host_io) = pair();
    let (controller_keys, host_keys) = keys();
    let controller = SecureLink::establish(
        controller_io,
        config(
            LinkRole::Controller,
            &controller_keys.private,
            &controller_keys.public,
            &host_keys.public,
        ),
    );
    let host = SecureLink::establish(
        host_io,
        config(
            LinkRole::Host,
            &host_keys.private,
            &host_keys.public,
            &controller_keys.public,
        ),
    );
    let (controller, host) = tokio::try_join!(controller, host).unwrap();
    assert!(!controller.is_closed());
    controller.close().await;
    tokio::time::timeout(Duration::from_secs(2), controller.closed())
        .await
        .unwrap();
    assert!(controller.is_closed());
    // The peer learns through the carrier ending, not through any hint.
    tokio::time::timeout(Duration::from_secs(2), host.closed())
        .await
        .expect("peer closure was not observed");
    host.close().await;
}
