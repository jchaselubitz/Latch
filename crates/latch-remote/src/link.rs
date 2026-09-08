//! Host ownership for Remote Link v1.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use latch::cli::remote_access::{
    authorize_enrollment, remote_link_identity, AuthenticatedGatewayOwner, DevicePermission,
};
use latch::session::paths::LatchHome;
use latch_transport::link::{
    LanRecordIo, LinkConfig, LinkPurpose, LinkRole, LinkStageTimings, LinkTimings, RelayStatus,
    SecureLink, Service, WssControl, WssRecordIo,
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use zeroize::Zeroizing;

/// Versioned host admission read from an inherited stdin pipe. The bearer is
/// never accepted in argv, an environment variable, or a file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteLinkHostConfig {
    version: u8,
    purpose: LinkPurpose,
    relay_url: String,
    admission: String,
    #[serde(default)]
    peer_public_key: Option<String>,
    #[serde(default)]
    grant_revision: u64,
    #[serde(default)]
    enrollment_id: Option<String>,
    #[serde(default)]
    enrollment_secret: Option<String>,
    #[serde(default = "default_lan_bind")]
    lan_bind: String,
}

fn default_lan_bind() -> String {
    "0.0.0.0:0".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentProposal {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    name: String,
    permission: DevicePermission,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentDecision {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    permission: DevicePermission,
    approved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentMirrored {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    permission: DevicePermission,
    grant_revision: u64,
}

impl RemoteLinkHostConfig {
    fn validate(&self) -> anyhow::Result<()> {
        if self.version != 1 {
            bail!("unsupported Remote Link version");
        }
        if !self.relay_url.starts_with("wss://") || self.relay_url.len() > 2048 {
            bail!("Remote Link relay URL must use wss");
        }
        if self.admission.is_empty()
            || self.admission.len() > 4096
            || self
                .admission
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            bail!("Remote Link admission is malformed");
        }
        match self.purpose {
            LinkPurpose::Session => {
                decode_key(self.peer_public_key.as_deref().unwrap_or_default())
                    .context("invalid pinned controller key")?;
                if self.grant_revision == 0 {
                    bail!("Remote Link grant revision must be positive");
                }
                let bind: SocketAddr = self
                    .lan_bind
                    .parse()
                    .context("invalid Remote Link LAN bind")?;
                if !bind.ip().is_unspecified() || bind.port() != 0 {
                    bail!("Remote Link LAN bind must select an ephemeral port on all interfaces");
                }
            }
            LinkPurpose::Enrollment => {
                let id = self.enrollment_id.as_deref().unwrap_or_default();
                if id.is_empty() || id.len() > 64 {
                    bail!("invalid enrollment id");
                }
                decode_key(self.enrollment_secret.as_deref().unwrap_or_default())
                    .context("invalid QR-only enrollment secret")?;
                if self.peer_public_key.is_some() || self.grant_revision != 0 {
                    bail!("enrollment cannot pre-authorize a controller or grant");
                }
            }
        }
        Ok(())
    }
}

/// Desktop-to-helper IPC lines. The initial stdin line is the
/// [`RemoteLinkHostConfig`]; every later line is one of these.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum HelperCommand {
    /// A control-plane-signed extension for the current relay lease.
    #[serde(rename_all = "camelCase")]
    LeaseExtension {
        version: u8,
        lease_id: String,
        claim: String,
    },
    /// A fresh single-use relay admission after the previous socket closed.
    #[serde(rename_all = "camelCase")]
    Admission {
        version: u8,
        relay_url: String,
        admission: String,
    },
    /// Stop serving and exit cleanly.
    Shutdown { version: u8 },
}

/// A fresh relay admission for one WSS socket.
struct Admission {
    relay_url: String,
    admission: String,
}

/// Helper-owned link lifecycle statuses printed as content-free JSON lines.
#[derive(Clone, Copy)]
enum HostStatus {
    LanReady,
    Connecting,
    WaitingForPeer,
    Authenticating,
    Ready,
    LinkClosed,
    Offline,
}

impl HostStatus {
    fn name(self) -> &'static str {
        match self {
            Self::LanReady => "lan_ready",
            Self::Connecting => "connecting",
            Self::WaitingForPeer => "waiting_for_peer",
            Self::Authenticating => "authenticating",
            Self::Ready => "ready",
            Self::LinkClosed => "link_closed",
            Self::Offline => "offline",
        }
    }
}

