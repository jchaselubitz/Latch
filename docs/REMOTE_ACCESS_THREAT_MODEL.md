# Remote-access threat model

This document covers Remote Link v1, the only supported Latch remote-access
transport. The historical transport and its pairing records are not accepted
by current clients.

## Assets and allowed locations

| Asset | Allowed location |
| --- | --- |
| Terminal bytes, messages, prompts, commands, paths, and environment | Mac and paired phone only; encrypted in transit |
| Gateway bearer and phone loopback capability | Endpoint owner-only memory/runtime state |
| Endpoint private keys and QR-only enrollment secret | Keychain/Secure Enclave or owner-only Mac storage; never a service |
| Endpoint public keys, opaque device IDs, grant revision, revocation state | Mac plus minimal control-plane directory |
| Opaque room, role, generation, expiry, limits, lease ID | Control plane and relay |
| Coarse audit result and bounded operational counts | Local audit; service audit without content |

The relay and control plane never receive gateway tokens, endpoint private
keys, QR-only enrollment secrets, Noise session keys, terminal data,
transcripts, session names, repository paths, prompt answers, or command text.

## Trust boundaries

1. **Session kernel to loopback gateway.** `latch serve` is bound to loopback
   and uses a short-lived owner-only bearer. It can reach only its own Latch
   process.
2. **Mac authority to transport helper.** The ordinary `latch` process maps an
   already-authenticated peer key to a current local grant and one fixed
   gateway destination. Caller authority headers are removed and replaced
   internally.
3. **Phone app to native transport.** Swift reaches the authenticated Rust
   channel only through a random 256-bit loopback capability. There is no
   manually entered gateway address or token.
4. **Endpoint to endpoint.** Every link performs fresh Noise XX with pinned
   static keys before Yamux or gateway bytes. TLS protects the WSS carrier but
   is not endpoint identity.
5. **Endpoints to relay.** Single-use signed admissions select one opaque room,
   role, purpose, generation, expiry, and limits. Admissions do not authorize
   application actions.
6. **Relay to control plane.** Redemption, lease renewal, and invalidation use
   dedicated service authentication. The relay cannot mint claims or inspect
   encrypted records.

## Enrollment

An operator-minted one-use invitation replaces anonymous owner bootstrap. A
five-minute QR code carries both the relay admission code and an independent
256-bit secret that is never sent to a service. The secret is mixed into the
Noise prologue. Possession of the admission code alone therefore cannot
complete enrollment.

After authenticated key exchange, both endpoints derive comparison words from
the transcript. The unlocked Mac displays the phone name and exact proposed
key; owner approval atomically commits that key, permission, room, and grant
revision. Cancellation, expiry, replay with changed fields, or key
substitution fails closed.

## Authorization and revocation

`observe` reads session/conversation observation surfaces, `interact` adds
structured actions, and `control` adds terminal input and resize. Terminal
access also requires iOS device-owner authentication and always takes the
session's exclusive human surface.

The Mac rechecks current device state every 250 ms on active routes. A
downgrade closes only streams whose route now exceeds the grant; revocation
closes all streams. The control-plane grant revision prevents a stale admission
from restoring old permission, and the durable outbox closes both relay roles.

## Abuse cases and mitigations

| Abuse case | Mitigation |
| --- | --- |
| Malicious control plane substitutes a phone | QR-only secret in the Noise prologue plus transcript comparison and exact-key Mac approval |
| Admission replay | Short expiry, one-time redemption ID, role/purpose binding, generation, bounded attempt ID |
| Compromised relay | Pinned Noise identities and opaque bounded records; no endpoint metadata in claims |
| DNS or TLS interception | Operating-system certificate validation plus pinned endpoint key |
| Browser-origin or local malware probes the adapter | Loopback-only random capability, bounded first request, no caller-selected destination |
| Forged grant headers or route escalation | Strip caller authority, insert current Mac-owned capability, shared route table and live grant check |
| Permission downgrade or revocation during a stream | 250 ms local check, monotonically increasing grant revision, relay room invalidation |
| Connection or memory exhaustion | Admission budgets, one active role per room, frame/stream/header limits, 8 MiB relay backpressure bound |
| Slow/non-draining peer | Deadlines, cancellation propagation, writer eviction, attach cleanup |
| Duplicate application action after reconnect | Existing idempotency and gateway-instance rules; never replay terminal input automatically |
| Sensitive diagnostics | Content-free coarse events only; secrets and application fields rejected mechanically |

## Failure behavior

Authentication and authorization failures are explicit and non-retryable for
that attempt. Transport loss cancels all child streams and requires a fresh
admission and handshake. A lease extension is accepted only when signed,
unexpired, and bound to the current lease. Revocation remains effective when
the relay is temporarily unavailable because the local Mac is authoritative
and invalidation is queued durably.

## Required evidence

Before deployment, verification must cover:

- admission without the QR secret, wrong peer pin, and exact-key enrollment;
- unauthorized and malformed gateway paths, forged authority, oversized
  headers, pipelining, blocked writers, final response completion, and current
  grant enforcement;
- single-use redemption, lease renewal, role replacement, backpressure, and
  authenticated invalidation;
- real PostgreSQL migration/atomicity/privacy tests;
- a real TLS/WSS composed endpoint-to-gateway path;
- Desktop, Swift package, generated XCFramework, and native iOS builds; and
- deployed-service captures and physical-device/network evidence in the later
  deployment objective.

No release is approved if relay or service evidence contains an application
payload, gateway token, QR-only secret, endpoint private key, or Noise session
key.

## Deployed configuration and residual risks

The relay and control plane are single-replica Railway services behind the
platform TLS edge; the relay serves plain WebSocket inside the platform and
the endpoints validate the edge certificate with the operating system trust
store. The edge therefore sees WebSocket framing and the admission bearer in
the upgrade request, but never a Noise plaintext: every record inside the
socket is end-to-end encrypted between the pinned endpoint keys, so a
compromised edge or relay is bounded to traffic analysis and denial of
service. Service secrets (admission signing key, relay service token,
invalidation secret, operator secret, APNs key) live in Railway's variable
store; their rotation and the bounded previous-key overlap are in
[REMOTE_LINK_OPERATIONS.md](REMOTE_LINK_OPERATIONS.md).

Residual risks accepted for this release: a single relay replica is a single
point of availability (not confidentiality); the relay hostname currently
resolves through the platform's edge, so an IPv6-only phone may reach it via
carrier NAT64 rather than native IPv6; attention notifications carry a fixed
sentence and no identifiers, but their timing reveals that some session
changed state; and the Mac's 250 ms grant check bounds, rather than
eliminates, the window in which a just-revoked phone can still receive
bytes.
