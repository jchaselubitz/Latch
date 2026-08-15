# Remote-access threat model

This document covers the remote-access platform described in
[REMOTE_ACCESS_IMPLEMENTATION_PLAN.md](REMOTE_ACCESS_IMPLEMENTATION_PLAN.md).
It is deliberately about the future paired-device transport as well as the
current loopback gateway. `latch serve` is not a public remote server.

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
   only. The relay forwards authenticated opaque frames and cannot terminate
   end-to-end application encryption.
5. **Desktop helper → readiness file.** The readiness document identifies a
   locally bound process but intentionally excludes the gateway token. The
   supervisor provisions an owner-only parent directory; the file is owner-only.

## Authorization and recovery

`observe` can list sessions and read events/terminal output. `interact` adds
structured message and prompt resolution. `control` adds terminal bytes and
resize. New devices never receive control by default. A revoked device is
removed from the Mac allowlist, active streams close, and later handshakes are
rejected. A gateway-token rotation affects new internal handshakes only; that
token is never sent to a phone.

The v1 read-only terminal mode is defence in depth: the gateway ignores input
and resize frames and starts tmux in read-only mode. The desktop transport
still enforces the `observe` permission before the WebSocket is opened.

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
| Terminal-control escalation | Explicit `control` grant; `observe` streams use `mode=read-only`; authorizer checks every terminal operation. |
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
