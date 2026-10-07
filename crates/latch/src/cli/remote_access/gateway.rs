//! Supervised loopback gateway ownership and authenticated stream proxying.

use super::devices::{
    audit, current_device, ensure_enabled, ensure_private_directory, lookup_device, read_json,
    Paths,
};
use super::framing::{authorize_and_inject, request_framing, MAX_INITIAL_REQUEST};
use crate::cli::serve::{load_token, mint_token};
use crate::session::paths::{LatchHome, FILE_MODE};
use anyhow::{anyhow, bail, Context};
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
const MAX_LINK_STREAMS: usize = 32;
const GATEWAY_BIND: &str = "127.0.0.1:0";
const CURRENT_GRANT_INTERVAL: Duration = Duration::from_millis(250);
/// Inactivity deadline for a logical application stream.
pub const PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
        "--exit-with-parent".into(),
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
    stream_limit: Arc<Semaphore>,
    streams: Arc<AtomicUsize>,
}

/// Owns the one fixed loopback gateway shared by every authenticated Remote
/// Link. A separate advisory lock prevents two supervisors from rotating the
/// shared token while the Conversation Hub's own cache lock remains the final
/// writer-exclusion authority.
pub struct SharedGatewayOwner {
    paths: Paths,
    child: Child,
    _owner_lock: GatewayOwnerLock,
}

pub(super) struct GatewayOwnerLock {
    file: fs::File,
}

impl GatewayOwnerLock {
    pub(super) fn acquire(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            bail!("another Remote Link gateway supervisor is already running");
        }
        file.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
        file.set_len(0)?;
        writeln!(&file, "{}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { file })
    }
}

impl Drop for GatewayOwnerLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl SharedGatewayOwner {
    /// Starts the sole fixed loopback gateway for this Latch home.
    pub async fn start(home: &LatchHome, latch_bin: &Path) -> anyhow::Result<Self> {
        let paths = Paths::new(home);
        ensure_enabled(&paths)?;
        ensure_private_directory(&paths.runtime())?;
        let owner_lock =
            GatewayOwnerLock::acquire(&paths.runtime().join("remote-link-gateway-owner.lock"))?;
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
            paths,
            child,
            _owner_lock: owner_lock,
        })
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
        let _ = fs::remove_file(self.paths.runtime().join("remote-link-gateway-ready.json"));
        let _ = fs::remove_file(self.paths.runtime().join("remote-link-gateway.token"));
    }
}

impl AuthenticatedGateway {
    /// Creates a per-device authority handle for the shared gateway. The
    /// readiness address and bearer are deliberately resolved for every new
    /// stream so a gateway restart does not require healthy links to restart.
    pub async fn connect(
        home: &LatchHome,
        peer_public_key: &str,
        grant_revision: u64,
    ) -> anyhow::Result<Self> {
        let paths = Paths::new(home);
        ensure_enabled(&paths)?;
        let device = current_device(&paths, peer_public_key, grant_revision)?;
        if device.revoked {
            bail!("revoked controller");
        }
        let ready = paths.runtime().join("remote-link-gateway-ready.json");
        let readiness = wait_readiness(&ready).await?;
        let address: SocketAddr = readiness
            .address
            .parse()
            .context("invalid gateway address")?;
        if !address.ip().is_loopback() {
            bail!("shared gateway was not loopback-bound");
        }
        Ok(Self {
            token: paths.runtime().join("remote-link-gateway.token"),
            paths,
            peer_public_key: peer_public_key.to_owned(),
            grant_revision,
            stream_limit: Arc::new(Semaphore::new(MAX_LINK_STREAMS)),
            streams: Arc::new(AtomicUsize::new(0)),
        })
    }

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
        let readiness: Readiness =
            read_json(&self.paths.runtime().join("remote-link-gateway-ready.json"))?;
        let address: SocketAddr = readiness
            .address
            .parse()
            .context("invalid gateway address")?;
        if !address.ip().is_loopback() {
            bail!("shared gateway was not loopback-bound");
        }
        let token = load_token(&self.token)?;
        proxy_authenticated_stream(
            stream,
            &self.paths,
            &self.peer_public_key,
            self.grant_revision,
            &token,
            address,
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

pub(super) async fn proxy_authenticated_stream<S>(
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
    let framing = request_framing(&initial)?;
    while initial.len() < framing.buffered {
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
    // The body bytes a streamed route still owes, relayed after authorization.
    // `authorize_and_inject` refuses a buffer longer than the declared request,
    // so this cannot underflow once it has succeeded.
    let streamed_remaining = framing
        .streamed
        .then(|| framing.total.saturating_sub(initial.len()) as u64);
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
        if let Some(remaining) = streamed_remaining {
            let copied = tokio::io::copy(
                &mut (&mut remote_reader).take(remaining),
                &mut gateway_writer,
            )
            .await?;
            if copied != remaining {
                bail!("Remote Link stream closed during a streamed request body");
            }
            // Nothing after the declared body reaches the gateway. A second
            // request riding the same stream would never have been authorized.
            tokio::io::copy(&mut remote_reader, &mut tokio::io::sink()).await?;
        } else {
            tokio::io::copy(&mut remote_reader, &mut gateway_writer).await?;
        }
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

async fn wait_readiness(path: &Path) -> anyhow::Result<Readiness> {
    for _ in 0..100 {
        if let Ok(contents) = tokio::fs::read_to_string(path).await {
            return serde_json::from_str(&contents).context("invalid gateway readiness document");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("timed out waiting for supervised gateway readiness")
}
