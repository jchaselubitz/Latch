# Remote-access threat model

This document covers the paired-device remote-access platform described in
[REMOTE_ACCESS_IMPLEMENTATION_PLAN.md](REMOTE_ACCESS_IMPLEMENTATION_PLAN.md).
`latch serve` is not a public remote server.

## Assets and classification

| Asset | Classification | Allowed location |
| --- | --- | --- |
| Terminal bytes, messages, prompts, commands, cwd, environment | highly sensitive content | Mac and paired client only, encrypted in transit |
| Gateway bearer token | internal-hop secret | owner-only Mac storage and the loopback gateway only |
| Device private keys and pairing secret | authentication secret | Keychain/Secure Enclave or owner-only Mac storage only |
| Device public keys, opaque device IDs, revocation state | sensitive metadata | Mac and minimal control plane |
| Presence, path type, aggregate byte counts, coarse errors | operational metadata | local audit log; opt-in diagnostic upload only |

The relay and control plane never receive terminal bytes, transcript content,
gateway tokens, device private keys, endpoint session keys, session names,
repository paths, or prompt answers.

## Trust boundaries

1. **Local session kernel → loopback gateway.** The gateway has the existing
   bearer token and may reach only its own Latch process. It remains bound to
   loopback; a remote peer cannot select an arbitrary local TCP destination.
2. **Gateway → desktop remote-access agent.** The agent is the policy
   enforcement point. It maps a mutually authenticated device key to a local
   authorization record before forwarding an allowed `/v1` operation.
3. **Paired device → encrypted transport.** A device identity is authenticated
   during every connection. Authorization is checked on connection and on each
   privileged operation; a client-provided device ID is never authoritative.
4. **Endpoints → rendezvous/relay.** The control plane coordinates candidates
   only. Cloudflare Realtime TURN is the named third-party relay: it sees
   ciphertext, endpoint network metadata, sizes, and timing, but cannot
   terminate the Noise encryption or read application content. DTLS is
   transport encryption required by SCTP, not the paired-device identity;
   Noise XX above the data channel proves the static key pinned during pairing.
5. **Desktop helper → readiness file.** The readiness document identifies a
   locally bound process but intentionally excludes the gateway token. The
   supervisor provisions an owner-only parent directory; the file is owner-only.

## Authorization and recovery

`observe` can list sessions and read the available observation surfaces.
`interact` adds structured message and prompt resolution. `control` adds
terminal bytes and resize. A direct CLI `pair confirm` defaults to `interact`,
while Latch Desktop's approved pairing flow grants `control` so its terminal
switch begins enabled. A revoked device is removed from the Mac allowlist,
active streams close, and later handshakes are rejected. A gateway-token
rotation affects new internal handshakes only; that token is never sent to a
phone.

`control` is granted as a separate "Allow terminal" decision layered on top of
the base `observe`/`interact` picker, not as the top notch of a single
severity ladder: the Mac remembers what a device held underneath the grant, so
turning the terminal off returns the device to Interact or Observe as it was
rather than to a default. A grant is written to the local device store first —
the store the helper actually enforces against — and then mirrored to the
control-plane pairing row; a mirror failure is reported but never rolls the
local grant back, because the Mac is the authority and the directory is a
convenience for the phone's own UI.

Revocation and a permission *downgrade* are both enforced by the same 250 ms
device-state check in `proxy_connection` (`crates/latch/src/cli/remote_access.rs`).
The check compares the device's live permission against the grant the
connected route actually required, not against the grant held at handshake
time, so a device dropped from `control` to `interact` mid-session loses its
terminal and keeps its chat connection — the terminal route's own requirement
is what closes, not the whole pairing. The audit trail records a
`permission_downgraded` event distinct from revocation so the two are
distinguishable after the fact.

On the phone, opening a terminal is additionally gated behind the device
owner: `TerminalUnlock` runs `LAContext` with
`deviceOwnerAuthentication` (Face ID or Touch ID, passcode fallback — never a
refusal for a device with no biometric enrollment) before a `TerminalSession`
is returned, and caches one passed check for a five-minute grace window so
repeated attach/detach within that window costs one prompt rather than one per
attach. A phone with no passcode set is refused outright rather than waved
through. This is a client-side gate on top of the Mac's `control` grant, not a
replacement for it: a stolen unlocked phone still needs the terminal grant to
have been given, and a phone that has the grant but fails the device-owner
check gets no terminal. Chat is deliberately not gated the same way — a lost
or stolen phone still needing Face ID to read a conversation would be a
different, and here unwanted, tradeoff.

A terminal surface a phone is holding is also released unilaterally by the
phone after inactivity: `AppModel` releases a held terminal after two minutes
with no input while the app is not the frontmost app (backgrounding outright
releases it immediately). This bounds how long a phone that is not actively
being watched — a notification, an incoming call, the app switcher, the Face
ID prompt itself — can keep the Mac's one terminal surface parked away from
whoever is actually at the keyboard. It is a phone-side liveness cleanup, not
a security boundary: the Mac's revocation and downgrade checks above remain
the authoritative enforcement point.

