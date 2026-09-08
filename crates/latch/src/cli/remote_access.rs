//! Local authority and fixed loopback gateway for Remote Link.
//!
//! Internet and LAN transport live exclusively in `latch-transport`, driven
//! by `latch-remote`. This ordinary CLI crate stores the Mac identity and
//! exact controller grants, and accepts only already-authenticated logical
//! streams for proxying to a supervised loopback gateway.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::cli::serve::routes::{route_for, DEVICE_GRANT_HEADER, DEVICE_ID_HEADER};
use crate::cli::serve::{load_token, mint_token};
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

pub use crate::cli::serve::routes::Grant as DevicePermission;

const MAX_PAIRED_DEVICES: usize = 32;
const MAX_INITIAL_REQUEST: usize = 32 * 1024;
const MAX_LINK_STREAMS: usize = 32;
const MAX_AUDIT_EVENTS: usize = 1_024;
const MAX_AUDIT_BYTES: usize = 512 * 1024;
const GATEWAY_BIND: &str = "127.0.0.1:0";
const CURRENT_GRANT_INTERVAL: Duration = Duration::from_millis(250);
/// Inactivity deadline for a logical application stream.
pub const PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[cfg(all(target_os = "macos", not(test)))]
const SECRET_SERVICE: &str = "co.cooperativ.latch.remote-access";

/// Public data for an enrolled controller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    /// Mac-local opaque controller identifier.
    pub device_id: String,
    /// Owner-visible controller name approved during enrollment.
    pub name: String,
    /// Current Mac-owned application grant.
    pub permission: DevicePermission,
    /// Whether the controller has been permanently revoked.
    pub revoked: bool,
    #[serde(default = "initial_grant_revision")]
    /// Monotonic revision invalidating stale transport admissions.
    pub grant_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Matching durable control-plane device, when enrollment completed.
    pub control_plane_device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Identity {
    device_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    private_key: String,
    public_key: String,
    #[serde(default = "initial_key_generation")]
    key_generation: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    name: String,
    public_key: String,
    permission: DevicePermission,
    #[serde(default = "initial_grant_revision")]
    grant_revision: u64,
    revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_plane_device_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeviceStore {
    devices: Vec<DeviceRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditEvent<'a> {
    timestamp: u64,
    event: &'a str,
    device_id: Option<&'a str>,
    result: &'a str,
}

/// Owner-facing lifecycle snapshot. It contains no listener address, bearer,
/// peer key, or transport metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatus {
    /// Status document version.
    pub format_version: u8,
    /// Whether the Mac accepts Remote Link connections.
    pub enabled: bool,
    /// Mac-local host device identifier.
    pub device_id: Option<String>,
    /// Host static public key pinned during enrollment.
    pub public_key: Option<String>,
    /// Current host identity generation.
    pub key_generation: Option<u64>,
    /// Number of retained controller records.
    pub paired_devices: usize,
    /// Number of retained revoked controller records.
    pub revoked_devices: usize,
}

#[derive(Clone)]
struct Paths {
    root: PathBuf,
}

impl Paths {
    fn new(home: &LatchHome) -> Self {
        Self {
            root: home.remote_access_dir(),
        }
    }
    fn identity(&self) -> PathBuf {
        self.root.join("identity.json")
    }
    #[cfg(any(not(target_os = "macos"), test))]
    fn identity_secret(&self) -> PathBuf {
        self.root.join("identity.key")
    }
    fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    fn devices(&self) -> PathBuf {
        self.root.join("devices.json")
    }
    fn audit(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
    fn runtime(&self) -> PathBuf {
        self.root.join("runtime")
    }
}

/// Enables or disables admission of new Remote Link connections.
pub fn set_enabled(home: &LatchHome, enabled: bool) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    ensure_root(&paths)?;
    if enabled {
        let _ = identity(&paths)?;
    }
    write_json(&paths.settings(), &Settings { enabled })?;
    if !enabled {
        let _ = fs::remove_file(paths.runtime().join("remote-link-gateway.token"));
        let _ = fs::remove_file(paths.runtime().join("remote-link-gateway-ready.json"));
    }
    audit(
        &paths,
        if enabled {
            "remote_access_enabled"
        } else {
            "remote_access_disabled"
        },
        None,
        "ok",
    )
}

