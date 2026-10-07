//! Bounded HTTP request framing, route authorization, and authority injection.

use super::DevicePermission;
use crate::cli::serve::routes::{route_for, DEVICE_GRANT_HEADER, DEVICE_ID_HEADER};
use anyhow::{anyhow, bail, Context};

pub(super) const MAX_INITIAL_REQUEST: usize = 32 * 1024;

pub(super) fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    device_id: &str,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let header_bytes = &request[..end + 4];
    if header_bytes.iter().enumerate().any(|(index, byte)| {
        (*byte == b'\r' && header_bytes.get(index + 1) != Some(&b'\n'))
            || (*byte == b'\n' && (index == 0 || header_bytes[index - 1] != b'\r'))
    }) {
        bail!("malformed HTTP header line ending");
    }
    if header_bytes
        .split(|byte| *byte == b'\n')
        .skip(1)
        .any(|line| line.starts_with(b" ") || line.starts_with(b"\t"))
    {
        bail!("folded HTTP headers are not permitted");
    }
    let mut header_slots = [httparse::EMPTY_HEADER; 128];
    let mut parsed = httparse::Request::new(&mut header_slots);
    if parsed
        .parse(header_bytes)
        .context("malformed HTTP request")?
        != httparse::Status::Complete(header_bytes.len())
    {
        bail!("incomplete HTTP request headers");
    }
    let method = parsed
        .method
        .ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = parsed.path.ok_or_else(|| anyhow!("missing HTTP target"))?;
    if parsed.version != Some(1)
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for header in parsed.headers.iter() {
        let name = header.name;
        let value = header.value;
        if !name.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
            || !value
                .iter()
                .all(|byte| *byte == b'\t' || (b' '..=b'~').contains(byte))
        {
            bail!("invalid HTTP header characters");
        }
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
            || name.eq_ignore_ascii_case(DEVICE_ID_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
        websocket_upgrade |= name.eq_ignore_ascii_case("upgrade")
            && value.trim_ascii().eq_ignore_ascii_case(b"websocket");
    }
    let framing = request_framing(&request)?;
    if request.len() > framing.total {
        bail!("HTTP pipelining is not permitted through Remote Link");
    }
    if request.len() < framing.buffered {
        bail!("incomplete HTTP request");
    }
    let (spec, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through Remote Link"))?;
    // The framing was chosen from a plain split of the request line before
    // httparse saw it. Both readings must name the same kind of route, or a
    // request could be buffered as one and authorized as the other.
    if spec.streamed_body_limit.is_some() != framing.streamed
        || (framing.streamed && websocket_upgrade)
    {
        bail!("request framing does not match its route");
    }
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    if !token.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
        || !device_id.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
    {
        bail!("invalid proxy authority value");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(method.as_bytes());
    injected.extend_from_slice(b" ");
    injected.extend_from_slice(target.as_bytes());
    injected.extend_from_slice(b" HTTP/1.1\r\n");
    for header in parsed.headers.iter() {
        injected.extend_from_slice(header.name.as_bytes());
        injected.extend_from_slice(b": ");
        injected.extend_from_slice(header.value);
        injected.extend_from_slice(b"\r\n");
    }
    injected.extend_from_slice(b"Authorization: Bearer ");
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
    injected.extend_from_slice(b"\r\n\r\n");
    injected.extend_from_slice(&request[end + 4..]);
    Ok((injected, required))
}

/// How much of one request the proxy holds before authorizing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RequestFraming {
    /// Headers plus the whole declared body.
    pub(super) total: usize,
    /// Bytes buffered before authorization: the whole request for an ordinary
    /// route, the headers alone for a streamed one.
    pub(super) buffered: usize,
    /// Whether the body is relayed after authorization rather than buffered.
    pub(super) streamed: bool,
}

/// Frames one request from its headers.
///
/// Every route but one carries a small JSON body, and the proxy buffers the
/// whole request under `MAX_INITIAL_REQUEST` so it is inspected before a byte
/// reaches the gateway. A route with a `streamed_body_limit` carries a file:
/// its headers are inspected the same way, it must declare a
/// `Content-Length` within that limit, and exactly that many body bytes are
/// relayed afterwards.
pub(super) fn request_framing(request: &[u8]) -> anyhow::Result<RequestFraming> {
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
    let header_len = header_end + 4;
    if let Some(limit) = streamed_body_limit(headers) {
        let Some(&body) = lengths.first() else {
            bail!("a streamed request must declare its Content-Length");
        };
        if body as u64 > limit {
            bail!("request body exceeds the route's limit");
        }
        return Ok(RequestFraming {
            total: header_len + body,
            buffered: header_len,
            streamed: true,
        });
    }
    let total = header_len + lengths.first().copied().unwrap_or(0);
    if total > MAX_INITIAL_REQUEST {
        bail!("initial request exceeds limit");
    }
    Ok(RequestFraming {
        total,
        buffered: total,
        streamed: false,
    })
}

/// The body allowance of the route a request line names, if it streams one.
fn streamed_body_limit(headers: &str) -> Option<u64> {
    let request_line = headers.split("\r\n").next()?;
    let mut parts = request_line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    route_for(method, target)?.0.streamed_body_limit
}
