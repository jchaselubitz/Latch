# Decision: remote-access transport and contract boundary

**Status:** accepted; implementation lives in `crates/latch-transport` and is
distributed to iOS as the owned `LatchTransportFFI.xcframework`.

## Decision

Use a standards-based **WebRTC data-channel transport with ICE/STUN/TURN** for
direct and relay connectivity. Both endpoints compile the same pinned Rust
core rather than integrating unrelated platform stacks. TURN is used only as
an opaque network relay. Latch adds endpoint-authenticated application
encryption and binds it to the paired device identities before any `/v1` bytes
are carried.

The core composes `webrtc-ice`, `webrtc-dtls`, `webrtc-sctp`, and
`webrtc-data` directly, below `RTCPeerConnection` and SDP. DTLS supplies the
encryption SCTP requires but is not an identity boundary: its self-signed
certificate is deliberately not the pairing pin. Noise XX runs above the one
reliable ordered data channel and verifies the static key against
`PairedDeviceRecord.mac.publicKey` before gateway traffic is sent.

Direct-first is enforced by information flow, not candidate preference. The
first ICE agent receives STUN entries only. Only after that attempt records a
failure may the client request and supply Cloudflare TURN credentials to a
fresh agent. A relay-to-direct recovery or any other selected-pair path change
gates application traffic until capability discovery has run again.

The Latch application layer depends only on the authenticated stream boundary
below. It does not know whether the selected path is `local`, `direct`, or
`relay`; a transport adapter exposes that path only as non-content diagnostic
metadata.

```text
paired identity + authorization
             │
             ▼
AuthenticatedByteStream ── byte-preserving /v1 proxy ── loopback latch serve
        local | direct | relay                         fixed destination only
```

## Boundary contract

Before a stream is handed to the desktop proxy, an adapter must provide:

- a mutually authenticated `peerId` derived from the paired public key;
- the Mac-owned permission set (`observe`, `interact`, `control`);
- an opaque `connectionId`, `transportMode`, and non-content diagnostics;
- ordered, reliable bytes with backpressure, explicit close/error categories,
  and bounded buffering;
- immediate closure when the Mac revokes the peer; and
- an endpoint encryption transcript bound to both device identities, the
  connection role, and negotiated protocol version.

The proxy maps those permissions to the fixed gateway paths. It does not
accept a target host, port, gateway token, or a claimed identity from stream
payload. `observe` selects the read-only terminal mode; `control` is required
for terminal input or resize.

WebRTC data channels are message-oriented, so the adapter uses bounded
length-prefixed records internally and presents ordered byte-stream semantics
above that framing. This preserves the existing HTTP/WebSocket `/v1` surface
without inventing a second terminal protocol.

## Alternatives considered

| Alternative | Decision |
| --- | --- |
| Expose `latch serve` with TLS | Rejected: bearer-token gateway remains an internal plaintext loopback hop and would make revocation/device authorization harder. |
| SSH/VPN only | Retained as advanced fallback, rejected as the primary product because it requires user network configuration. |
| Network.framework alone | Useful for local paths, but it does not provide the required interoperable ICE/TURN direct-connect path. |
| Custom UDP/NAT traversal | Rejected: too much unaudited protocol and operational risk. |
| WebRTC data channels | Chosen: mature ICE/STUN/TURN connectivity, native Apple support, direct path selection, and TURN fallback beneath a transport-neutral adapter. |

## Canonical v1 contract

`schemas/remote-access/v1/` is the source of truth for additions to `/v1` and
the private supervision handoff. The v1 bundle defines gateway discovery,
readiness, terminal modes, send bodies, and idempotency-key syntax. The
committed generator creates Rust and TypeScript representations; CI runs it in
`--check` mode. Fixtures under `fixtures/remote-access/v1/` are wire examples,
not implementation snapshots.

Compatibility rules:

1. A v1 server never removes, renames, or changes the meaning of a documented
   field, endpoint, WebSocket frame, close code, or accepted control mode.
2. New optional behavior is additive and must be advertised by
   `GET /v1/capabilities.features`; clients do not probe a failing endpoint to
   infer it.
3. A client receiving discovery 404 uses the existing terminal-only legacy
   surface. A protocol major other than 1 is unsupported rather than guessed.
4. `Idempotency-Key` is optional for compatibility and valid only for
   `message` and `resolve`. A supplied key is scoped to the resolved session
   and payload. Completed results are retained for ten minutes or 1,024 keys,
   whichever limit is reached; duplicate in-flight calls wait for the original
   result. The cache is intentionally in-memory and `gatewayInstanceId`
   changes on a restart.
5. `mode=read-only` is additive; missing mode remains `control` so existing
   CLI and Remote SDK terminal behavior is unchanged.

## Swift generation path

The native iOS/macOS module should not hand-copy the TypeScript model. At the
start of the native client objective, convert the JSON Schema bundle into the
`components.schemas` section of an OpenAPI 3.1 document (retaining the exact
canonical `$id` values), then run [Swift OpenAPI Generator](https://github.com/apple/swift-openapi-generator)
for Codable client models. The adapter-facing stream types remain hand-written
because they are platform transport interfaces, not HTTP documents. CI should
compare the exported OpenAPI component schemas with `schemas/remote-access/v1/`
and run the same fixture corpus through Swift `JSONDecoder`.

## Service-level targets

Phase 1 records connection attempts, selected path, reconnect time, relay
latency, and crash-free sessions as aggregate non-content metrics. Pre-release
targets are set only after the LAN/direct/relay network matrix has measured a
representative baseline; no guessed SLO is treated as a product commitment.