/// The home the helper serves, so status transitions can also land in the
/// Mac's content-free audit trail for field evidence.
static AUDIT_HOME: std::sync::OnceLock<LatchHome> = std::sync::OnceLock::new();

fn emit_status(status: HostStatus, extra: serde_json::Value) {
    let mut value = serde_json::json!({ "type": "status", "version": 1, "status": status.name() });
    if let (Some(object), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in more {
            object.insert(key.clone(), item.clone());
        }
    }
    let _ = emit_json(&value);
    if let Some(home) = AUDIT_HOME.get() {
        let detail = extra
            .get("reason")
            .or_else(|| extra.get("carrier"))
            .and_then(|item| item.as_str())
            .unwrap_or("ok");
        let _ = latch::cli::remote_access::record_link_status(home, status.name(), detail);
    }
}

/// One authenticated link ready to serve, with the carrier it arrived on.
struct EstablishedLink {
    link: Arc<SecureLink>,
    carrier: &'static str,
    control: Option<Arc<WssControl>>,
    timings: LinkStageTimings,
}

/// Runs one host pair and owns every task from WSS/LAN through the fixed
/// loopback gateway. The gateway child lives for the whole process; links
/// come and go underneath it. Returning always closes the link, child, and
/// stream tasks.
pub fn serve_remote_link(
    home: LatchHome,
    latch_bin: PathBuf,
    config: RemoteLinkHostConfig,
) -> anyhow::Result<()> {
    config.validate()?;
    let _ = AUDIT_HOME.set(home.clone());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match config.purpose {
        LinkPurpose::Session => runtime.block_on(run_session(home, latch_bin, config)),
        LinkPurpose::Enrollment => runtime.block_on(run_enrollment(home, config)),
    }
}

/// Bound on how long a LAN peer may take to authenticate once it connects.
const LAN_AUTHENTICATION_LIMIT: Duration = Duration::from_secs(10);

