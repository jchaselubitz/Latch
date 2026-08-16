# Remote access Phase 4 hardening and validation

This document is the production-boundary record for remote access, including
the paired iPhone client. The public gateway remains loopback-only; LAN and
ICE/TURN adapters terminate mutually authenticated encryption before reaching
a fixed supervised gateway destination. This is implementation and
deterministic-test evidence, not a declaration that the feature is ready for
production rollout.

## Hardened local state and recovery

- macOS stores the Mac Noise private key as a generic password in Keychain
  under `co.cooperativ.latch.remote-access`; `identity.json` contains public
  metadata only. Existing Phase 1 identity files migrate on first use. Headless
  non-macOS builds use a separate owner-only `identity.key` file so CI can test
  the same metadata boundary.
- Pairing records store a domain-separated SHA-256 digest, never the QR secret.
  Pairing is one-time, expires after five minutes, and is capped at eight
  pending requests and 32 paired devices.
- JSON state and the audit trail are written to a private temporary file,
  synced, and atomically renamed. Reads reject symlinks, non-files, and
  group/world-readable state. This makes interrupted updates recover to either
  the previous or new complete document.
- Disabling remote access cancels pending pairings and removes supervised
  gateway readiness/token files. Revocation remains immediate for active LAN
  streams through the 250 ms device-state check.
- `latch remote-access rotate-device-key DEVICE --public-key KEY` replaces a
  phone Noise key while retaining its device identity and grants. The old key
  stops authenticating immediately. A revoked device cannot use rotation as a
  recovery path.

Account recovery is intentionally outside the local protocol: this repository
does not yet contain an account service. Loss of the Mac Keychain item requires
enabling a fresh Mac identity and explicitly approving devices again. No
control-plane recovery mechanism is allowed to synthesize device grants.

## Operational controls and privacy

- `latch remote-access disable` is the global incident switch.
- `latch remote-access relay disable` independently prevents new relay
  credentials while leaving LAN/direct access available. The first attempt is
  direct with STUN only; TURN credentials are requested only after that attempt
  fails.
- LAN handshakes have a ten-second deadline and a 32-connection cap. The
  Cloudflare TURN fallback uses short-lived service credentials and the
  control-plane device rate limit; its availability and capacity are a release
  gate, not a local quota claim. Noise records remain length-bounded, and
  duplicate endpoint admission is rejected.
- The audit trail retains at most 1,024 events or 512 KiB. It records only a
  timestamp, coarse event, opaque device id when necessary, and result.
- `latch remote-access diagnostics` produces an inspectable local JSON bundle
  containing switches, device counts, and coarse event counts. It includes no
  names, endpoint addresses, keys, tokens, repository/session identifiers, or
  application content, and it never uploads automatically.
- The proxy rejects caller-supplied authorization, transfer encoding,
  ambiguous content lengths, malformed/folded headers, encoded traversal, and
  pipelined requests. Non-WebSocket HTTP is forced closed after one authorized
  request so a lower permission cannot smuggle a second operation on the same
  connection. WebSocket authorization is fixed by its initial terminal/events
  route and gateway-enforced mode.

The existing desktop updater remains the only update installation boundary. It
checks the release manifest, archive digest, code-signing team identity, and
Gatekeeper before replacement; remote-access code does not download or install
updates and has no privileged helper.

## Incident runbook

1. Suspected relay incident: run `latch remote-access relay disable`, export
   diagnostics, and preserve the bounded audit file. Direct/LAN access remains
   available.
2. Suspected device compromise: run `latch remote-access revoke DEVICE`.
   Existing LAN transport is closed within the revocation polling interval and
   new LAN/rendezvous/relay authorization must reject the device.
3. Suspected Mac compromise: run `latch remote-access disable`, revoke devices,
   rotate the internal gateway token by restarting supervision, and rotate the
   Mac identity after host recovery. Re-enable and approve devices explicitly.
4. Control-plane outage: use the documented SSH tunnel/private-network
   advanced mode. Never expose the plaintext gateway directly.

## Verification matrix

The repository test suites provide deterministic protocol coverage; physical
network rows require preproduction infrastructure and are called out rather
than being represented as real-world measurements.

| Area | Automated evidence | Result |
| --- | --- | --- |
| Pairing expiry/replay/cancellation and secret-at-rest | Rust remote-access plus LatchMobileKit pairing tests | Pass |
| Presence, rendezvous, ICE candidate validation, and direct-first TURN issuance | Control-plane, desktop-host, and LatchMobileKit signaling tests | Pass |
| Paired-phone flow: pair, signal, pinned Noise, list, send, then revoke | LatchMobileKit composed end-to-end test; Rust LAN proxy and ICE record round trips | Pass in deterministic test environment |
| Noise identity pin | Rust transcript vectors and LatchMobileKit Noise tests; the pin is the paired record, never the rendezvous claim | Pass |
| Observe/interact/control routing, prompt/message/key distinctions | Rust proxy tests plus gateway/SDK suites | Pass |
| Session discovery, structured events, prompt resolution | gateway integration and Remote SDK tests | Pass |
| Read-only and controlling terminal modes | Rust gateway integration and Remote SDK tests | Pass |
| Duplicate submission prevention | gateway idempotency integration tests | Pass |
| Direct setup, reconnect, path migration | in-process ICE host-candidate and policy tests | Pass in deterministic test environment |
| TURN fallback, end-to-end secrecy, credential expiry | Direct-first policy and Noise tests; no relay soak environment | Pass for policy/cryptography; deployment evidence deferred |
| Immediate revocation and device-key rotation | Rust remote-access tests | Pass |
| Loopback isolation and stale gateway credential | Rust gateway/supervision tests | Pass |
| Parser, timeout, quota, symlink, and interrupted-write failures | Rust hardening tests | Pass |
| Same-LAN path on the CI host | Authenticated listener Noise round trip, post-handshake lifetime, and revocation-close test | Pass |
| Home IPv4 NAT, IPv6, cellular-to-home, double/CGNAT | Protocol simulation only; no physical lab attached | Deferred preproduction evidence |
| Symmetric NAT / UDP blocked | TURN retry policy only; no physical or deployed-relay run | Deferred preproduction evidence |
| Captive portal, latency/loss/reordering, sleep/wake/logout | State/failure injection only | Deferred preproduction evidence |
| Sustained regional relay availability and latency SLO | No deployed relay/load environment in this repository | Deferred preproduction evidence |
| External security review | No independent reviewer attached to this objective | Required release gate |

## Residual release gates

The paired phone client and its transport boundary are implemented, but
production rollout remains gated on all of the following:

- an independent security review;
- Apple signing and notarization in the release pipeline; and
- a deployed Cloudflare TURN relay with measured load, availability, and soak
  results.

Physical NAT traversal, cellular, captive-portal, sleep/wake, and sustained
relay-availability evidence are also still deferred. Remote access and relay
must remain off by rollout policy until those external gates are satisfied.
These are deployment evidence gaps, not permissions for the phone client to
bypass the documented boundaries.
