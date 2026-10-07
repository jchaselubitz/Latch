//! Host identity, enrollment, controller grants, and bounded audit storage.

use super::DevicePermission;
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};
use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use x25519_dalek::{PublicKey, StaticSecret};
const MAX_PAIRED_DEVICES: usize = 32;
const MAX_AUDIT_EVENTS: usize = 1_024;
const MAX_AUDIT_BYTES: usize = 512 * 1024;
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
pub(super) struct DeviceRecord {
    pub(super) device_id: String,
    name: String,
    public_key: String,
    pub(super) permission: DevicePermission,
    #[serde(default = "initial_grant_revision")]
    pub(super) grant_revision: u64,
    pub(super) revoked: bool,
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
pub(super) struct Paths {
    root: PathBuf,
}

impl Paths {
    pub(super) fn new(home: &LatchHome) -> Self {
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
    pub(super) fn audit(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
    pub(super) fn runtime(&self) -> PathBuf {
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

/// Punctuation a paired-device name may contain besides letters and digits.
/// The phone (`PairingModel.enrollableName`), the control plane label check and
/// `fixtures/remote-link/v1/device-names.json` all state the same set;
/// `scripts/check-remote-link-contract.sh` fails when they drift.
pub const DEVICE_NAME_PUNCTUATION: &str = " ._'()-";
/// Longest paired-device name the Mac stores, in UTF-8 bytes.
pub const MAX_DEVICE_NAME_BYTES: usize = 80;

/// The name a peer proposes is shown to the owner in the approval prompt, so
/// anything outside the allowlist is refused rather than cleaned up: the
/// prompt must show exactly what will be stored, and a newline, tab, bidi
/// control or zero-width character could otherwise restyle the sentence that
/// states the requested grant.
pub fn validate_device_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.len() > MAX_DEVICE_NAME_BYTES {
        bail!("device name must be between 1 and {MAX_DEVICE_NAME_BYTES} bytes");
    }
    if name.starts_with(' ') || name.ends_with(' ') {
        bail!("device name must not begin or end with a space");
    }
    if !name
        .chars()
        .all(|c| c.is_alphabetic() || c.is_numeric() || DEVICE_NAME_PUNCTUATION.contains(c))
    {
        bail!("device name may contain only letters, digits, spaces and . ' _ ( ) -");
    }
    Ok(())
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
    validate_device_name(name)?;
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
pub(super) fn current_device(
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

/// Resolves a controller key to its current record. A phone keeps its
/// identity key across re-enrollment, so the store can hold revoked earlier
/// records for the same key; enrollment refuses a second *active* record for
/// a key, so the active one (if any) is the authority, and only when every
/// record for the key is revoked does the newest revoked one answer.
pub(super) fn lookup_device(
    paths: &Paths,
    public_key: &str,
) -> anyhow::Result<Option<DeviceRecord>> {
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

pub(super) fn ensure_enabled(paths: &Paths) -> anyhow::Result<()> {
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

pub(super) fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
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

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
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

pub(super) fn audit(
    paths: &Paths,
    event: &str,
    device_id: Option<&str>,
    result: &str,
) -> anyhow::Result<()> {
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
#[path = "tests.rs"]
mod tests;
