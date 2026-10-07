//! The versioned host admission the helper reads from its stdin pipe.

use std::net::SocketAddr;

use anyhow::{bail, Context};
use latch_transport::link::LinkPurpose;
use serde::Deserialize;

/// Versioned host admission read from an inherited stdin pipe. The bearer is
/// never accepted in argv, an environment variable, or a file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteLinkHostConfig {
    pub(super) version: u8,
    pub(super) purpose: LinkPurpose,
    pub(super) relay_url: String,
    pub(super) admission: String,
    #[serde(default)]
    pub(super) peer_public_key: Option<String>,
    #[serde(default)]
    pub(super) grant_revision: u64,
    #[serde(default)]
    pub(super) enrollment_id: Option<String>,
    #[serde(default)]
    pub(super) enrollment_secret: Option<String>,
    #[serde(default = "default_lan_bind")]
    pub(super) lan_bind: String,
}

fn default_lan_bind() -> String {
    "0.0.0.0:0".into()
}

impl RemoteLinkHostConfig {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
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

pub(super) fn decode_key(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("identity keys must be 32-byte lowercase hexadecimal values");
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(Into::into))
        .collect()
}
