//! Bearer-token minting and request authentication.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::Context;
use axum::http::{header, HeaderMap, HeaderValue};

use crate::session::paths::{DIR_MODE, FILE_MODE};

const TOKEN_BYTES: usize = 32;
const SUBPROTOCOL_PREFIX: &str = "latch.v1.";

/// Creates or replaces the serve token and returns the new value.
pub fn mint_token(path: &Path) -> anyhow::Result<String> {
    if let Some(parent) = path.parent() {
        if parent != Path::new("") {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE))
                .with_context(|| format!("cannot tighten {}", parent.display()))?;
        }
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    OpenOptions::new()
        .read(true)
        .open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .context("cannot read /dev/urandom")?;
    let token = hex_encode(&bytes);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(token)
}

/// Reads the current token, ignoring surrounding whitespace.
pub fn load_token(path: &Path) -> anyhow::Result<String> {
    let token =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(token.trim().to_owned())
}

/// Token presented on an HTTP or WebSocket handshake.
pub fn presented_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value.to_str().ok()?.trim();
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))?;
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_owned());
        }
    }
    subprotocol_token(headers)
}

/// The `latch.v1.<token>` subprotocol, when the client offered one.
pub fn selected_subprotocol(headers: &HeaderMap) -> Option<String> {
    parse_protocols(headers.get(header::SEC_WEBSOCKET_PROTOCOL)?)
        .into_iter()
        .find(|protocol| protocol.starts_with(SUBPROTOCOL_PREFIX))
}

/// Constant-time compare of two token strings.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let left = expected.as_bytes();
    let right = presented.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Loopback-bound servers reject non-loopback browser origins.
pub fn origin_allowed(origin: Option<&HeaderValue>, bind_is_loopback: bool) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if origin.eq_ignore_ascii_case("null") {
        return true;
    }
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let host = uri.host().unwrap_or("");
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host.eq_ignore_ascii_case("[::1]");
    if bind_is_loopback {
        loopback
    } else {
        true
    }
}

fn subprotocol_token(headers: &HeaderMap) -> Option<String> {
    selected_subprotocol(headers)
        .and_then(|protocol| protocol.strip_prefix(SUBPROTOCOL_PREFIX).map(str::to_owned))
        .filter(|token| !token.is_empty())
}

fn parse_protocols(value: &HeaderValue) -> Vec<String> {
    value
        .to_str()
        .unwrap_or("")
        .split(',')
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{origin_allowed, token_matches};
    use axum::http::HeaderValue;

    #[test]
    fn token_compare_rejects_length_mismatch_and_empty_expected() {
        assert!(!token_matches("", "abc"));
        assert!(!token_matches("abc", "ab"));
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
    }

    #[test]
    fn loopback_origin_policy() {
        assert!(origin_allowed(None, true));
        assert!(origin_allowed(
            Some(&HeaderValue::from_static("http://127.0.0.1:3000")),
            true
        ));
        assert!(origin_allowed(
            Some(&HeaderValue::from_static("http://localhost:5173")),
            true
        ));
        assert!(!origin_allowed(
            Some(&HeaderValue::from_static("https://evil.example")),
            true
        ));
        assert!(origin_allowed(
            Some(&HeaderValue::from_static("https://phone.example")),
            false
        ));
    }
}
