//! LAN and WSS carrier acceptors, and the Bonjour advertisement that lets the
//! paired phone find the LAN listener.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use latch_transport::link::{
    LanRecordIo, LinkConfig, LinkError, LinkStageTimings, SecureLink, WssControl, WssRecordIo,
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use super::ipc::{emit_status, request_admission, HostStatus};

/// A fresh relay admission for one WSS socket.
pub(super) struct Admission {
    pub(super) relay_url: String,
    pub(super) admission: String,
}

/// One authenticated link ready to serve, with the carrier it arrived on.
pub(super) struct EstablishedLink {
    pub(super) link: Arc<SecureLink>,
    pub(super) carrier: &'static str,
    pub(super) control: Option<Arc<WssControl>>,
    pub(super) timings: LinkStageTimings,
}

/// Bound on how long a LAN peer may take to authenticate once it connects.
/// A LAN Noise exchange takes milliseconds; the bound exists so a connection
/// that never speaks gives its handshake permit back quickly.
pub const LAN_AUTHENTICATION_LIMIT: Duration = Duration::from_secs(3);

/// LAN handshakes allowed in flight at once.
pub const LAN_HANDSHAKE_PERMITS: usize = 4;

/// A LAN peer that completed Noise authentication against the pinned key.
pub struct LanLink {
    /// The authenticated link, ready to accept streams.
    pub link: Arc<SecureLink>,
    /// Time from accepting the TCP connection to a completed handshake.
    pub authenticate_ms: u64,
}

/// Accepts LAN carriers and authenticates each one concurrently, forwarding
/// every authenticated link to `links`. Handshakes run as tasks bounded by
/// [`LAN_HANDSHAKE_PERMITS`], and each has [`LAN_AUTHENTICATION_LIMIT`] to
/// finish, so a connection that never speaks holds one permit for at most that
/// long and cannot keep the paired phone out. Returns when the listener fails
/// or `links` is closed; dropping the future cancels in-flight handshakes.
pub async fn run_lan_acceptor(
    listener: tokio::net::TcpListener,
    link_config: Arc<dyn Fn() -> LinkConfig + Send + Sync>,
    links: mpsc::Sender<LanLink>,
) {
    let permits = Arc::new(Semaphore::new(LAN_HANDSHAKE_PERMITS));
    let mut handshakes = JoinSet::new();
    loop {
        // Taking the permit before accepting leaves further connections in
        // the listen backlog until a handshake finishes or times out.
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        if links.is_closed() {
            break;
        }
        while handshakes.try_join_next().is_some() {}
        let config = link_config();
        let links = links.clone();
        handshakes.spawn(async move {
            let _permit = permit;
            let started = std::time::Instant::now();
            let established = tokio::time::timeout(
                LAN_AUTHENTICATION_LIMIT,
                SecureLink::establish(LanRecordIo::new(stream), config),
            )
            .await;
            match established {
                Ok(Ok(link)) => {
                    let authenticate_ms = started.elapsed().as_millis() as u64;
                    if let Err(unsent) = links
                        .send(LanLink {
                            link,
                            authenticate_ms,
                        })
                        .await
                    {
                        unsent.0.link.close().await;
                    }
                }
                // An unauthenticated LAN peer is refused and forgotten; the
                // listener keeps serving the paired phone.
                Ok(Err(error)) => log::debug!("LAN handshake refused: {error}"),
                Err(_) => log::debug!("LAN handshake timed out"),
            }
        });
    }
}

