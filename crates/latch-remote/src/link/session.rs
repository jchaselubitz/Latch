//! The paired-session loop: one current authenticated link per pair, its
//! gateway streams, and the relay lease and admission handoffs from Desktop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use latch::cli::remote_access::{remote_link_identity, AuthenticatedGateway};
use latch::session::paths::LatchHome;
use latch_transport::link::{
    LinkConfig, LinkPurpose, LinkRole, LinkStageTimings, LinkTimings, RelayStatus, SecureLink,
    Service, WssControl,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use zeroize::Zeroizing;

use super::acceptors::{
    advertise_lan, run_lan_acceptor, run_wss_acceptor, Admission, EstablishedLink, LanLink,
};
use super::config::decode_key;
use super::ipc::{emit_json, emit_status, ipc_reader, shutdown_signal, HelperCommand, HostStatus};
use super::RemoteLinkHostConfig;

pub(super) async fn run_session(
    home: LatchHome,
    config: RemoteLinkHostConfig,
) -> anyhow::Result<()> {
    let mut ipc = ipc_reader("latch-remote-ipc")?;
    let identity = remote_link_identity(&home)?;
    let local_private = decode_key(&identity.private_key)?;
    let local_public = decode_key(&identity.public_key)?;
    let peer_public_key = config.peer_public_key.as_deref().expect("validated");
    let peer_public = decode_key(peer_public_key)?;
    let gateway_handle =
        AuthenticatedGateway::connect(&home, peer_public_key, config.grant_revision).await?;
    let listener = tokio::net::TcpListener::bind(&config.lan_bind)
        .await
        .context("cannot bind Remote Link LAN listener")?;
    let lan_address = listener.local_addr()?;
    let _bonjour = advertise_lan(&local_public, &peer_public, lan_address.port())?;
    emit_status(
        HostStatus::LanReady,
        serde_json::json!({ "port": lan_address.port() }),
    );

    let grant_revision = config.grant_revision;
    let link_config = Arc::new(move || LinkConfig {
        purpose: LinkPurpose::Session,
        role: LinkRole::Host,
        local_private_key: Zeroizing::new(local_private.clone()),
        local_public_key: local_public.clone(),
        expected_remote_public_key: Some(peer_public.clone()),
        enrollment_id: None,
        enrollment_secret: None,
        grant_revision,
        timings: LinkTimings::default(),
    });

    let (links_tx, mut links_rx) = mpsc::channel::<EstablishedLink>(2);
    let (admissions_tx, admissions_rx) = mpsc::channel::<Admission>(2);
    admissions_tx
        .send(Admission {
            relay_url: config.relay_url.clone(),
            admission: config.admission.clone(),
        })
        .await
        .expect("initial admission");

    // The LAN acceptor runs for the life of the process. A newly authenticated
    // LAN peer replaces whatever link is current, which is how a phone that
    // walks back onto the Wi-Fi re-establishes without waiting for the
    // relay side to notice.
    let (lan_tx, mut lan_rx) = mpsc::channel::<LanLink>(2);
    let lan_acceptor = tokio::spawn(run_lan_acceptor(listener, link_config.clone(), lan_tx));
    let lan_forwarder = tokio::spawn({
        let links_tx = links_tx.clone();
        async move {
            while let Some(lan) = lan_rx.recv().await {
                let established = EstablishedLink {
                    link: lan.link,
                    carrier: "lan",
                    control: None,
                    timings: LinkStageTimings {
                        connect_ms: 0,
                        peer_wait_ms: 0,
                        authenticate_ms: lan.authenticate_ms,
                    },
                };
                if links_tx.send(established).await.is_err() {
                    break;
                }
            }
        }
    });

    // The WSS acceptor consumes one admission per socket. It waits for the
    // relay to report the phone present before starting the handshake
    // deadline, so an idle Mac keeps one waiting socket rather than churning
    // helpers on a 10-second timer.
    let current_control: Arc<tokio::sync::Mutex<Option<Arc<WssControl>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let wss_acceptor = tokio::spawn({
        let link_config = link_config.clone();
        let links_tx = links_tx.clone();
        let current_control = current_control.clone();
        async move { run_wss_acceptor(admissions_rx, link_config, links_tx, current_control).await }
    });

    let mut current: Option<Arc<SecureLink>> = None;
    let mut streams = JoinSet::new();
    let mut current_lease: Option<String> = None;
    let result = loop {
        tokio::select! {
            established = links_rx.recv() => {
                let Some(established) = established else { break Err(anyhow!("link acceptors stopped")); };
                if let Some(previous) = current.take() {
                    // One authenticated controller per pair. The newest link
                    // wins and the old one is cancelled before it can mix
                    // bytes with the replacement.
                    previous.close().await;
                    streams.abort_all();
                    while streams.join_next().await.is_some() {}
                    emit_status(HostStatus::LinkClosed, serde_json::json!({ "reason": "replaced" }));
                }
                emit_status(HostStatus::Ready, serde_json::json!({
                    "carrier": established.carrier,
                    "connectMs": established.timings.connect_ms,
                    "peerWaitMs": established.timings.peer_wait_ms,
                    "authenticateMs": established.timings.authenticate_ms,
                }));
                if established.control.is_none() {
                    // A LAN link owns no relay lease. Keep the waiting relay
                    // socket where it is: it is the path back if Wi-Fi drops.
                }
                current = Some(established.link);
            }
            accepted = async {
                match current.as_ref() {
                    Some(link) => link.accept(LinkPurpose::Session).await,
                    None => std::future::pending().await,
                }
            } => {
                match accepted {
                    Ok((header, stream)) => {
                        if header.service != Service::Gateway || header.grant_revision != grant_revision {
                            // A stale or forbidden request closes only this link;
                            // the phone re-authenticates with the current grant.
                            if let Some(link) = current.take() { link.close().await; }
                            streams.abort_all();
                            emit_status(HostStatus::LinkClosed, serde_json::json!({ "reason": "stale_grant" }));
                            continue;
                        }
                        let gateway = gateway_handle.clone();
                        streams.spawn(async move { gateway.proxy(stream).await });
                    }
                    Err(_) => {
                        // The link ended: peer gone, dead-peer timeout, or
                        // protocol failure. Streams die with it; the gateway
                        // and its latchd sessions do not.
                        if let Some(link) = current.take() { link.close().await; }
                        streams.abort_all();
                        while streams.join_next().await.is_some() {}
                        emit_status(HostStatus::LinkClosed, serde_json::json!({ "reason": "peer_gone" }));
                    }
                }
            }
            Some(result) = streams.join_next(), if !streams.is_empty() => {
                // A single request can fail because the shared gateway is
                // restarting or because that request lost authority. Neither
                // condition is a link failure, so keep healthy device links
                // and their other streams alive.
                if let Err(error) = result {
                    if error.is_panic() {
                        break Err(anyhow!("Remote Link stream task panicked: {error}"));
                    }
                }
            }
            command = ipc.recv() => {
                let Some(command) = command else { break Err(anyhow!("Desktop IPC closed")); };
                let command: HelperCommand = serde_json::from_str(&command)
                    .context("invalid helper IPC message")?;
                match command {
                    HelperCommand::LeaseExtension { version, lease_id, claim } => {
                        if version != 1 || claim.is_empty() || claim.len() > 4096 {
                            break Err(anyhow!("invalid lease extension command"));
                        }
                        if current_lease.as_deref() != Some(lease_id.as_str()) {
                            // A stale extension for a socket that already went
                            // away is not an error; the next lease_started
                            // event tells Desktop which lease is live.
                            continue;
                        }
                        let control = current_control.lock().await.clone();
                        if let Some(control) = control {
                            let _ = control.extend_lease(&claim).await;
                        }
                    }
                    HelperCommand::Admission { version, relay_url, admission } => {
                        if version != 1 || !relay_url.starts_with("wss://") || admission.is_empty()
                            || admission.len() > 4096
                        {
                            break Err(anyhow!("invalid admission command"));
                        }
                        let _ = admissions_tx.send(Admission { relay_url, admission }).await;
                    }
                    HelperCommand::Shutdown { version } => {
                        if version != 1 { break Err(anyhow!("invalid shutdown command")); }
                        break Ok(());
                    }
                }
            }
            status = async {
                let control = current_control.lock().await.clone();
                match control {
                    Some(control) => control.next_status().await,
                    None => { tokio::time::sleep(Duration::from_millis(100)).await; None }
                }
            } => {
                match status {
                    Some(RelayStatus::LeaseStarted { lease_id, expires_at }) => {
                        current_lease = Some(lease_id.clone());
                        let _ = emit_json(&serde_json::json!({
                            "type": "lease_started", "version": 1,
                            "leaseId": lease_id, "expiresAt": expires_at,
                        }));
                    }
                    Some(RelayStatus::PeerReady) => emit_status(HostStatus::Authenticating, serde_json::json!({})),
                    Some(RelayStatus::PeerUnavailable) => emit_status(HostStatus::WaitingForPeer, serde_json::json!({})),
                    None => {}
                }
            }
            signal = shutdown_signal() => {
                signal?;
                break Ok(());
            }
        }
    };
    if let Some(link) = current.take() {
        link.close().await;
    }
    streams.abort_all();
    while streams.join_next().await.is_some() {}
    lan_acceptor.abort();
    lan_forwarder.abort();
    wss_acceptor.abort();
    let _ = lan_acceptor.await;
    let _ = lan_forwarder.await;
    let _ = wss_acceptor.await;
    result
}