async fn run_session(
    home: LatchHome,
    latch_bin: PathBuf,
    config: RemoteLinkHostConfig,
) -> anyhow::Result<()> {
    let mut ipc = ipc_reader("latch-remote-ipc")?;
    let identity = remote_link_identity(&home)?;
    let local_private = decode_key(&identity.private_key)?;
    let local_public = decode_key(&identity.public_key)?;
    let peer_public_key = config.peer_public_key.as_deref().expect("validated");
    let peer_public = decode_key(peer_public_key)?;
    let mut gateway =
        AuthenticatedGatewayOwner::start(&home, &latch_bin, peer_public_key, config.grant_revision)
            .await?;
    let gateway_handle = gateway.gateway();
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
    let lan_acceptor = tokio::spawn({
        let link_config = link_config.clone();
        let links_tx = links_tx.clone();
        async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let started = std::time::Instant::now();
                let established = tokio::time::timeout(
                    LAN_AUTHENTICATION_LIMIT,
                    SecureLink::establish(LanRecordIo::new(stream), link_config()),
                )
                .await;
                match established {
                    Ok(Ok(link)) => {
                        let timings = LinkStageTimings {
                            connect_ms: 0,
                            peer_wait_ms: 0,
                            authenticate_ms: started.elapsed().as_millis() as u64,
                        };
                        if links_tx
                            .send(EstablishedLink {
                                link,
                                carrier: "lan",
                                control: None,
                                timings,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    // An unauthenticated LAN peer is refused and forgotten; the
                    // listener keeps serving the paired phone.
                    Ok(Err(_)) | Err(_) => continue,
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
            result = gateway.wait() => break result,
            Some(result) = streams.join_next(), if !streams.is_empty() => {
                if let Err(error) = result {
                    if !error.is_cancelled() {
                        break Err(anyhow!("Remote Link stream task failed: {error}"));
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
    wss_acceptor.abort();
    let _ = lan_acceptor.await;
    let _ = wss_acceptor.await;
    gateway.close().await;
    result
}

/// Consumes admissions one socket at a time. Every socket ends the same way:
/// a status line saying why, then `admission_needed`, then waiting for
/// Desktop to supply the next single-use ticket.
async fn run_wss_acceptor(
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
        // Unbounded: the relay closes the socket at lease expiry if Desktop
        // stops renewing, and that closure is what ends this wait.
        if records.wait_for_peer(None).await.is_err() {
            *current_control.lock().await = None;
            emit_status(
                HostStatus::Offline,
                serde_json::json!({ "reason": "socket_closed" }),
            );
            request_admission("socket_closed");
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

fn request_admission(reason: &str) {
    let _ = emit_json(&serde_json::json!({
        "type": "admission_needed", "version": 1, "reason": reason,
    }));
}

async fn run_enrollment(home: LatchHome, config: RemoteLinkHostConfig) -> anyhow::Result<()> {
    let mut ipc = ipc_reader("latch-remote-enrollment-ipc")?;
    let identity = remote_link_identity(&home)?;
    let local_private = decode_key(&identity.private_key)?;
    let local_public = decode_key(&identity.public_key)?;
    let enrollment_id = config.enrollment_id.as_deref().expect("validated");
    let enrollment_secret = decode_key(config.enrollment_secret.as_deref().expect("validated"))?;
    let (mut records, _control) =
        WssRecordIo::connect_with_control(&config.relay_url, &config.admission)
            .await
            .context("cannot connect enrollment relay")?;
    emit_status(HostStatus::WaitingForPeer, serde_json::json!({}));
    // The owner is holding a QR code up to a phone; the provisional room lives
    // five minutes and so does this wait.
    records
        .wait_for_peer(Some(Duration::from_secs(5 * 60)))
        .await
        .context("no phone joined the enrollment room")?;
    emit_status(HostStatus::Authenticating, serde_json::json!({}));
    let link = SecureLink::establish(
        records,
        LinkConfig {
            purpose: LinkPurpose::Enrollment,
            role: LinkRole::Host,
            local_private_key: Zeroizing::new(local_private),
            local_public_key: local_public.clone(),
            expected_remote_public_key: None,
            enrollment_id: Some(enrollment_id.to_owned()),
            enrollment_secret: Some(Zeroizing::new(enrollment_secret)),
            grant_revision: 0,
            timings: LinkTimings::default(),
        },
    )
    .await
    .context("enrollment authentication failed")?;
    let result = async {
        let (header, mut stream) = link.accept(LinkPurpose::Enrollment).await?;
        if header.service != Service::Enrollment || header.grant_revision != 0 {
            bail!("controller requested a forbidden enrollment service");
        }
        let proposal: EnrollmentProposal = read_json_line(&mut stream).await?;
        validate_proposal(&proposal, enrollment_id, &link.remote_public_key)?;
        let controller_key = decode_key(&proposal.controller_public_key)?;
        let permission = permission_name(proposal.permission);
        let comparison = link.enrollment_comparison(enrollment_id, &controller_key, permission)?;
        emit_json(&serde_json::json!({
            "type": "enrollment_pending",
            "version": 1,
            "enrollmentId": enrollment_id,
            "provisionalDeviceId": proposal.provisional_device_id,
            "controllerPublicKey": proposal.controller_public_key,
            "name": proposal.name,
            "permission": proposal.permission,
            "comparison": comparison,
        }))?;

        let command = ipc
            .recv()
            .await
            .ok_or_else(|| anyhow!("Desktop IPC closed before enrollment decision"))?;
        let decision: EnrollmentDecision =
            serde_json::from_str(&command).context("invalid enrollment decision")?;
        validate_decision(&decision, &proposal)?;
        if !decision.approved {
            emit_json(&serde_json::json!({
                "type": "enrollment_rejected", "version": 1,
                "enrollmentId": enrollment_id,
            }))?;
            return Ok(());
        }

        let committed = authorize_enrollment(
            &home,
            enrollment_id,
            &proposal.controller_public_key,
            &proposal.name,
            proposal.permission,
            &proposal.provisional_device_id,
        )?;
        emit_json(&serde_json::json!({
            "type": "enrollment_committed", "version": 1,
            "enrollmentId": enrollment_id,
            "provisionalDeviceId": proposal.provisional_device_id,
            "controllerPublicKey": proposal.controller_public_key,
            "permission": proposal.permission,
            "grantRevision": committed.grant_revision,
        }))?;

        let command = ipc
            .recv()
            .await
            .ok_or_else(|| anyhow!("Desktop IPC closed before enrollment mirror confirmation"))?;
        let mirrored: EnrollmentMirrored =
            serde_json::from_str(&command).context("invalid enrollment mirror confirmation")?;
        validate_mirrored(&mirrored, &proposal, committed.grant_revision)?;
        write_json_line(
            &mut stream,
            &serde_json::json!({
                "type": "pairing_approved",
                "version": 1,
                "enrollmentId": enrollment_id,
                "hostPublicKey": identity.public_key,
                "controllerPublicKey": proposal.controller_public_key,
                "permission": proposal.permission,
                "grantRevision": committed.grant_revision,
            }),
        )
        .await?;
        stream.shutdown().await?;
        emit_json(&serde_json::json!({
            "type": "enrollment_complete", "version": 1,
            "enrollmentId": enrollment_id,
        }))?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    link.close().await;
    result
}

fn ipc_reader(name: &str) -> anyhow::Result<mpsc::Receiver<String>> {
    let (send, receive) = mpsc::channel::<String>(4);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) if !line.is_empty() => {
                        if send.blocking_send(line).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
        .context("cannot start helper enrollment IPC reader")?;
    Ok(receive)
}

fn validate_proposal(
    proposal: &EnrollmentProposal,
    enrollment_id: &str,
    authenticated_key: &[u8],
) -> anyhow::Result<()> {
    if proposal.r#type != "enrollment_proposal"
        || proposal.version != 1
        || proposal.enrollment_id != enrollment_id
        || proposal.provisional_device_id.is_empty()
        || proposal.provisional_device_id.len() > 96
        || proposal.name.trim().is_empty()
        || proposal.name.len() > 80
        || decode_key(&proposal.controller_public_key)? != authenticated_key
    {
        bail!("enrollment proposal does not match the authenticated controller");
    }
    Ok(())
}

fn validate_decision(
    decision: &EnrollmentDecision,
    proposal: &EnrollmentProposal,
) -> anyhow::Result<()> {
    if decision.r#type != "enrollment_decision"
        || decision.version != 1
        || decision.enrollment_id != proposal.enrollment_id
        || decision.provisional_device_id != proposal.provisional_device_id
        || decision.controller_public_key != proposal.controller_public_key
        || decision.permission != proposal.permission
    {
        bail!("enrollment decision does not match the pending proposal");
    }
    Ok(())
}

fn validate_mirrored(
    mirrored: &EnrollmentMirrored,
    proposal: &EnrollmentProposal,
    grant_revision: u64,
) -> anyhow::Result<()> {
    if mirrored.r#type != "enrollment_mirrored"
        || mirrored.version != 1
        || mirrored.enrollment_id != proposal.enrollment_id
        || mirrored.provisional_device_id != proposal.provisional_device_id
        || mirrored.controller_public_key != proposal.controller_public_key
        || mirrored.permission != proposal.permission
        || mirrored.grant_revision != grant_revision
    {
        bail!("enrollment mirror does not match the committed proposal");
    }
    Ok(())
}

fn permission_name(permission: DevicePermission) -> &'static str {
    match permission {
        DevicePermission::Observe => "observe",
        DevicePermission::Interact => "interact",
        DevicePermission::Control => "control",
    }
}

async fn read_json_line<T: DeserializeOwned>(
    stream: &mut latch_transport::link::LogicalStream,
) -> anyhow::Result<T> {
    const MAX_MESSAGE_BYTES: usize = 16 * 1024;
    let mut message = Vec::new();
    loop {
        let byte = stream
            .read_u8()
            .await
            .context("cannot read enrollment message")?;
        if byte == b'\n' {
            break;
        }
        if message.len() == MAX_MESSAGE_BYTES {
            bail!("enrollment message exceeds limit");
        }
        message.push(byte);
    }
    if message.is_empty() {
        bail!("empty enrollment message");
    }
    serde_json::from_slice(&message).context("invalid enrollment message")
}

async fn write_json_line(
    stream: &mut latch_transport::link::LogicalStream,
    value: &impl Serialize,
) -> anyhow::Result<()> {
    let mut message = serde_json::to_vec(value)?;
    if message.len() > 16 * 1024 {
        bail!("enrollment message exceeds limit");
    }
    message.push(b'\n');
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

fn emit_json(value: &serde_json::Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn advertise_lan(
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
        ]),
    )?
    .enable_addr_auto();
    let daemon = ServiceDaemon::new()?;
    daemon.register(service)?;
    Ok(daemon)
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

fn decode_key(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("identity keys must be 32-byte lowercase hexadecimal values");
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(Into::into))
        .collect()
}