/// Consumes admissions one socket at a time. Every socket ends the same way:
/// a status line saying why, then `admission_needed`, then waiting for
/// Desktop to supply the next single-use ticket.
pub(super) async fn run_wss_acceptor(
    mut admissions: mpsc::Receiver<Admission>,
    link_config: Arc<dyn Fn() -> LinkConfig + Send + Sync>,
    links_tx: mpsc::Sender<EstablishedLink>,
    current_control: Arc<tokio::sync::Mutex<Option<Arc<WssControl>>>>,
) {
    while let Some(admission) = admissions.recv().await {
        emit_status(HostStatus::Connecting, serde_json::json!({}));
        let started = std::time::Instant::now();
        let connected =
            WssRecordIo::connect_with_control(&admission.relay_url, &admission.admission).await;
        let (mut records, control) = match connected {
            Ok(value) => value,
            Err(error) => {
                emit_status(
                    HostStatus::Offline,
                    serde_json::json!({ "reason": "connect_failed" }),
                );
                let _ = error;
                request_admission("connect_failed");
                continue;
            }
        };
        let connect_ms = started.elapsed().as_millis() as u64;
        let control = Arc::new(control);
        *current_control.lock().await = Some(control.clone());
        emit_status(HostStatus::WaitingForPeer, serde_json::json!({}));
        // No wait bound: an absent phone is normal and is waited out for as
        // long as the relay keeps talking. What is bounded is relay silence.
        // The relay closes the socket at lease expiry if Desktop stops
        // renewing, but a path that silently discards traffic can never
        // deliver that close, so the carrier's own inactivity bound is what
        // ends the wait and sends us back for a fresh admission.
        if let Err(error) = records.wait_for_peer(None).await {
            *current_control.lock().await = None;
            let reason = match error {
                LinkError::Timeout => "relay_silent",
                _ => "socket_closed",
            };
            emit_status(HostStatus::Offline, serde_json::json!({ "reason": reason }));
            request_admission(reason);
            continue;
        }
        let peer_wait_ms = started.elapsed().as_millis() as u64 - connect_ms;
        emit_status(HostStatus::Authenticating, serde_json::json!({}));
        match SecureLink::establish(records, link_config()).await {
            Ok(link) => {
                let timings = LinkStageTimings {
                    connect_ms,
                    peer_wait_ms,
                    authenticate_ms: link.authenticate_ms,
                };
                let closed = link.clone();
                if links_tx
                    .send(EstablishedLink {
                        link,
                        carrier: "relay",
                        control: Some(control),
                        timings,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                // Hold this socket's control handle until the link ends so
                // lease renewals reach the right socket, then ask for the
                // next admission.
                closed.closed().await;
                *current_control.lock().await = None;
                request_admission("link_closed");
            }
            Err(_) => {
                *current_control.lock().await = None;
                emit_status(
                    HostStatus::Offline,
                    serde_json::json!({ "reason": "authentication_failed" }),
                );
                request_admission("authentication_failed");
            }
        }
    }
}

pub(super) fn advertise_lan(
    local_public: &[u8],
    peer_public: &[u8],
    port: u16,
) -> anyhow::Result<ServiceDaemon> {
    let identity = local_public
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    let peer = peer_public
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    let instance = format!("latch-{}-{}", &identity[..12], &peer[..12]);
    let service = ServiceInfo::new(
        "_latch-remote._tcp.local.",
        &instance,
        &format!("{instance}.local."),
        (),
        port,
        HashMap::from([
            ("identityKey".to_owned(), identity),
            ("linkVersion".to_owned(), "1".to_owned()),
            ("lanHost".to_owned(), format!("{instance}.local")),
            ("lanPort".to_owned(), port.to_string()),
            // Concrete addresses of the interfaces a phone can share, IPv4
            // first. Resolving the `.local` name on the phone took longer
            // than the LAN connect bound in the field, and the name also
            // resolves to loopback and tunnel addresses the phone cannot use.
            ("lanAddrs".to_owned(), lan_addresses().join(",")),
        ]),
    )?
    .enable_addr_auto();
    let daemon = ServiceDaemon::new()?;
    daemon.register(service)?;
    Ok(daemon)
}

/// Addresses of interfaces a phone on the same network can reach: `en*`
/// only (no loopback, link-local, tunnel, peer-to-peer, or VM bridge
/// interfaces). IPv4 first so the common case connects on the first try.
fn lan_addresses() -> Vec<String> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            let name = interface.name.as_str();
            // Only Ethernet/Wi-Fi style interfaces (`en*`) are candidates: VM and
            // container bridges (`bridge*`, `vmenet*`, `vnic*`) and tunnels
            // publish addresses a phone on the Wi-Fi cannot reach, and every
            // unreachable address costs the phone a connect bound on each
            // reconnect (seen in the field: two bridge addresses added two
            // seconds to every helper-restart recovery).
            if interface.is_loopback() || !name.starts_with("en") {
                continue;
            }
            match interface.ip() {
                std::net::IpAddr::V4(ip) => {
                    if !ip.is_link_local() && !ip.is_unspecified() {
                        v4.push(ip.to_string());
                    }
                }
                std::net::IpAddr::V6(ip) => {
                    let link_local = (ip.segments()[0] & 0xffc0) == 0xfe80;
                    if !link_local && !ip.is_unspecified() {
                        v6.push(ip.to_string());
                    }
                }
            }
        }
    }
    v4.extend(v6);
    v4.truncate(8);
    v4
}