There is no read-only terminal mode to fall back on. A terminal connection is
the session's single exclusive surface, so the gateway requires the `control`
grant for the terminal route and refuses `observe` and `interact` before the
WebSocket is opened. Observation without control is served by the conversation
socket, which cannot take the surface or type into a pane, and by
`GET /v2/sessions/{id}/preview`.

That preview is not the read-only terminal this rule denies. It is a
`capture-pane` query — one read of the pane's cells at one instant, the same
kind of read the conversation connector already performs to observe a screen —
so it enters no attach, paints no second surface, follows nothing, and carries
no input. `observe` therefore permits it. It does widen what an `observe`
device can read: the pane's rendered screen, and up to 200 lines of
primary-screen history, which the conversation projection does not expose
verbatim. That is deliberate and bounded — the same session content the grant
already entitles the device to read through the conversation socket, in the
form the pane holds it — and it is capped, deadline-bounded, and forced to zero
history while a full-screen application owns the pane.

Two further limits keep a terminal connection from being used as a denial of
service against the session itself. The steal only commits once the socket has
declared a real terminal size, so an unauthenticated or half-initialised
socket cannot evict the desk surface. And a peer that stops draining output is
closed and its attach reaped, rather than being allowed to hold the surface
while the pane stalls behind it.

## Abuse cases and mitigations

| Abuse case | Required mitigation and recovery |
| --- | --- |
| Stolen/replayed QR material | Single-use, high-entropy pairing secret; five-minute expiry; confirmation on unlocked Mac; consume on success or cancellation. |
| Stolen unlocked phone | Local device authentication before a new connection and sensitive operation; per-device revocation immediately closes streams. |
| Compromised relay/control plane | Pin paired endpoint keys; transcript-bound authenticated key agreement; end-to-end encryption before relay application data; retain no content on the service. |
| DNS, certificate, or LAN impersonation | Verify the paired Mac identity on every local/direct path; Bonjour is discovery only, never authorization. |
| Browser-origin attack on gateway | Keep loopback binding, reject non-loopback origins, require bearer authentication, and do not expose the token to remote clients. |
| Confused deputy to another local service | The remote agent has one fixed loopback target and an allowlisted `/v1` surface; no host/port supplied by a device is ever dialed. |
| Duplicate submit after reconnect | `Idempotency-Key` binds one message or resolve payload to one resolved session for the gateway instance's bounded retry window. Reuse with different content returns 409. |
| Terminal-control escalation | Explicit `control` grant on the terminal route, with no lesser terminal mode to downgrade into; authorizer checks every terminal operation. |
| Permission downgraded mid-session (not just revoked) | The 250 ms device-state check compares against the route's required grant, not the grant held at handshake, and closes a live terminal stream the moment `control` is lost while leaving a lesser-permission stream (e.g. chat) open. |
| Stolen unlocked phone reaches the terminal | `TerminalUnlock` requires `LAContext` device-owner authentication before a `TerminalSession` opens, independent of the Mac's `control` grant; a phone with no passcode is refused. |
| Phone left connected and unattended holds the Mac's terminal | The idle countdown releases a held terminal surface after two minutes with no input while the app is not frontmost; backgrounding releases it immediately. |
| Surface denial of service | A steal commits only after a valid size is declared; socket writes are deadline-bounded and a non-draining peer is evicted and its attach reaped, so a stalled device cannot hold the surface or block the pane. |
| Connection exhaustion | Per-device/account connection, frame, request, and buffered-byte limits; reject before proxying; audit aggregate failure category only. |
| Hostile terminal output | Treat output as terminal bytes, never markup; use a hardened renderer and avoid putting content in diagnostics, notifications, or logs. |
| Update/dependency compromise | Signed/notarized helper, signed update metadata, pinned dependency review, and rollback/runbook before production rollout. |

## Failure behavior

Authentication/authorization failures are explicit and non-retryable. Network
interruptions reauthenticate, rediscover capabilities, resume events from the
last acknowledged cursor, and reattach a terminal from its current screen.
Clients must not automatically replay terminal input or `keys`. They may retry
a message or prompt resolution only with the same idempotency key and only
while the observed `gatewayInstanceId` is unchanged. A changed instance ID
means the in-memory v1 retry window was lost; the client must refresh state and
ask for user confirmation rather than guess whether a prior submission ran.

## Security validation required before release

Phase 4 must exercise pairing replay, identity substitution, revocation during
an active stream, direct and forced-relay captures, malformed framing, rate
limits, lock/sleep/network changes, update failures, and log/crash-report
redaction. No release is approved if a relay capture or service log reveals a
gateway token, terminal byte, transcript, message, prompt answer, or endpoint
decryption key.