/// Atomically records the exact controller key approved over the authenticated
/// enrollment stream. Replaying the exact committed enrollment is idempotent;
/// changing any security-relevant field is refused.
pub fn authorize_enrollment(
    home: &LatchHome,
    enrollment_id: &str,
    device_public_key: &str,
    name: &str,
    permission: DevicePermission,
    control_plane_device_id: &str,
) -> anyhow::Result<DeviceSummary> {
    let paths = Paths::new(home);
    ensure_enabled(&paths)?;
    decode_static_key(device_public_key)?;
    if !valid_prefixed_id(enrollment_id, "enr") {
        bail!("invalid enrollment id");
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 80 {
        bail!("device name must be between 1 and 80 characters");
    }
    let control_plane_device_id = control_plane_device_id.trim();
    if !valid_prefixed_id(control_plane_device_id, "dev") {
        bail!("invalid control-plane device id");
    }
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    if let Some(existing) = store
        .devices
        .iter()
        .find(|device| device.enrollment_id.as_deref() == Some(enrollment_id))
    {
        if existing.public_key == device_public_key
            && existing.control_plane_device_id.as_deref() == Some(control_plane_device_id)
            && existing.permission == permission
            && !existing.revoked
        {
            return Ok(summary(existing));
        }
        bail!("enrollment was already committed to a different controller");
    }
    if store.devices.len() >= MAX_PAIRED_DEVICES {
        bail!("paired device limit reached");
    }
    if store.devices.iter().any(|device| {
        (!device.revoked && device.public_key == device_public_key)
            || device.control_plane_device_id.as_deref() == Some(control_plane_device_id)
    }) {
        bail!("controller identity is already enrolled");
    }
    let record = DeviceRecord {
        device_id: random_hex(16)?,
        name: name.to_owned(),
        public_key: device_public_key.to_owned(),
        permission,
        grant_revision: initial_grant_revision(),
        revoked: false,
        enrollment_id: Some(enrollment_id.to_owned()),
        control_plane_device_id: Some(control_plane_device_id.to_owned()),
    };
    let result = summary(&record);
    store.devices.push(record);
    write_json(&paths.devices(), &store)?;
    audit(&paths, "enrollment_approved", Some(&result.device_id), "ok")?;
    Ok(result)
}

/// Records one content-free Remote Link lifecycle event from the helper
/// (`link_ready`, `link_closed`, ...) so `latch remote-access audit` shows
/// the link's own stage transitions next to the stream events. `detail` is
/// a fixed vocabulary word such as a carrier name or close reason.
pub fn record_link_status(home: &LatchHome, status: &str, detail: &str) -> anyhow::Result<()> {
    let ok = |value: &str| {
        !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    if !ok(status) || !ok(detail) {
        bail!("link status and detail must be short lowercase identifiers");
    }
    audit(&Paths::new(home), &format!("link_{status}"), None, detail)
}

/// Lists retained controller records without secrets.
pub fn list_devices(home: &LatchHome) -> anyhow::Result<Vec<DeviceSummary>> {
    let store: DeviceStore = read_json_or_default(&Paths::new(home).devices())?;
    Ok(store.devices.iter().map(summary).collect())
}

/// Changes a controller grant and advances its revision.
pub fn grant(
    home: &LatchHome,
    device_id: &str,
    permission: DevicePermission,
) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    let device = store
        .devices
        .iter_mut()
        .find(|item| item.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    if device.revoked {
        bail!("device is revoked");
    }
    device.permission = permission;
    device.grant_revision = device
        .grant_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("device grant revision exhausted"))?;
    write_json(&paths.devices(), &store)?;
    audit(&paths, "permission_changed", Some(device_id), "ok")
}

/// Revokes a controller locally and advances its revision.
pub fn revoke(home: &LatchHome, device_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    let device = store
        .devices
        .iter_mut()
        .find(|item| item.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    device.revoked = true;
    device.grant_revision = device
        .grant_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("device grant revision exhausted"))?;
    write_json(&paths.devices(), &store)?;
    audit(&paths, "device_revoked", Some(device_id), "ok")
}

/// Replaces a controller key while invalidating existing admissions.
pub fn rotate_device_key(
    home: &LatchHome,
    device_id: &str,
    new_public_key: &str,
) -> anyhow::Result<()> {
    decode_static_key(new_public_key)?;
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    if store
        .devices
        .iter()
        .any(|item| item.device_id != device_id && item.public_key == new_public_key)
    {
        bail!("device identity is already paired");
    }
    let device = store
        .devices
        .iter_mut()
        .find(|item| item.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    if device.revoked {
        bail!("device is revoked");
    }
    device.public_key = new_public_key.to_owned();
    device.grant_revision = device
        .grant_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("device grant revision exhausted"))?;
    write_json(&paths.devices(), &store)?;
    audit(&paths, "device_key_rotated", Some(device_id), "ok")
}

/// Reads the bounded content-free local security audit.
pub fn read_audit(home: &LatchHome) -> anyhow::Result<Vec<serde_json::Value>> {
    let path = Paths::new(home).audit();
    if !path.exists() {
        return Ok(Vec::new());
    }
    String::from_utf8(read_private_bytes(&path)?)
        .context("audit log is not UTF-8")?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .context("invalid audit log")
}

/// Reads the owner-facing Remote Link lifecycle snapshot.
pub fn status(home: &LatchHome) -> anyhow::Result<RemoteAccessStatus> {
    let paths = Paths::new(home);
    let settings: Settings = read_json_or_default(&paths.settings())?;
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    let identity: Option<Identity> = if paths.identity().is_file() {
        Some(read_json(&paths.identity())?)
    } else {
        None
    };
    Ok(RemoteAccessStatus {
        format_version: 2,
        enabled: settings.enabled,
        device_id: identity.as_ref().map(|item| item.device_id.clone()),
        public_key: identity.as_ref().map(|item| item.public_key.clone()),
        key_generation: identity.as_ref().map(|item| item.key_generation),
        paired_devices: store.devices.len(),
        revoked_devices: store.devices.iter().filter(|item| item.revoked).count(),
    })
}

/// Endpoint key material copied into the dedicated transport helper.
pub struct RemoteLinkIdentityMaterial {
    /// Host static private key, copied only into the dedicated helper process.
    pub private_key: String,
    /// Host static public key.
    pub public_key: String,
}

/// Loads the enabled host identity for the dedicated transport helper.
pub fn remote_link_identity(home: &LatchHome) -> anyhow::Result<RemoteLinkIdentityMaterial> {
    let paths = Paths::new(home);
    ensure_enabled(&paths)?;
    let identity = identity(&paths)?;
    Ok(RemoteLinkIdentityMaterial {
        private_key: identity.private_key,
        public_key: identity.public_key,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Readiness {
    address: String,
}

fn gateway_args(token: &Path, ready: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "serve".into(),
        "--bind".into(),
        GATEWAY_BIND.into(),
        "--token-file".into(),
        token.as_os_str().to_owned(),
        "--ready-file".into(),
        ready.as_os_str().to_owned(),
    ]
}

async fn start_gateway(binary: &Path, token: &Path, ready: &Path) -> anyhow::Result<Child> {
    let _ = fs::remove_file(ready);
    mint_token(token)?;
    Command::new(binary)
        .args(gateway_args(token, ready))
        .kill_on_drop(true)
        .spawn()
        .context("cannot start supervised loopback gateway")
}

/// Cloneable sink for independently scheduled authenticated logical streams.
#[derive(Clone)]
pub struct AuthenticatedGateway {
    paths: Paths,
    peer_public_key: String,
    grant_revision: u64,
    token: PathBuf,
    address: SocketAddr,
    stream_limit: Arc<Semaphore>,
    streams: Arc<AtomicUsize>,
}

/// Owns the fixed loopback gateway child for one authenticated physical link.
pub struct AuthenticatedGatewayOwner {
    gateway: AuthenticatedGateway,
    child: Child,
}

impl AuthenticatedGatewayOwner {
    /// Starts one fixed loopback gateway for an authenticated physical link.
    pub async fn start(
        home: &LatchHome,
        latch_bin: &Path,
        peer_public_key: &str,
        grant_revision: u64,
    ) -> anyhow::Result<Self> {
        let paths = Paths::new(home);
        ensure_enabled(&paths)?;
        let device = current_device(&paths, peer_public_key, grant_revision)?;
        if device.revoked {
            bail!("revoked controller");
        }
        ensure_private_directory(&paths.runtime())?;
        let ready = paths.runtime().join("remote-link-gateway-ready.json");
        let token = paths.runtime().join("remote-link-gateway.token");
        let child = start_gateway(latch_bin, &token, &ready).await?;
        let readiness = wait_readiness(&ready).await?;
        let address: SocketAddr = readiness
            .address
            .parse()
            .context("invalid gateway address")?;
        if !address.ip().is_loopback() {
            bail!("supervised gateway was not loopback-bound");
        }
        Ok(Self {
            gateway: AuthenticatedGateway {
                paths,
                peer_public_key: peer_public_key.to_owned(),
                grant_revision,
                token,
                address,
                stream_limit: Arc::new(Semaphore::new(MAX_LINK_STREAMS)),
                streams: Arc::new(AtomicUsize::new(0)),
            },
            child,
        })
    }

    /// Returns a cloneable proxy handle for independent logical streams.
    pub fn gateway(&self) -> AuthenticatedGateway {
        self.gateway.clone()
    }

    /// Waits for unexpected gateway termination.
    pub async fn wait(&mut self) -> anyhow::Result<()> {
        self.child
            .wait()
            .await
            .context("supervised gateway exited")?;
        bail!("supervised gateway exited")
    }

    /// Stops and reaps the supervised gateway child.
    pub async fn close(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

impl AuthenticatedGateway {
    /// Proxies one logical stream after rechecking current local authority.
    pub async fn proxy<S>(&self, stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let _permit = self
            .stream_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("Remote Link stream capacity exceeded"))?;
        let token = load_token(&self.token)?;
        proxy_authenticated_stream(
            stream,
            &self.paths,
            &self.peer_public_key,
            self.grant_revision,
            &token,
            self.address,
            &self.streams,
        )
        .await
    }
}

#[cfg(feature = "remote-link-test")]
/// Injects a loopback target for composed integration testing.
pub async fn proxy_authenticated_stream_for_test<S>(
    home: &LatchHome,
    stream: S,
    peer_public_key: &str,
    grant_revision: u64,
    gateway_token: &str,
    gateway_address: SocketAddr,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    proxy_authenticated_stream(
        stream,
        &Paths::new(home),
        peer_public_key,
        grant_revision,
        gateway_token,
        gateway_address,
        &Arc::new(AtomicUsize::new(0)),
    )
    .await
}

struct StreamGauge(Arc<AtomicUsize>);

impl StreamGauge {
    fn raise(value: &Arc<AtomicUsize>) -> Self {
        value.fetch_add(1, Ordering::Relaxed);
        Self(value.clone())
    }
}

impl Drop for StreamGauge {
    fn drop(&mut self) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
    }
}

async fn proxy_authenticated_stream<S>(
    mut stream: S,
    paths: &Paths,
    peer_key: &str,
    grant_revision: u64,
    gateway_token: &str,
    gateway_addr: SocketAddr,
    streams: &Arc<AtomicUsize>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let device = current_device(paths, peer_key, grant_revision)?;
    let mut initial = Vec::new();
    while !initial.windows(4).any(|window| window == b"\r\n\r\n") {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request headers exceed limit");
        }
        let mut bytes = [0_u8; 4096];
        let read = stream.read(&mut bytes).await?;
        if read == 0 {
            bail!("Remote Link stream closed before request");
        }
        initial.extend_from_slice(&bytes[..read]);
    }
    let required_len = complete_initial_request_len(&initial)?;
    while initial.len() < required_len {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request exceeds limit");
        }
        let mut bytes = [0_u8; 4096];
        let read = stream.read(&mut bytes).await?;
        if read == 0 {
            bail!("Remote Link stream closed during request");
        }
        initial.extend_from_slice(&bytes[..read]);
    }
    let (initial, required) =
        authorize_and_inject(initial, device.permission, &device.device_id, gateway_token)?;
    let gateway = TcpStream::connect(gateway_addr)
        .await
        .context("cannot connect to loopback gateway")?;
    let (mut gateway_reader, mut gateway_writer) = gateway.into_split();
    gateway_writer.write_all(&initial).await?;
    let _gauge = StreamGauge::raise(streams);
    audit(
        paths,
        "remote_link_stream_opened",
        Some(&device.device_id),
        "ok",
    )?;

    let (mut remote_reader, mut remote_writer) = tokio::io::split(stream);
    let mut outbound = tokio::spawn(async move {
        tokio::io::copy(&mut gateway_reader, &mut remote_writer).await?;
        remote_writer.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    });
    let mut inbound = tokio::spawn(async move {
        tokio::io::copy(&mut remote_reader, &mut gateway_writer).await?;
        gateway_writer.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    });
    let mut current_grant = tokio::time::interval(CURRENT_GRANT_INTERVAL);
    tokio::select! {
        result = &mut outbound => {
            inbound.abort();
            result??
        }
        result = &mut inbound => {
            result??;
            tokio::time::timeout(PROXY_IDLE_TIMEOUT, &mut outbound)
                .await
                .map_err(|_| anyhow!("gateway response idle timeout"))???
        }
        result = async {
            loop {
                current_grant.tick().await;
                match lookup_device(paths, peer_key)? {
                    Some(current) if !current.revoked
                        && current.grant_revision == grant_revision
                        && current.permission.permits(required) => {}
                    _ => return Ok::<(), anyhow::Error>(()),
                }
            }
        } => {
            outbound.abort();
            inbound.abort();
            result?
        }
    };
    audit(
        paths,
        "remote_link_stream_closed",
        Some(&device.device_id),
        "ok",
    )?;
    Ok(())
}

fn current_device(
    paths: &Paths,
    peer_key: &str,
    grant_revision: u64,
) -> anyhow::Result<DeviceRecord> {
    let device = lookup_device(paths, peer_key)?.ok_or_else(|| anyhow!("unpaired controller"))?;
    if device.revoked {
        bail!("revoked controller");
    }
    if device.grant_revision != grant_revision {
        bail!("stale controller grant revision");
    }
    Ok(device)
}

fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    device_id: &str,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers = std::str::from_utf8(&request[..end]).context("request headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words.next().ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = words.next().ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = words
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if words.next().is_some()
        || version != "HTTP/1.1"
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            bail!("malformed HTTP header");
        }
        let (name, value) = line.split_once(':').expect("header delimiter checked");
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
            || name.eq_ignore_ascii_case(DEVICE_ID_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
        websocket_upgrade |=
            name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket");
    }
    let required_len = complete_initial_request_len(&request)?;
    if required_len != request.len() {
        bail!("HTTP pipelining is not permitted through Remote Link");
    }
    let (_, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through Remote Link"))?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_GRANT_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(permission.as_header_value().as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_ID_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(device_id.as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(&request[end..]);
    Ok((injected, required))
}

fn complete_initial_request_len(request: &[u8]) -> anyhow::Result<usize> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers =
        std::str::from_utf8(&request[..header_end]).context("request headers are not UTF-8")?;
    let lengths = headers
        .split("\r\n")
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()
        .context("invalid Content-Length")?;
    if lengths.len() > 1 {
        bail!("multiple Content-Length headers are not permitted");
    }
    let total = header_end + 4 + lengths.first().copied().unwrap_or(0);
    if total > MAX_INITIAL_REQUEST {
        bail!("initial request exceeds limit");
    }
    Ok(total)
}

