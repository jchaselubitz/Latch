//! Sealing attention payloads to a phone's notification key.
//!
//! RFC 9180 HPKE in base mode with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
//! and ChaCha20-Poly1305 — the suite CryptoKit names
//! `HPKE.Ciphersuite.Curve25519_SHA256_ChachaPoly`, so the phone's
//! notification extension opens it with the platform implementation.
//!
//! Wire form: lowercase hex of the 32-byte encapsulated key followed by the
//! ciphertext. The `info` string pins the purpose and version; the event id
//! is the associated data, so a sealed payload cannot be replayed under a
//! different event.

use anyhow::{anyhow, Result};
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, OpModeS, Serializable};
use serde::Serialize;

use super::AttentionKind;

/// HPKE `info`: purpose and payload version. The phone must use the same.
pub const INFO: &[u8] = b"latch-attention/v1";

/// Longest name or title carried, in characters. Keeps the sealed payload
/// well inside APNs' 4 KiB limit whatever a session was called.
pub const MAX_LABEL_CHARS: usize = 80;

/// The only facts a notification may state.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Payload {
    /// Format version.
    pub v: u8,
    /// Session id, so a tap can open exactly this session.
    pub s: String,
    /// Session name.
    pub n: String,
    /// Session title, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// `"approval"` or `"completion"`.
    pub k: &'static str,
}

impl Payload {
    /// Builds the version-1 payload, bounding the labels.
    pub fn new(session: &str, name: &str, title: Option<&str>, kind: AttentionKind) -> Self {
        Self {
            v: 1,
            s: session.to_owned(),
            n: truncate(name),
            t: title.map(truncate).filter(|value| !value.is_empty()),
            k: match kind {
                AttentionKind::Approval => "approval",
                AttentionKind::Completion => "completion",
            },
        }
    }
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_LABEL_CHARS).collect()
}

/// Seals `payload` to `recipient` (an X25519 public key) for `event_id`.
pub fn seal(recipient: &[u8; 32], event_id: &str, payload: &Payload) -> Result<String> {
    let public = <X25519HkdfSha256 as hpke::Kem>::PublicKey::from_bytes(recipient)
        .map_err(|_| anyhow!("invalid notification key"))?;
    let plaintext = serde_json::to_vec(payload)?;
    let (encapped, ciphertext) =
        hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeS::Base,
            &public,
            INFO,
            &plaintext,
            event_id.as_bytes(),
        )
        .map_err(|_| anyhow!("sealing failed"))?;
    let mut wire = encapped.to_bytes().to_vec();
    wire.extend_from_slice(&ciphertext);
    Ok(wire.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpke::{Kem as _, OpModeR};

    fn open(
        secret: &<X25519HkdfSha256 as hpke::Kem>::PrivateKey,
        event_id: &str,
        wire: &str,
    ) -> Vec<u8> {
        let bytes: Vec<u8> = (0..wire.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&wire[i..i + 2], 16).unwrap())
            .collect();
        let (encapped, ciphertext) = bytes.split_at(32);
        let encapped = <X25519HkdfSha256 as hpke::Kem>::EncappedKey::from_bytes(encapped).unwrap();
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            secret,
            &encapped,
            INFO,
            ciphertext,
            event_id.as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn sealed_payload_opens_only_for_its_event() {
        let (secret, public) = X25519HkdfSha256::gen_keypair();
        let key: [u8; 32] = public.to_bytes().as_slice().try_into().unwrap();
        let payload = Payload::new(
            "ses_1",
            "auth-refactor",
            Some("Refactor auth"),
            AttentionKind::Approval,
        );
        let wire = seal(&key, "0123456789abcdef0123456789abcdef", &payload).unwrap();
        let opened = open(&secret, "0123456789abcdef0123456789abcdef", &wire);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&opened).unwrap(),
            serde_json::json!({"v": 1, "s": "ses_1", "n": "auth-refactor", "t": "Refactor auth", "k": "approval"})
        );

        let bytes: Vec<u8> = (0..wire.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&wire[i..i + 2], 16).unwrap())
            .collect();
        let (encapped, ciphertext) = bytes.split_at(32);
        let encapped = <X25519HkdfSha256 as hpke::Kem>::EncappedKey::from_bytes(encapped).unwrap();
        let wrong_event = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            &secret,
            &encapped,
            INFO,
            ciphertext,
            b"ffffffffffffffffffffffffffffffff",
        );
        assert!(wrong_event.is_err());
    }

    #[test]
    fn labels_are_bounded() {
        let long = "x".repeat(500);
        let payload = Payload::new("ses_1", &long, Some(""), AttentionKind::Completion);
        assert_eq!(payload.n.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(payload.t, None);
        assert_eq!(payload.k, "completion");
    }
}

/// A fixed vector shared with the phone's `AttentionPayloadTests`: the same
/// bytes must open on both sides, which is what proves the two HPKE
/// implementations agree on the suite, `info`, and associated data.
#[cfg(test)]
pub(crate) mod interop {
    /// Recipient X25519 secret key.
    pub const SECRET: &str = "896615161d5c4dab6be99bb69b180f22c86eb0ac1b8912e958ed5dd08813573b";
    /// Event id used as associated data.
    pub const EVENT: &str = "0123456789abcdef0123456789abcdef";
    /// Sealed `{"v":1,"s":"ses_vector","n":"auth-refactor","t":"Refactor auth","k":"approval"}`.
    pub const WIRE: &str = "5395375bbdd01f8bf21bdef04680cbec79f3a2f8b3c110e5d0b3186636666c448553f9cf22c79b1b6ab25b15835e5f5256bacc499ec9f868201ccbdceea3c8f61a11422f256646f245c9acb01acb68787147a91ac93d9058e19acb2071a7acdbf93308d752a83beea3bea4abd7f8d548e758c16f473d3176e61a7541985ac6";

    #[test]
    fn the_shared_vector_opens() {
        use super::*;
        use hpke::OpModeR;
        let unhex = |value: &str| -> Vec<u8> {
            (0..value.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
                .collect()
        };
        let secret =
            <X25519HkdfSha256 as hpke::Kem>::PrivateKey::from_bytes(&unhex(SECRET)).unwrap();
        let wire = unhex(WIRE);
        let (encapped, ciphertext) = wire.split_at(32);
        let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            &secret,
            &<X25519HkdfSha256 as hpke::Kem>::EncappedKey::from_bytes(encapped).unwrap(),
            INFO,
            ciphertext,
            EVENT.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&opened).unwrap(),
            serde_json::json!({"v": 1, "s": "ses_vector", "n": "auth-refactor", "t": "Refactor auth", "k": "approval"})
        );
    }
}
