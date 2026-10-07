//! Noise XX authentication, the purpose- and pin-bound prologue, and the
//! encrypted LinkHello exchanged before the multiplexer is exposed.

use rand::RngCore;
use serde::{Deserialize, Serialize};

use super::error::auth_error;
use super::{
    LinkConfig, LinkError, LinkHello, LinkLimits, LinkPurpose, LinkRole, RecordIo, MAX_RECORD_BYTES,
};

pub(super) const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

pub(super) async fn run_handshake<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
    role: LinkRole,
) -> Result<(), LinkError> {
    match role {
        LinkRole::Controller => {
            handshake_send(records, handshake).await?;
            handshake_receive(records, handshake).await?;
            handshake_send(records, handshake).await?;
        }
        LinkRole::Host => {
            handshake_receive(records, handshake).await?;
            handshake_send(records, handshake).await?;
            handshake_receive(records, handshake).await?;
        }
    }
    Ok(())
}

async fn handshake_send<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
) -> Result<(), LinkError> {
    let mut output = vec![0_u8; MAX_RECORD_BYTES];
    let written = handshake
        .write_message(&[], &mut output)
        .map_err(auth_error)?;
    records.send_record(output[..written].to_vec()).await
}

async fn handshake_receive<R: RecordIo>(
    records: &mut R,
    handshake: &mut snow::HandshakeState,
) -> Result<(), LinkError> {
    let record = records.recv_record().await?.ok_or(LinkError::Closed)?;
    let mut payload = vec![0_u8; MAX_RECORD_BYTES];
    handshake
        .read_message(&record, &mut payload)
        .map_err(auth_error)?;
    Ok(())
}

pub(super) fn prologue(config: &LinkConfig) -> Result<Vec<u8>, LinkError> {
    let mut value = b"latch-remote-link\0v1\0".to_vec();
    value.extend_from_slice(match config.purpose {
        LinkPurpose::Enrollment => b"enrollment\0",
        LinkPurpose::Session => b"session\0",
    });
    value.extend_from_slice(b"controller->host\0");
    if config.purpose == LinkPurpose::Session {
        let remote = config
            .expected_remote_public_key
            .as_ref()
            .expect("validated");
        let (controller, host) = match config.role {
            LinkRole::Controller => (&config.local_public_key, remote),
            LinkRole::Host => (remote, &config.local_public_key),
        };
        value.extend_from_slice(controller);
        value.extend_from_slice(host);
    } else {
        value.extend_from_slice(
            config
                .enrollment_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        value.push(0);
        value.extend_from_slice(config.enrollment_secret.as_ref().expect("validated"));
    }
    Ok(value)
}

pub(super) fn new_hello(grant_revision: u64) -> LinkHello {
    let mut nonce = [0_u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    LinkHello {
        r#type: "link_hello".into(),
        version: 1,
        nonce: hex(&nonce),
        grant_revision,
        limits: LinkLimits::default(),
    }
}

pub(super) fn validate_hello(hello: &LinkHello) -> Result<(), LinkError> {
    if hello.r#type != "link_hello" || hello.version != 1 || hello.nonce.len() != 64 {
        return Err(LinkError::Authentication("invalid LinkHello".into()));
    }
    if hello.limits != LinkLimits::default() {
        return Err(LinkError::Limit("peer selected unsupported limits"));
    }
    Ok(())
}

pub(super) async fn send_encrypted_json<R: RecordIo, T: Serialize>(
    records: &mut R,
    cipher: &mut snow::TransportState,
    value: &T,
) -> Result<(), LinkError> {
    let plaintext =
        serde_json::to_vec(value).map_err(|error| LinkError::Authentication(error.to_string()))?;
    let mut output = vec![0_u8; plaintext.len() + 16];
    let written = cipher
        .write_message(&plaintext, &mut output)
        .map_err(auth_error)?;
    output.truncate(written);
    records.send_record(output).await
}

pub(super) async fn recv_encrypted_json<R: RecordIo, T: for<'de> Deserialize<'de>>(
    records: &mut R,
    cipher: &mut snow::TransportState,
) -> Result<T, LinkError> {
    let record = records.recv_record().await?.ok_or(LinkError::Closed)?;
    let mut plaintext = vec![0_u8; record.len()];
    let written = cipher
        .read_message(&record, &mut plaintext)
        .map_err(auth_error)?;
    serde_json::from_slice(&plaintext[..written])
        .map_err(|error| LinkError::Authentication(error.to_string()))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}