async fn wait_readiness(path: &Path) -> anyhow::Result<Readiness> {
    for _ in 0..100 {
        if let Ok(contents) = tokio::fs::read_to_string(path).await {
            return serde_json::from_str(&contents).context("invalid gateway readiness document");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("timed out waiting for supervised gateway readiness")
}

/// Resolves a controller key to its current record. A phone keeps its
/// identity key across re-enrollment, so the store can hold revoked earlier
/// records for the same key; enrollment refuses a second *active* record for
/// a key, so the active one (if any) is the authority, and only when every
/// record for the key is revoked does the newest revoked one answer.
fn lookup_device(paths: &Paths, public_key: &str) -> anyhow::Result<Option<DeviceRecord>> {
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    let mut matching = store
        .devices
        .into_iter()
        .filter(|item| item.public_key == public_key);
    let mut newest = None;
    for item in matching.by_ref() {
        if !item.revoked {
            return Ok(Some(item));
        }
        newest = Some(item);
    }
    Ok(newest)
}

fn identity(paths: &Paths) -> anyhow::Result<Identity> {
    ensure_root(paths)?;
    if paths.identity().exists() {
        let mut identity: Identity = read_json(&paths.identity())?;
        if identity.private_key.is_empty() {
            identity.private_key = load_identity_secret(paths, &identity.device_id)?;
        } else {
            store_identity_secret(paths, &identity.device_id, &identity.private_key)?;
            let private = std::mem::take(&mut identity.private_key);
            write_json(&paths.identity(), &identity)?;
            identity.private_key = private;
        }
        decode_static_key(&identity.private_key).context("invalid stored Mac identity")?;
        return Ok(identity);
    }
    let mut private = [0_u8; 32];
    fill_random(&mut private)?;
    let secret = StaticSecret::from(private);
    let public = PublicKey::from(&secret);
    let identity = Identity {
        device_id: random_hex(16)?,
        private_key: hex_encode(secret.as_bytes()),
        public_key: hex_encode(public.as_bytes()),
        key_generation: initial_key_generation(),
    };
    store_identity_secret(paths, &identity.device_id, &identity.private_key)?;
    let mut public_identity = identity.clone();
    public_identity.private_key.clear();
    write_json(&paths.identity(), &public_identity)?;
    Ok(identity)
}

fn ensure_enabled(paths: &Paths) -> anyhow::Result<()> {
    ensure_root(paths)?;
    let settings: Settings = read_json_or_default(&paths.settings())?;
    if !settings.enabled {
        bail!("remote access is disabled; run `latch remote-access enable` first");
    }
    Ok(())
}

fn ensure_root(paths: &Paths) -> anyhow::Result<()> {
    ensure_private_directory(&paths.root)
}

fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("refusing symlinked private directory {}", path.display());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        bail!("refusing non-private directory {}", path.display());
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&read_private_bytes(path)?)
        .with_context(|| format!("invalid {}", path.display()))
}

