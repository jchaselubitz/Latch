//! End-to-end check that an approved rendezvous offer becomes a peer stream.
//!
//! This exercises the seam the helper actually depends on: an offer in the
//! control plane's published shape goes in, a real ICE agent answers it, the
//! `latch-noise-v1` data channel opens, and what comes out is a `PeerStream`
//! the `latch` crate's Noise proxy can drive. The in-memory network keeps the
//! run independent of the machine having a routable interface — loopback is
//! excluded from real gathering on purpose.

use std::sync::Arc;
use std::time::Duration;

use latch::cli::remote_access::{IceCandidateRecord, PeerRoute, PeerTransport, RemoteOffer};
use latch_transport::rtc::{
    IceCredentials, RemoteDescription, Role, RtcEndpoint, TestNetwork, TransportCandidate,
};

use latch_remote::ice::IceResponder;

#[tokio::test]
async fn an_approved_offer_yields_a_peer_stream_carrying_noise_records() {
    let network = Arc::new(TestNetwork::new(Some(Default::default())));
    let mac = IceCredentials {
        ufrag: "macufrag".into(),
        password: "mac-password-with-more-than-128-bits".into(),
    };
    let phone = IceCredentials {
        ufrag: "phoneufr".into(),
        password: "phone-password-with-more-than-128-bits".into(),
    };

    let responder = IceResponder::for_test(mac.clone(), Arc::clone(&network));
    let published = responder.start().await.expect("the agent gathers");
    assert_eq!(published.ufrag, mac.ufrag);
    assert!(!published.candidates.is_empty());

    let (initiator, phone_description) =
        RtcEndpoint::gather_on_test_network(phone.clone(), &[], network)
            .await
            .expect("the phone gathers");

    // The offer is built in exactly the shape the desktop app records after it
    // has re-checked the peer against the local device store.
    let offer = RemoteOffer {
        request_id: "a".repeat(32),
        peer_device_id: "b".repeat(32),
        ice_ufrag: phone.ufrag.clone(),
        ice_pwd: phone.password.clone(),
        candidates: phone_description
            .candidates
            .iter()
            .map(|candidate| record(candidate, expires_at()))
            .collect(),
        expires_at: expires_at(),
    };

    let responder = Arc::new(responder);
    let accepting = {
        let responder = Arc::clone(&responder);
        tokio::spawn(async move { responder.accept().await })
    };
    responder.offer(offer).await.expect("the offer is accepted");

    let mac_candidates: Vec<_> = published
        .candidates
        .iter()
        .map(transport_candidate)
        .collect();
    let dialing = tokio::spawn(async move {
        initiator
            .connect(
                RemoteDescription {
                    credentials: mac,
                    candidates: mac_candidates,
                },
                Role::Initiator,
            )
            .await
    });

    let phone_channel = tokio::time::timeout(Duration::from_secs(20), dialing)
        .await
        .expect("the phone connects before the deadline")
        .expect("the dialing task runs")
        .expect("the phone opens the data channel");
    let mut peer = tokio::time::timeout(Duration::from_secs(20), accepting)
        .await
        .expect("the helper accepts before the deadline")
        .expect("the accepting task runs")
        .expect("an offer produced a peer stream");

    // The route travels with the stream so the proxy can record which path
    // served this connection. Both ends are host candidates on the in-memory
    // network, so the only correct answer here is a direct host pair; an
    // `Unknown` would mean the observation was lost between the nominated pair
    // and the audit trail.
    assert_eq!(peer.route, PeerRoute::DirectHost);

    // The proxy above this seam only ever moves opaque Noise records, so that
    // is what the round trip asserts — in both directions, since the terminal
    // depends on the response half as much as the request half.
    phone_channel
        .write(b"noise handshake record")
        .await
        .expect("the phone writes");
    assert_eq!(
        peer.reader.read_record().await.expect("the helper reads"),
        b"noise handshake record"
    );
    peer.writer
        .write_record(b"noise response record")
        .await
        .expect("the helper writes");
    assert_eq!(
        phone_channel.read().await.expect("the phone reads"),
        b"noise response record"
    );

    // Answering an offer consumes the agent, so a replacement is gathered in
    // the background. Presence has to reach that replacement, on the same
    // credentials, or the next rendezvous advertises dead ports.
    let refreshed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(description) = responder.local_description().await {
                if description.candidates != published.candidates {
                    return description;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the agent re-gathers");
    assert_eq!(refreshed.ufrag, published.ufrag);
    assert_eq!(refreshed.password, published.password);
    assert!(!refreshed.candidates.is_empty());

    let _ = phone_channel.close().await;
}

fn expires_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 60
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

#[tokio::test]
async fn an_idle_agent_is_gathered_again_before_its_addresses_go_stale() {
    let network = Arc::new(TestNetwork::new(Some(Default::default())));
    let mac = IceCredentials {
        ufrag: "macufrag".into(),
        password: "mac-password-with-more-than-128-bits".into(),
    };
    let responder = IceResponder::for_test_regathering_after(
        mac.clone(),
        Arc::clone(&network),
        Duration::from_millis(200),
    );
    let published = responder.start().await.expect("the agent gathers");

    // No offer arrives. The agent is still replaced, on the same credentials,
    // so presence keeps describing ports something is listening on.
    let refreshed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(description) = responder.local_description().await {
                if description.candidates != published.candidates {
                    return description;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the idle agent re-gathers on its own");
    assert_eq!(refreshed.ufrag, mac.ufrag);
    assert_eq!(refreshed.password, mac.password);
    assert!(!refreshed.candidates.is_empty());
}
