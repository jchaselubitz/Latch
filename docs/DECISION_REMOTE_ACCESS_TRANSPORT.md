# Remote-access transport decision

Date: 7 September 2026

## Decision

Latch Remote Link uses outbound WebSocket Secure connections on TCP 443 for
internet reachability. A separate opaque relay joins one Mac socket and one
phone socket in a short-lived room and forwards bounded binary records. It
does not receive account IDs, device IDs, endpoint keys, grants, gateway
tokens, paths, session names, or application plaintext.

The same shared Rust core is compiled for macOS and iOS. It performs:

1. platform-trust-store TLS verification for WSS;
2. a fresh `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake with pinned endpoint
   static keys and an explicit Remote Link protocol binding;
3. Yamux multiplexing inside the authenticated encrypted link; and
4. bounded logical byte streams for the existing schema-first gateway.

Both endpoints initiate outbound connections, so Remote Link requires no
public Mac listener, port forwarding, or NAT traversal. An authenticated LAN
carrier races the WSS carrier and uses the identical Noise/Yamux link. Direct
internet optimization is deliberately deferred.

## Ownership

`latch-transport` owns WSS, LAN transport, Noise, Yamux, record limits,
cancellation, heartbeat, and link control. `latch-remote` drives that core,
obtains fresh admissions and lease extensions, and bridges authenticated
logical streams to the Mac authority.

The ordinary `latch` crate does not depend on `latch-transport`. It owns the
Mac identity, exact per-device grants, audit records, and one supervised
loopback gateway. It accepts only a peer key and grant revision already
authenticated by the link core, strips client-supplied authority headers,
inserts the current internal capability, and rejects routes outside the shared
allowlist.

On iOS, the Rust core is required through `latch-transport-ffi`. Swift exposes
the existing `GatewayTransport` contract through a random 256-bit,
loopback-only capability. A caller cannot choose a gateway host or bearer
token.

## Enrollment and admission

The control plane opens a five-minute enrollment and returns a service-visible
admission code plus a separate 256-bit enrollment secret placed only in the QR
payload. The endpoints mix the QR-only secret into the enrollment Noise
prologue. Possessing or substituting the admission code without that secret
cannot complete the handshake.

After the encrypted endpoints derive the same comparison words, the Mac shows
the phone name and proposed key. Owner approval atomically commits that exact
key, permission, link room, and grant revision locally and in PostgreSQL.
There is no anonymous account bootstrap: Desktop first exchanges an
operator-minted, one-use owner invitation.

Normal admission claims contain only an opaque room, role, purpose, generation,
expiry, one-time ID, and resource limits. The relay redeems them with the
control plane, accepts each once, and renews a room only from a signed
lease-extension claim forwarded over the WSS control channel. Revocation and
permission downgrades increment durable grant state and enqueue relay room
invalidation.

## Authorization

Noise authentication proves the pinned device key; it does not grant an
operation. Every logical stream is checked against the Mac's current record.
Permissions remain `observe`, `interact`, and `control`. Terminal access
requires `control` and takes the session's one human surface. A live
downgrade or revocation closes any stream whose current route grant is no
longer satisfied.

## Rejected alternatives

| Alternative | Reason |
| --- | --- |
| Public `latch serve` with TLS | Exposes a bearer gateway and weakens per-device revocation and fixed-destination enforcement. |
| SSH or VPN as the primary product | Requires user-managed network configuration; remains an advanced local option. |
| Custom UDP/NAT traversal | Adds protocol and operational risk without being required for reachability. |
| Platform-specific cryptography or multiplexers | Creates divergent trust boundaries and wire behavior. |
| Bundled TLS roots | Can drift from iOS/macOS trust policy; Remote Link uses platform verification. |
| Mixed old/new compatibility | The owner approved a coordinated clean replacement and re-pairing. |

## Operational consequences

The relay and control plane remain independent deployables. For this release,
each runs exactly one always-awake replica. The hosting edge must preserve the
WebSocket `Authorization` header and keep idle sockets longer than the
15-second heartbeat.

Deployed shape (8 September 2026): both services run on Railway in project
`latch`, one replica each with app sleeping disabled; the control plane at
`latch-production-7e52.up.railway.app` deploys from `main`, the relay at
`latch-relay-production.up.railway.app` from `services/relay` on the same
branch. Configuration, verification checks, and the deployment, revocation,
key-rotation, incident, and rollback runbooks are in
[REMOTE_LINK_OPERATIONS.md](REMOTE_LINK_OPERATIONS.md). The physical matrix
and its measured results are in
[REMOTE_ACCESS_FIELD_VERIFICATION.md](REMOTE_ACCESS_FIELD_VERIFICATION.md);
this decision is implemented in source and locally verified, and is
considered released only when that record's gates are met.
