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
  credentials while leaving LAN/direct access available. It is a refusal at
  issuance, not a client-side ordering rule: relay candidates are now offered
  to the first attempt, and preferring a direct path is left to ICE's own pair
  priority, which ranks host and reflexive candidates above relayed ones.
  Withholding TURN until a direct attempt had failed bought no stronger
  preference and cost every genuinely symmetric-NAT phone a guaranteed second
  round trip.
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

The repository test suites provide deterministic protocol coverage, now
including NAT behaviour against simulated networks with a real TURN server.
Rows that need a physical network are called out as not yet run rather than
being represented as real-world measurements; how they are run and recorded is
in [REMOTE_ACCESS_FIELD_VERIFICATION.md](REMOTE_ACCESS_FIELD_VERIFICATION.md).

| Area | Automated evidence | Result |
| --- | --- | --- |
| Pairing expiry/replay/cancellation and secret-at-rest | Rust remote-access plus LatchMobileKit pairing tests | Pass |
| Presence, rendezvous, ICE candidate validation, and TURN issuance | Control-plane, desktop-host, and LatchMobileKit signaling tests | Pass |
| Paired-phone flow: pair, signal, pinned Noise, list, send, then revoke | LatchMobileKit composed end-to-end test; Rust LAN proxy and ICE record round trips | Pass in deterministic test environment |
| Noise identity pin | Rust transcript vectors and LatchMobileKit Noise tests; the pin is the paired record, never the rendezvous claim | Pass |
| Observe/interact/control routing, prompt/message/key distinctions | Rust proxy tests plus gateway/SDK suites | Pass |
| Session discovery, structured events, prompt resolution | gateway integration and Remote SDK tests | Pass |
| Read-only and controlling terminal modes | Rust gateway integration and Remote SDK tests | Pass |
| Duplicate submission prevention | gateway idempotency integration tests | Pass |
| Direct setup, reconnect, path migration | in-process ICE host-candidate and policy tests | Pass in deterministic test environment |
| Path selection is recorded and readable | Mac audit-trail counters in the diagnostics bundle and phone-side counters, both content-free; unit tests on both | Pass |
| TURN fallback, end-to-end secrecy, credential expiry | Prefer-direct policy and Noise tests; simulated-NAT relay selection against a real in-process TURN server; no deployed relay soak | Pass for policy, cryptography, and path selection; deployed-relay evidence deferred |
| Immediate revocation and device-key rotation | Rust remote-access tests | Pass |
| Terminal grant separate from base access; permission downgrade closes a live terminal stream | Rust `proxy_connection` downgrade test (`permission_downgraded` audit row), LatchDesktop `RemoteAccessTests`, `SessionRouteTests` | Pass |
| Phone-side Face ID/passcode gate before a terminal opens; chat stays ungated | LatchMobileKit `TerminalUnlock`/`AppModel` tests | Pass at the unit level; the Face ID prompt itself needs a physical device — see below |
| Idle terminal release while backgrounded | LatchMobileKit `AppModel` idle-countdown tests | Pass |
| ICE responder in the helper (offer → `latch-noise-v1` data channel → Noise handshake → proxy) | Rust integration test over `webrtc`'s in-memory network; presence candidate ordering tests | Pass in deterministic test environment |
| Relay-from-start policy (STUN + TURN gathered together, relay a legitimate first-attempt outcome) | latch-transport and latch-transport-ffi policy tests, LatchMobileKit `GatewayTransportTests` | Pass |
| `CloudflareTurnProvider` parses both the documented object shape and an `iceServers` array | `services/control-plane/src/cloudflare-turn.test.ts` | Pass |
| Never-relay backed by presence host-candidate filtering; Tailscale/tailnet host candidate usable as a plain TCP Noise target | Desktop `presenceCandidates(neverRelay:)` tests, phone-side direct-target tests | Pass |
| Sleep prevention while a phone is connected | `SleepAssertion` unit coverage | Pass |
| Loopback isolation and stale gateway credential | Rust gateway/supervision tests | Pass |
| Parser, timeout, quota, symlink, and interrupted-write failures | Rust hardening tests | Pass |
| Same-LAN path on the CI host | Authenticated listener Noise round trip, post-handshake lifetime, and revocation-close test | Pass |
| Symmetric NAT forces the relay; a cone NAT does not | Virtual WAN, two LANs behind configurable NATs, and a real in-process TURN server: cone/cone nominates a reflexive pair, symmetric/symmetric nominates a relayed one, records round-trip on both | Pass against simulated NATs; no carrier or venue network involved |
| Off-LAN prerequisites: the helper gathers against STUN, presence outlives the first candidate window, and an offer reaches the helper inside the phone's ICE budget | Desktop `RemoteAccessTests`/`ControlPlaneHostTests` (STUN flags, relay refusal, lifetime re-stamp, bounded wait); `latch-remote` idle re-gather test; control-plane long-poll tests; phone `PairedRouteTests` (concurrent dial, remembered route, fast refusal) | Pass |
| Home IPv4 NAT, IPv6, cellular-to-home, double/CGNAT | No physical run: no phone paired to a Mac running an ICE-capable helper during this objective | Not yet run — see below |
| Hotel/corporate Wi-Fi with UDP blocked (TURN over TLS 443) | No physical run; the relay path is exercised only against the simulated NATs above | Not yet run — see below |
| Wi-Fi to cellular migration mid-terminal, Mac sleep/wake, phone background/foreground | No physical run; reconnect and the sleeping-Mac message are covered by phone-side unit tests only | Not yet run — see below |
| Captive portal, latency/loss/reordering, logout | State/failure injection only | Deferred preproduction evidence |
| Sustained regional relay availability and latency SLO | No deployed relay/load environment in this repository | Deferred preproduction evidence |
| External security review | No independent reviewer attached to this objective | Required release gate |