fn read_private_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("refusing insecure private file {}", path.display());
    }
    fs::read(path).with_context(|| format!("cannot read {}", path.display()))
}

fn read_json_or_default<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> anyhow::Result<T> {
    if path.exists() {
        read_json(path)
    } else {
        Ok(T::default())
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    ensure_private_directory(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        random_hex(8)?
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(FILE_MODE))?;
    fs::rename(&temporary, path)?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

fn audit(paths: &Paths, event: &str, device_id: Option<&str>, result: &str) -> anyhow::Result<()> {
    ensure_root(paths)?;
    let bytes = serde_json::to_vec(&AuditEvent {
        timestamp: unix_time(),
        event,
        device_id,
        result,
    })?;
    let existing = if paths.audit().exists() {
        read_private_bytes(&paths.audit())?
    } else {
        Vec::new()
    };
    let mut lines: VecDeque<&[u8]> = existing
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    while lines.len() >= MAX_AUDIT_EVENTS {
        lines.pop_front();
    }
    let mut retained = lines.iter().map(|line| line.len() + 1).sum::<usize>();
    while retained.saturating_add(bytes.len() + 1) > MAX_AUDIT_BYTES {
        let Some(removed) = lines.pop_front() else {
            break;
        };
        retained = retained.saturating_sub(removed.len() + 1);
    }
    let mut bounded = Vec::with_capacity(retained + bytes.len() + 1);
    for line in lines {
        bounded.extend_from_slice(line);
        bounded.push(b'\n');
    }
    bounded.extend_from_slice(&bytes);
    bounded.push(b'\n');
    write_bytes_atomic(&paths.audit(), &bounded)
}

fn summary(record: &DeviceRecord) -> DeviceSummary {
    DeviceSummary {
        device_id: record.device_id.clone(),
        name: record.name.clone(),
        permission: record.permission,
        revoked: record.revoked,
        grant_revision: record.grant_revision,
        control_plane_device_id: record.control_plane_device_id.clone(),
    }
}

fn initial_key_generation() -> u64 {
    1
}
fn initial_grant_revision() -> u64 {
    1
}

#[cfg(all(target_os = "macos", not(test)))]
fn store_identity_secret(_paths: &Paths, account: &str, secret: &str) -> anyhow::Result<()> {
    security_framework::passwords::set_generic_password(SECRET_SERVICE, account, secret.as_bytes())
        .context("cannot store the remote-access identity in macOS Keychain")
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_identity_secret(_paths: &Paths, account: &str) -> anyhow::Result<String> {
    let bytes = security_framework::passwords::get_generic_password(SECRET_SERVICE, account)
        .context("cannot load the remote-access identity from macOS Keychain")?;
    String::from_utf8(bytes).context("stored remote-access identity is not UTF-8")
}

#[cfg(any(not(target_os = "macos"), test))]
fn store_identity_secret(paths: &Paths, _account: &str, secret: &str) -> anyhow::Result<()> {
    write_bytes_atomic(&paths.identity_secret(), secret.as_bytes())
}

#[cfg(any(not(target_os = "macos"), test))]
fn load_identity_secret(paths: &Paths, _account: &str) -> anyhow::Result<String> {
    let path = paths.identity_secret();
    String::from_utf8(read_private_bytes(&path)?)
        .context("stored remote-access identity is not UTF-8")
}

fn fill_random(bytes: &mut [u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .read(true)
        .open("/dev/urandom")?
        .read_exact(bytes)?;
    Ok(())
}

fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut data = vec![0_u8; bytes];
    fill_random(&mut data)?;
    Ok(hex_encode(&data))
}

fn valid_prefixed_id(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(&format!("{prefix}_"))
        .is_some_and(|hex| {
            hex.len() == 32
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn decode_static_key(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("device key must be 32-byte lowercase hexadecimal");
    }
    decode_hex(value)
}

fn decode_hex(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() % 2 != 0 {
        bail!("hex value must have an even length");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0]).ok_or_else(|| anyhow!("invalid hex value"))?;
            let low = hex_nibble(pair[1]).ok_or_else(|| anyhow!("invalid hex value"))?;
            Ok(high << 4 | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn enrolled_home(permission: DevicePermission) -> (tempfile::TempDir, LatchHome, String) {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path());
        set_enabled(&home, true).unwrap();
        let secret = StaticSecret::from([7_u8; 32]);
        let key = hex_encode(PublicKey::from(&secret).as_bytes());
        authorize_enrollment(
            &home,
            &format!("enr_{}", "1".repeat(32)),
            &key,
            "Test phone",
            permission,
            &format!("dev_{}", "2".repeat(32)),
        )
        .unwrap();
        (directory, home, key)
    }

    #[test]
    fn a_re_enrolled_key_resolves_to_its_active_record_not_an_older_revoked_one() {
        // A phone keeps its identity key across re-pairing. The first
        // pairing after the transport replacement failed exactly here: the
        // retired-protocol record for the same key had been revoked and the
        // lookup returned it, so the helper refused a controller the owner
        // had just approved.
        let (_directory, home, key) = enrolled_home(DevicePermission::Control);
        let paths = Paths::new(&home);
        let first = list_devices(&home).unwrap()[0].device_id.clone();
        revoke(&home, &first).unwrap();
        assert!(matches!(
            current_device(&paths, &key, initial_grant_revision()),
            Err(error) if error.to_string() == "revoked controller"
        ));

        let second = authorize_enrollment(
            &home,
            &format!("enr_{}", "3".repeat(32)),
            &key,
            "Test phone again",
            DevicePermission::Interact,
            &format!("dev_{}", "4".repeat(32)),
        )
        .unwrap();
        let current = current_device(&paths, &key, initial_grant_revision()).unwrap();
        assert_eq!(current.device_id, second.device_id);
        assert_eq!(current.permission, DevicePermission::Interact);
        assert!(!current.revoked);

        // Revoking the active record leaves only revoked history for the key,
        // and that must still read as revoked, never as unpaired.
        revoke(&home, &second.device_id).unwrap();
        assert!(matches!(
            current_device(&paths, &key, initial_grant_revision()),
            Err(error) if error.to_string() == "revoked controller"
        ));
    }

    #[test]
    fn fixed_gateway_proxy_rejects_forged_authority_paths_and_pipelining() {
        let good = authorize_and_inject(
            b"GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\n".to_vec(),
            DevicePermission::Observe,
            "phone-local-id",
            "internal-token",
        )
        .unwrap()
        .0;
        let good = String::from_utf8(good).unwrap();
        assert!(good.contains("Authorization: Bearer internal-token"));
        assert!(good
            .to_ascii_lowercase()
            .contains("x-latch-device-grant: observe"));
        assert!(good
            .to_ascii_lowercase()
            .contains("x-latch-device-id: phone-local-id"));

        for request in [
            "GET /v2/sessions HTTP/1.1\r\nAuthorization: Bearer forged\r\n\r\n",
            "GET /v2/sessions HTTP/1.1\r\nX-Latch-Device-Grant: control\r\n\r\n",
            "GET /v2/sessions HTTP/1.1\r\nX-Latch-Device-Id: someone-else\r\n\r\n",
            "GET /v2/../secret HTTP/1.1\r\nHost: latch\r\n\r\n",
            "GET /v2/not-a-route HTTP/1.1\r\nHost: latch\r\n\r\n",
            "GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\nGET /v2/sessions HTTP/1.1\r\n\r\n",
        ] {
            assert!(authorize_and_inject(
                request.as_bytes().to_vec(),
                DevicePermission::Control,
                "phone-local-id",
                "internal-token",
            )
            .is_err());
        }

        assert!(authorize_and_inject(
            b"POST /v2/sessions HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            DevicePermission::Observe,
            "phone-local-id",
            "internal-token",
        )
        .is_err());
    }

    #[test]
    fn unreadable_authority_store_fails_closed() {
        let (_directory, home, key) = enrolled_home(DevicePermission::Control);
        let paths = Paths::new(&home);
        fs::write(paths.devices(), b"not-json").unwrap();
        assert!(current_device(&paths, &key, 1).is_err());
    }

    #[tokio::test]
    async fn live_grant_change_closes_an_active_privileged_stream() {
        let (_directory, home, key) = enrolled_home(DevicePermission::Control);
        let device_id = list_devices(&home).unwrap().remove(0).device_id;
        let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = gateway.local_addr().unwrap();
        let gateway_seen = tokio::spawn(async move {
            let (mut stream, _) = gateway.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let count = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..count])
                .to_ascii_lowercase()
                .contains("x-latch-device-grant: control"));
            stream.read_to_end(&mut Vec::new()).await.unwrap();
        });
        let (mut client, server) = tokio::io::duplex(4096);
        let proxy_home = home.clone();
        let proxy_key = key.clone();
        let proxy = tokio::spawn(async move {
            proxy_authenticated_stream(
                server,
                &Paths::new(&proxy_home),
                &proxy_key,
                1,
                "internal-token",
                address,
                &Arc::new(AtomicUsize::new(0)),
            )
            .await
        });
        client
            .write_all(b"GET /v2/sessions/test/terminal HTTP/1.1\r\nHost: latch\r\nUpgrade: websocket\r\n\r\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        grant(&home, &device_id, DevicePermission::Observe).unwrap();
        tokio::time::timeout(Duration::from_secs(2), proxy)
            .await
            .expect("grant enforcement did not close the stream")
            .unwrap()
            .unwrap();
        gateway_seen.await.unwrap();
    }
}