## The rows that are not yet run

Four rows above say "not yet run" rather than "deferred preproduction
evidence". The change of wording is the point: the obstacle is no longer that
the infrastructure does not exist. The instrumentation, the procedure, and the
recorder exist, and are described in
[REMOTE_ACCESS_FIELD_VERIFICATION.md](REMOTE_ACCESS_FIELD_VERIFICATION.md).
What is missing is a person, a phone, and the networks.

Two specific things blocked the run during this objective, and both are worth
knowing before someone attempts it:

- The helper installed on the development Mac predates the ICE responder
  entirely — it has no `--ice-server` flag and its readiness document carries no
  ICE credentials — so a phone pointed at it falls back to Bonjour and then
  fails. That failure looks like a network result and is not one. Installing
  the current helper and toggling Remote Access to relaunch it is a
  prerequisite, not a detail.
- The helper reads the Mac identity from the Keychain, which prompts, so it
  cannot be launched from a headless shell. The relaunch has to happen in a
  desktop session.

Before mission `coo:897` a physical run could not have passed even with the
current helper installed, and it is worth recording why, because each of the
three defects looked like a network result from the phone:

- The app launched the helper with no `--ice-server`, so the Mac gathered
  private host candidates only and never published a reflexive address a phone
  off its network could pair with.
- The helper stamped its candidates with a 90-second lifetime once, at
  launch, and the app republished that stamp verbatim; from the second window
  on the control plane refused every presence refresh, and the phone read the
  Mac as offline.
- The app collected rendezvous offers as a side effect of the presence
  refresh, every 30 seconds, while the phone abandoned its ICE attempt after
  six. The offer reached the helper after the phone had already given up.

All three are fixed in the app, the helper, and the control plane, with the
phone's ICE budget raised to fifteen seconds and offers now collected by a
long-polled loop. The physical rows remain to be run.

Once a run happens, `scripts/field-run.sh` records it into `docs/field-runs/`
and `scripts/field-run.sh matrix` regenerates the table from what was actually
recorded. These rows should be replaced with that output rather than with
prose.

## Residual release gates

The paired phone client and its transport boundary are implemented, but
production rollout remains gated on all of the following:

- an independent security review;
- Apple signing and notarization in the release pipeline; and
- a deployed Cloudflare TURN relay with measured load, availability, and soak
  results.

Physical NAT traversal, cellular, captive-portal, sleep/wake, and sustained
relay-availability evidence are also still outstanding — now as unrun
procedures rather than as absent capability. The Face ID/passcode terminal
gate is likewise unverified on real biometric hardware, since the simulator
has none to exercise; the permission logic it wraps is covered by unit tests
only. Remote access and relay must remain off by rollout policy until those
external gates are satisfied. These are deployment evidence gaps, not
permissions for the phone client to bypass the documented boundaries.
