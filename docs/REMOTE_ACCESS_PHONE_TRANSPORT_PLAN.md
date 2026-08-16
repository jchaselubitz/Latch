# Remote access: phone transport plan

**Status:** design accepted for implementation; supersedes nothing, and adds
the missing connecting piece between `docs/REMOTE_ACCESS_IMPLEMENTATION_PLAN.md`
(what the product should be), `docs/DECISION_REMOTE_ACCESS_TRANSPORT.md` (the
transport boundary contract), and `docs/REMOTE_ACCESS_PHASE_4.md` (what the
headless platform hardened). It is the plan for making a paired iPhone reach
the Mac's gateway, which is the one thing none of those documents has yet
delivered.

## The problem in one sentence

Pairing succeeds and then stops: `PairingModel.confirm()` saves a
`PairedDeviceRecord` and returns, while `AppModel.linkState` — the only thing
`SessionsView` renders from — can be set to `.linked` only by typing a
`latch serve` address and bearer token into `SettingsView.swift:83`. Nothing
connects those two facts, on either end.

## What actually exists today

Everything below was read, not inferred. Line numbers are current as of this
document.

| Piece | Where | State |
| --- | --- | --- |
| Noise XX responder over TCP | `crates/latch/src/cli/remote_access.rs:1764` | Works; no prologue; pins nothing itself — the caller looks the peer static key up in the device store |
| LAN listener + loopback gateway proxy | `remote_access.rs:1232`, `:1538` | Runs; two defects below stop it carrying a request |
| Permission enforcement and header hygiene | `remote_access.rs:1616` | Complete and tested |
| Bonjour advertisement | `remote_access.rs:1527` | Registered with an empty hostname and empty address, and no TXT record |
| `DeviceDirectory` presence/rendezvous rules | `remote_access.rs:171` | In-memory model, referenced only by its own unit tests |
| `probe_direct_path` UDP hole punch | `remote_access.rs:1346` | Returns a reachable peer address and nothing consumes it. Superseded by D4 in favour of `webrtc-ice`. Retire once the direct-probe smoke tool is no longer useful |
| `OpaqueRelay` / `RelayTicket` / `establish_relay_ciphers` | `remote_access.rs:390`, `:322`, `:637` | Complete in-process model of a Latch-operated relay. Superseded by D4: the relay is Cloudflare TURN and reliability comes from SCTP, so this will not be deployed. Retire |
| `DirectConnection` state machine | `remote_access.rs:221` | Complete; test-only |
| Control plane: presence, rendezvous, TURN credentials, pairing, revocation | `services/control-plane/src/api.ts:556` onward | Deployed contract, fully implemented, with tests. Cloudflare TURN is provisioned and configured on Railway; `CloudflareTurnProvider` itself is untested |
| Mac's control-plane client | `apps/LatchDesktop/Sources/LatchDesktop/ControlPlaneHost.swift` | Real HTTP client. Enrolls the account and host device, holds the device bearer token in Keychain, registers pairing requests, lists paired clients. Has **no** presence, rendezvous, or TURN calls |
| Phone's control-plane client | `apps/LatchMobile/Sources/LatchMobileKit/ControlPlane.swift` | Enroll, re-read device, revoke. Nothing else |
| Phone's gateway client | `LatchMobileKit/LatchGateway.swift`, `EventStream.swift` | Complete `/v1` client, but hard-wired to `URLSession` against a typed `GatewayLink` |
| Phone's X25519 identity | `LatchMobileKit/DeviceIdentity.swift` | Created, Secure-Enclave-wrapped, private key reserved for exactly this handshake |
| Phone's Noise implementation | — | Does not exist |

### The gap is on both ends, and it is not symmetric

The phone is missing a signaling client, a Noise implementation, and a way to
run `/v1` over anything but HTTPS. That is the half the mission statement
names, and it is real.

The Mac is missing more than that. It has exactly **one** datapath: a TCP
listener that speaks Noise XX and proxies to the loopback gateway. It has no
control-plane presence publisher, no UDP or ICE datapath, no STUN client, and
no relay client. `DeviceDirectory` is not the Mac's missing client — it is a
model of the *service*, and `services/control-plane/src/api.ts` already
implements that service in TypeScript, with strictly more validation. Wiring
`DeviceDirectory` "to the endpoints" would be wiring a server to itself. What
the Mac actually needs is a client that speaks to the deployed service, using
`PresenceRecord`, `DirectCandidate`, `RendezvousRequest`, and
`RendezvousResponse` as wire types and `validate_presence` / `validate_request`
as pre-flight validators.

## Blocking defects in the existing datapath

No test in the repository opens a TCP connection to the LAN listener. The
`the_helper_never_advertises_the_gateway_and_status_tracks_the_listener` test
covers readiness bookkeeping; `proxy_connection`, `responder_handshake`,
`encrypt_record`, and `decrypt_record` have never run against each other. Two
defects survive there, and either one alone stops a phone from ever seeing a
session list.

**1. The ten-second deadline covers the whole connection, not the handshake.**
`remote_access.rs:1303` wraps the entire `proxy_connection` future in
`tokio::time::timeout(HANDSHAKE_TIMEOUT, …)`. `REMOTE_ACCESS_PHASE_4.md`
describes this as "LAN handshakes have a ten-second deadline", which is the
intent. As written, every connection — including the `/v1/sessions/:id/events`
WebSocket that the chat screen lives on — is killed at ten seconds and audited
as a rejected connection.

**2. The transport state mutex is held across the inbound socket read.**
`remote_access.rs:1591` takes `inbound_state.lock().await` and then awaits
`decrypt_record`, which blocks on the phone's socket. The outbound task at
`:1584` needs the same lock to encrypt the gateway's reply. In the ordinary
request/response shape — phone sends a request, then waits — the inbound task
parks holding the lock and the response can never be encrypted. The first
response never reaches the phone.

Both are one-line-scale fixes (time out only `responder_handshake`; split the
`TransportState` into its two directions, or hold each lock only around the
cipher call rather than around the I/O). Both must land before any phone-side
work can be validated, and both need the round-trip test that would have caught
them.

## Decisions

### D1. The Mac's control-plane client stays in desktop Swift

Presence and rendezvous go into `ControlPlaneHost.swift`, not into the Rust
CLI.

The credentials already live there. `HostEnrollment` holds the account token,
the control-plane device id, and the device bearer token in the Keychain under
`co.cooperativ.latch.control-plane`; presence and rendezvous authenticate with
that same device token. Putting the client in Rust would mean duplicating
Keychain access and adding an HTTP client to a binary whose dependency list is
explicitly held to what CLI startup can afford — the constraint is written into
`crates/latch/Cargo.toml` and restated in the header of `ControlPlaneHost.swift`.

The cost is that the desktop app must be running for the Mac to be reachable.
That is already true: `RemoteAccessController` supervises the helper, so no
helper runs without the app. It is worth stating plainly rather than
discovering later, and it is the reason a headless-server deployment is out of
scope for this plan.

The candidates presence publishes come from the helper, which is the only
process that knows its own bound address. It already writes them: `LanReadiness`
(`remote_access.rs:865`) carries the listener address, and
`RemoteAccessController` already reads it as `status.listenerAddress`. Presence
publication is therefore a read of state the desktop already polls, not a new
channel into the helper.

### D2. Presence lifecycle binds to the switches that already exist

- Published on the transition into "helper running with a listener address",
  which `RemoteAccessController.waitForListener()` already detects.
- Refreshed on a timer at `presenceTtlSeconds / 3` (30s against the default
  90s TTL, matching Rust's `PRESENCE_LIFETIME`), so one missed refresh does not
  drop the Mac offline.
- `DELETE /v1/presence` on `remote-access disable`, on helper exit, and on app
  termination. `setEnabled(false)` and the existing termination handler are the
  hooks.
- Never published while `enabled` is false. This is the global incident switch
  from the runbook and it must gate publication, not merely gate new
  connections.
- Relay is a *separate* gate. `relay disable` must stop `POST /v1/turn-credentials`
  and refuse relay admission, and must **not** stop presence — the runbook's
  first incident step is "run `latch remote-access relay disable`… Direct/LAN
  access remains available", which is only true if presence keeps publishing.
  The control plane already enforces the account-level half of this
  (`api.ts` refuses TURN credentials when `relayEnabled` is false); the desktop
  mirrors the local switch to the account with `PATCH /v1/account`.
- Revocation must drop the peer inside the documented 250ms device-state check.
  That check already exists at `remote_access.rs:1599` for live streams. The
  new surface is rendezvous: an offer addressed to a revoked device must not be
  answered. The control plane already drops presence, offers, and TURN
  credentials on revoke, so the Mac's obligation is to re-read the device store
  before answering an offer rather than trusting the offer's provenance.

### D3. The phone needs almost nothing new on `PairedDeviceRecord`

The record already carries everything signaling requires: `deviceId`,
`accessToken`, `controlPlane`, `permission`, and `mac.publicKey` (the pin) and
`mac.deviceId` (the rendezvous target). The one wrinkle is that `mac.deviceId`
is `String?` — a pairing against a Mac with no control plane has none — so the
transport must treat a nil id as "manual link only" rather than crashing into
an optional unwrap.

What is missing is not a pairing field but a *session* type: a
`RemoteTransportSession` holding the negotiated path, the live Noise transport,
the granted permission as of the last device read, and the connection
diagnostics. That is deliberately not persisted. Nothing about a transport
attempt should outlive the app.

Phone-published presence (`POST /v1/presence` for the phone's own candidates)
is on the objective list and will be implemented, but it is **not** on the
critical path: in the phone-initiates flow the phone offers its candidates and
ICE credentials inside `POST /v1/rendezvous` and the Mac collects them with
`GET /v1/rendezvous`. That asymmetry is also why only the Mac's ICE credentials
have to live in presence (D4b). Standing phone presence is only needed for a Mac-initiated
connection, which nothing currently performs, and it continuously publishes the
phone's address set to the account for no present benefit. It ships behind the
same enable switch as everything else and stays off until a Mac-initiated flow
exists.

### D4. The direct path is standards-based ICE, with Cloudflare as the relay

**Decided.** Cloudflare Realtime TURN is provisioned (key `latch-relay`, id
`1489e4eb5ba03a5d4773aba9c6f3eddd`) and `CLOUDFLARE_TURN_KEY_ID` /
`CLOUDFLARE_TURN_API_TOKEN` are set on Railway, so
`POST /v1/turn-credentials` issues real ICE servers. That settles the fork in
favour of the accepted decision record: standards-based ICE with STUN and TURN,
Noise established over the resulting path, and the relay operated by Cloudflare
rather than by Latch.

Two pieces of the repository are superseded by this and should be retired
rather than built on:

- `probe_direct_path` (`remote_access.rs:1346`) is a hand-rolled
  simultaneous-open UDP probe — the "custom UDP/NAT traversal" the decision
  record rejected. It stays only as long as `latch remote-access direct-probe`
  (`main.rs:923`) is useful as a smoke tool.
- `OpaqueRelay`, `RelayTicket`, and `establish_relay_ciphers`
  (`remote_access.rs:390`, `:322`, `:637`) model a ticketed Latch-operated
  relay that will now never be deployed: there is no ticket endpoint in
  `api.ts` and there will not be one. What survives from that work is the
  *policy* in `DirectConnection` (`remote_access.rs:221`) — no relay without a
  prior direct failure, capability re-discovery after a path change — not the
  ticket, frame, and rate-window mechanics. Objective 5's instruction to
  "respect the relay quotas already fixed on the Rust side" therefore has to be
  reread: those quotas belong to a relay that is not shipping. Cloudflare's
  quotas and the credential TTL (`turnCredentialTtlSeconds`, default 120s) are
  the real limits.

What the decision closes: the server-reflexive candidate gap. Cloudflare
returns a `stun:` entry alongside the `turn:`/`turns:` entries, so both ends can
gather a reflexive candidate and presence can advertise something usable off a
LAN.

### D4a. Resolved: one shared Rust stack, composed below WebRTC's SDP layer

**Where the ICE agent lives on the Mac.** In the helper, alongside the Noise
responder and the gateway proxy, and the helper becomes its own workspace
binary — `latch-remote`. Three reasons, in order of weight: the agent owns a
socket it keeps sending consent-freshness checks on for the life of the
connection, so splitting it from the datapath means two processes driving one
socket; the gateway token stays where `RemoteAccessSupervisor` already keeps it;
and an ICE agent parses unsolicited STUN packets from arbitrary internet
sources, which must not be linked into the binary every terminal window execs.
A Cargo feature is the cheaper-looking alternative and does not work — features
are compile-time, so the shipped `latch` either contains the STUN parser or
does not.

The split follows a seam already visible in `remote_access.rs`: local state
(identity, pairing, device store, grants, revocation, audit, status,
diagnostics) stays in `latch` with its current dependencies; transport
(listener, Noise handshake, proxy, ICE, Bonjour) moves to the new crate.
`mdns-sd` leaves the CLI as a side effect. The desktop change is small —
`RemoteAccessSupervisor.arguments()` already builds an argv against
`client.executableURL`.

**Which implementation, on both ends.** The `webrtc-rs` crate family, compiled
for iOS and consumed from Swift over FFI, so the phone and the Mac run the
*same* ICE implementation rather than two that must interoperate on networks
neither can test.

`str0m` was the first candidate and is ruled out for one specific reason: it
does not implement a TURN client, deliberately. Its README says TURN "is a way
of obtaining sockets" and that obtaining them is the application's job. Pairing
sans-IO `str0m` with the async `turn` crate means running two I/O models side
by side, and on iOS the tokio runtime comes along regardless — which was the
main thing sans-IO was buying.

A libwebrtc XCFramework on iOS is the other alternative and is ruled out on
cost: it ships VP8, VP9, AV1 and Opus encoders to move a few hundred bytes per
second of terminal text, and it would force a second, different ICE
implementation on the Mac.

**Data channels, not raw ICE.** This reverses an earlier reading of this
document. The argument for skipping DTLS was that Noise makes it redundant —
true — but the nominated ICE path is a *datagram* path, and HTTP/1.1 with a
WebSocket upgrade needs a reliable, ordered stream. Noise's `u16`-prefixed
record framing (`remote_access.rs:1813`) assumes exactly that and gets it from
TCP on the LAN path. Off-LAN, something has to provide it. SCTP does, and in
WebRTC SCTP arrives on top of DTLS. So DTLS comes along, not because it adds a
security property, but because it is what SCTP sits on.

Compose the crates directly rather than taking `RTCPeerConnection`:

    webrtc-ice   → nominated datagram path (host, srflx, relay candidates)
    webrtc-dtls  → transport encryption; NOT the peer authentication
    webrtc-sctp  → reliable, ordered association
    webrtc-data  → the data channel the Noise records ride on

Skipping `RTCPeerConnection` skips SDP, which matters: signaling stays
structured fields the control plane can keep validating, rather than an opaque
blob passing through a service whose entire candidate contract exists to refuse
hostnames and loopback addresses.

All four are one family, versioned together — `webrtc-ice`, `webrtc-sctp` and
`webrtc-data` were all at 0.17.2 on 20 July 2026, with roughly 1.4M recent
downloads each. `str0m` is equally alive (0.23.0, 13 August 2026); it is
declined on fit, not on health.

**Noise is unchanged, and so is objective 4.** DTLS authenticates nothing here:
its certificates are self-signed and no fingerprint is verified out of band.
The peer identity is still, and only, the Noise static key checked against
`PairedDeviceRecord.mac.publicKey`. Noise moves from over-TCP to
over-data-channel and its framing does not change, because both are reliable
ordered streams. Say this explicitly in the threat model so nobody later reads
the DTLS layer as the authentication.

**Enforcing "no relay before a direct failure."** `webrtc-ice` gathers relay
candidates whenever TURN URLs are in its `AgentConfig`, and may nominate a
relayed pair on its own. The policy is enforced by *not giving it the URLs*:
run the first agent with STUN only, and on failure request
`POST /v1/turn-credentials` and restart with the relay URLs added. That also
means TURN credentials are never minted for connections that do not need them,
which is what objective 3 already assumes.

### D4b. The signaling schema cannot carry ICE, and this blocks Phases D and E

`webrtc_ice::Agent::dial` and `accept` both take `remote_ufrag: String` and
`remote_pwd: String`. ICE connectivity checks are STUN binding requests whose
`USERNAME` is `remote_ufrag:local_ufrag` and whose `MESSAGE-INTEGRITY` is keyed
by the peer's password. Without exchanging those two values, no check can ever
succeed. The agent also needs each remote candidate's type, priority,
foundation, component and transport to order and prune pairs — and it needs the
type in particular to know which pairs are relayed.

Today's contract carries none of it. `POST /v1/presence` accepts one body key,
`candidates`, and each entry is validated against the allowlist
`['address', 'expiresAt']` with an unknown property producing a 400
(`validation.ts`). There is nowhere to put a ufrag. This is a hard blocker, not
a refinement, and it lands on work already completed: the Mac publishes a bare
listener address and the phone's `TransportCandidate` is `{address, expiresAt}`.

The change, which should land before any more client code is written against
the current shape:

- Extend the candidate object with the ICE fields — `type`, `priority`,
  `foundation`, `component`, `protocol`, and optional `relatedAddress`,
  `relatedPort`, `tcpType`. Keep `address` exactly as it is, still validated as
  an IP literal and port, so the service keeps refusing hostnames and the
  privacy control survives.
- Add `iceUfrag` and `icePwd` to the `POST /v1/presence` and
  `POST /v1/rendezvous` bodies, and return them from `GET /v1/presence/:id`,
  `POST /v1/rendezvous` and `GET /v1/rendezvous`.
- Clients reconstruct the SDP candidate line from the structured fields for
  `unmarshal_candidate`; nothing sends or stores an SDP blob.

The Mac's ICE credentials are scoped to the lifetime of its agent — the helper
process — not to a presence refresh. Rotating them every 30 seconds would race
a phone that read them at the start of a window and began checks at the end of
it. They rotate on helper restart and on ICE restart. The phone's credentials
are per-session and travel in its rendezvous offer, which is why phone-side
presence stays optional (D3).

One consequence for the threat model: the relay is now a named third party.
`REMOTE_ACCESS_THREAT_MODEL.md` should say that relayed traffic transits
Cloudflare, that Cloudflare sees encrypted bytes and traffic patterns and not
content, and that this is why the Noise session is established above the
transport rather than relying on it.

**Before the first relay attempt, verify the provider parses.**
`CloudflareTurnProvider.issue` (`services/control-plane/src/cloudflare-turn.ts:21`)
requires the `iceServers` field to be an **array** and throws otherwise, which
`api.ts` turns into a 503 `relay_unavailable`. Cloudflare's
`generate-ice-servers` endpoint documents a single `iceServers` **object** with
all URLs in one entry. `CloudflareTurnProvider` has no test coverage at all —
`FakeTurnProvider` in `test-harness.ts:38` returns a two-element array, a shape
the live API does not produce — so nothing in CI would catch the difference.
This is one `curl` against the new key to confirm, and a parser that accepts
both shapes to fix.

### D5. Noise on iOS is hand-written, and BLAKE2s is the reason

`Noise_XX_25519_ChaChaPoly_BLAKE2s` needs BLAKE2s. Neither CryptoKit nor
swift-crypto provides it. The other three primitives are available:
`Curve25519.KeyAgreement` for X25519, `ChaChaPoly` with a 12-byte nonce (four
zero bytes then the little-endian 64-bit counter) and AAD support, and HMAC
once a hash exists.

So the phone gets, in `LatchMobileKit/Noise/`:

- `BLAKE2s.swift` — RFC 7693, unkeyed, 32-byte digest, with the RFC's test
  vectors.
- `NoiseSymmetricState.swift` — HMAC-BLAKE2s, HKDF, `MixKey`, `MixHash`,
  `EncryptAndHash`, `DecryptAndHash`, per the Noise specification.
- `NoiseHandshake.swift` — the XX initiator only. The phone is never the
  responder; implementing the responder would be code that exists only to be a
  target.
- `NoiseTransport.swift` — post-handshake `CipherState` pair with the
  `u16` length prefix and 65535-byte record cap that `read_frame` and
  `write_frame` (`remote_access.rs:1813` onward) already define.

Prologue handling is explicit. The Mac's `responder_handshake` uses **no
prologue**, and that is the only one that matters: the second prologue in the
tree, `latch-relay-v1:{relay_id}` in `establish_relay_ciphers`, belongs to the
`OpaqueRelay` model that D4 retires and does not survive it. The Swift API
still takes the prologue as a required parameter with no default, so if a
future path ever needs a distinct one, a caller cannot get it by omission.

**Peer verification is the non-negotiable part.** After the third handshake
message the initiator compares the remote static key, in constant time, against
`PairedDeviceRecord.mac.publicKey` and nothing else. Not against
`peerIdentityKey` from the rendezvous response — that is the control plane's
claim about which Mac it thinks this is, and the whole point of pairing was to
move the pin out of the service's reach. A mismatch is terminal: no retry, no
fallback to relay, no other candidate. It surfaces the same way
`PairingModel.confirm()` already surfaces `identityMismatch`, and it records an
explicit local failure. The rendezvous `peerIdentityKey` is used for exactly
one thing: an early, cheap "the service thinks this is a different Mac" warning
before the handshake is attempted. It can abort; it can never authorize.

### D6. One `/v1` request per Noise session, and a loopback shim to carry it

The Mac's proxy handles exactly one authorized request per connection. It
injects `Connection: close` for anything that is not a WebSocket upgrade
(`remote_access.rs:1671`) and rejects pipelining outright. That is a
deliberate security property — "a lower permission cannot smuggle a second
operation on the same connection" — and this plan does not weaken it.

The consequence for the phone is that an HTTP call costs a Noise XX handshake
(two round trips plus three DH operations), while a WebSocket holds one session
open for its lifetime. Off-LAN this maps cleanly onto SCTP: one data channel per
request over an association that is already established, so the recurring cost
is a data channel plus a Noise handshake, never a fresh ICE negotiation. That is acceptable: the phone makes few HTTP calls
(discovery, session list, send) and lives on the event stream. Connection reuse
or multiplexing would require a new framing layer on both ends and a protocol
version negotiation; it is deferred until measurements say it matters, not
assumed.

`LatchGateway` and `EventStream` are not rewritten. Instead the phone runs an
in-process loopback shim: an `NWListener` on `127.0.0.1:0` that accepts a plain
HTTP or WebSocket connection from `URLSession`, opens a Noise session to the
Mac, and pumps bytes. `AppModel` then builds a `GatewayLink` pointing at
`http://127.0.0.1:<shim-port>`, and every existing client path — discovery,
sessions, send with idempotency keys, the `ws://` upgrade `EventStream` derives
at `EventStream.swift:291` — works unchanged.

Two details this forces, both small and both testable:

- The Mac **rejects** a request carrying `Authorization` or
  `Proxy-Authorization`; it injects the gateway credential itself. So the
  tunnel `GatewayLink` carries an empty token, and `LatchGateway`/`EventStream`
  omit the header when the token is empty. The shim additionally refuses, with
  a local 502 and a clear message, any request that still carries one — a
  credential is refused, never silently stripped.
- The shim binds loopback and iOS App Transport Security applies to
  `http://127.0.0.1`; `NSAllowsLocalNetworking` in the app target's Info.plist
  covers it. Verified on device before Phase B closes.

The alternative — hand-writing an HTTP/1.1 and RFC 6455 client directly over
the Noise stream — avoids an in-app listener but duplicates WebSocket framing
that `URLSessionWebSocketTask` already provides correctly. It is the fallback
if the shim runs into a platform limit, and the shim is written behind a
`GatewayTransport` seam so that swap is contained.

### D7. Identifier and validation mismatches to reconcile

These are wire-level and would each produce a runtime rejection.

| Mismatch | Rust | Control plane | Resolution |
| --- | --- | --- | --- |
| Device id shape | `valid_opaque_id` requires exactly 32 hex characters (`remote_access.rs:1434`) | `dev_<32 hex>` (`validation.ts` `OPAQUE_ID`) | Rust's validator is the local identity format, not the control plane's. The client validates against the control-plane form; the local `identity.device_id` is never sent as a control-plane id |
| Candidate cap | 1–16 | `maxCandidates`, default 8 | Publish at most 8, and treat the server's 400 as authoritative rather than assuming 16 |
| Loopback candidates | Rejected by `DirectCandidate::validate` | **Accepted** — `CANDIDATE_ADDRESS` matches any IP literal | Keep the client-side rejection and state honestly that this is a client rule, not a server-enforced one. Objective text describing it as server-side is incorrect; consider adding the server check too |
| Rendezvous TTL | up to 90s (`validate_request`) | `rendezvousTtlSeconds`, default 60s | Request the server's window, not Rust's |
| `requestId` | 32 hex (`valid_opaque_id`) | 8–128 of `[A-Za-z0-9._:-]` | Any 8–128 character id the service accepts. The 64-hex form `probe_direct_path` needed is no longer a constraint now that D4 selects ICE |
| Bonjour record | `ServiceInfo::new(…, "", "", port, None)` at `remote_access.rs:1530` — empty hostname, empty address, no TXT | — | A record with no host and no address does not resolve. Fix before relying on LAN discovery; add the Mac's public key hint as a TXT record so the phone can skip Macs it is not paired with, while still pinning at the handshake |

## Boundary invariants that do not move

Every phase below is subject to these. They come from
`DECISION_REMOTE_ACCESS_TRANSPORT.md` and `REMOTE_ACCESS_PHASE_4.md` and this
work is not permitted to relax them.

1. The gateway stays loopback-only, on an ephemeral port, with a per-launch
   token the phone never sees. Presence advertises transport candidates and
   never a gateway credential.
2. The pin is `PairedDeviceRecord.mac.publicKey`. The control plane's
   `peerIdentityKey` is a hint, never an authorization.
3. Revocation is immediate: within the 250ms device-state check for a live
   stream, and at the next request for anything the control plane mediates.
4. `observe` / `interact` / `control` are enforced on the Mac, in
   `authorize_and_inject`. The phone's use of the granted permission is a
   user-interface honesty measure, not a security control, and the Mac must
   keep refusing regardless of what the phone offers.
5. No terminal bytes, transcripts, session names, prompt answers, or gateway
   tokens pass through the control plane or a relay in plaintext.
6. Relay is never selected without a prior direct failure — the rule
   `fallback_to_relay` (`remote_access.rs:254`) already enforces on the Rust
   side, mirrored on the phone.
7. `remote-access disable` and `relay disable` remain independent, and both
   remain effective against every new surface added here.

## Phases

The mission's objectives are the skeleton. The ordering below differs from the
objective numbering in one respect, for a reason: objectives 2 and 3 build
signaling for a datapath that does not exist yet on either end, so neither can
be verified when it lands. Doing the phone's Noise client and the Sessions-tab
integration first — against the LAN listener, which is the one datapath the Mac
already has — makes objective 6's acceptance criterion ("a successful pairing
populates the Sessions tab") reachable early and gives objectives 2, 3, and 5
something real to be tested against.

| Objective | Phase |
| --- | --- |
| — (prerequisite) | A |
| 4 (Noise on iOS), 6 (Sessions tab) | B |
| 2 (Mac control-plane client) | C |
| 3 (phone signaling layer) | D |
| 5 (direct + relay datapath) | E |
| 7 (closeout and honest docs) | F |

### Phase A — make the existing datapath carry a request

Prerequisite. Rust only. No phone work depends on anything else here.

1. Scope the ten-second deadline to `responder_handshake` alone
   (`remote_access.rs:1303`); give the proxied connection its own idle timeout
   rather than an absolute one, so a quiet event stream is not a dead one.
2. Fix the transport-state locking at `remote_access.rs:1584`/`:1591` so the
   inbound and outbound directions cannot block each other. Splitting the
   `TransportState` per direction is preferable to narrowing the critical
   section, because it makes the deadlock unrepresentable.
3. Fix `advertise_bonjour` (`remote_access.rs:1530`) to register a resolvable
   record, with the Mac's public key as a TXT hint.
4. Add the round-trip test that is missing: bind a listener, run a real Noise
   XX initiator against it, issue `GET /v1/capabilities`, assert the response
   arrives; then hold a WebSocket-shaped connection past ten seconds and assert
   it survives; then revoke mid-stream and assert closure inside 250ms.

**Done when** a Rust test client completes a full request over the LAN
listener, a long-lived connection survives, and revocation still cuts it.

### Phase B — the phone reaches the Mac on a LAN (objectives 4 and 6)

1. `LatchMobileKit/Noise/` per D5: BLAKE2s with RFC vectors, symmetric state,
   XX initiator, transport with the `u16` framing.
2. Interoperability fixtures. `snow` exposes
   `Builder::fixed_ephemeral_key_for_testing_only`, so a Rust test can emit
   deterministic transcripts into `fixtures/remote-access/noise/` and the Swift
   suite can replay them. Both suites read the same fixtures; neither snapshots
   its own output.
3. The mismatch test, explicitly: a responder presenting a static key other
   than `mac.publicKey` produces a terminal identity failure, no retry is
   attempted, and no bytes are forwarded.
4. LAN discovery on the phone: `NWBrowser` for `_latch-remote._tcp`, filtered
   by the TXT hint, pinned at the handshake regardless.
5. The loopback shim per D6, behind a `GatewayTransport` seam, plus the
   empty-token change in `LatchGateway` and `EventStream`.
6. `AppModel` gains a second way to become `.linked`: from a paired record and
   an established tunnel, running the same `discover()` then `listSessions()`
   contract it runs over HTTPS. The manual `latch serve` path stays coequal —
   it is the only path that works with no control plane at all, and
   `SettingsView.swift:30` already documents both as supported.
7. `AppModel.surface` intersects `GatewayCompatibility.sessionSurface(for:)`
   with the granted `DevicePermission`, so an `observe` phone loses the
   composer instead of sending and being refused. `DevicePermission.permits`
   already exists for this.
8. Remove the interim copy: the "does not carry the session list yet" footer at
   `PairingView.swift:225` and the matching note in `SessionsView.swift:138`.
   They stop being true here and must not outlive their truth.

**Done when** a phone paired to a Mac on the same network shows that Mac's
sessions with no address typed anywhere, an observe-only phone shows no
composer, and revoking on the Mac empties the tab.

### Phase C — the Mac publishes presence and answers offers (objective 2)

Desktop Swift, per D1 and D2.

1. Extend `ControlPlaneHostAPI` with `publishPresence`, `clearPresence`,
   `collectOffers`, and `setRelayEnabled`, mirroring the existing method style
   and error mapping.
2. A `PresencePublisher` owned by `RemoteAccessController`: publishes on
   listener-up, refreshes at TTL/3, deletes on disable, helper exit, and app
   termination. Never publishes while `enabled` is false. Independent of the
   relay switch.
3. Pre-flight validation on the client so it cannot publish what the service
   will reject: at most `maxCandidates` entries, no loopback, absolute expiries
   inside the presence window. The Rust rules in `validate_presence` are the
   reference; the server's 400 is the authority.
4. Extend the control-plane signaling schema per D4b: ICE fields on the
   candidate object, `iceUfrag`/`icePwd` on presence and rendezvous bodies and
   responses. This is the long-lead item — a deployed service two clients
   depend on — so it lands before more client code is written against the
   current shape.
5. Add `GET /v1/ice-servers` per D4: device-authenticated, STUN entries only,
   no peer scope, and **not** gated on `relayEnabled`, so reflexive candidate
   gathering survives `relay disable`. Add the `CloudflareTurnProvider` test
   coverage that does not exist today, including the object-shaped `iceServers`
   response the live API returns.
6. Publish the agent's full candidate set and ICE credentials, not a bare
   listener address. Rendezvous is single-shot with no trickle, so a candidate
   absent from presence at offer time never reaches the peer. Until the ICE
   agent exists the Mac publishes only its host candidate, which is a correct
   degenerate case of the same shape rather than a different one.
7. An offer poller that collects `GET /v1/rendezvous`, re-checks the requester
   against the local device store (a revoked device's offer is dropped, not
   answered), and hands the accepted candidate set to the datapath. Until
   Phase E there is no direct datapath to hand it to, so this phase's poller
   answers only with the LAN candidate it already publishes, and logs a
   content-free "no usable path" outcome otherwise. That is honest and it is
   testable.
8. Local audit events for publish, clear, and offer-answered, matching the
   existing content-free event vocabulary.

**Done when** a Mac with remote access on appears `online` in
`GET /v1/devices` for its paired phone, disappears within the TTL of disable,
never appears while disabled, keeps appearing with relay disabled, and refuses
an offer from a locally revoked device.

### Phase D — the phone's signaling client (objective 3)

`LatchMobileKit`, extending `ControlPlane.swift` with a `SignalingClient`
sibling that reuses `ControlPlaneError` and its message mapping.

1. `GET /v1/presence/:deviceId` — is the Mac online, and with which candidates.
2. `POST /v1/rendezvous` — offer candidates, receive the Mac's plus
   `peerIdentityKey` and `permission`.
3. `GET /v1/rendezvous` — collect offers (for a future Mac-initiated flow).
4. `POST /v1/turn-credentials` — relay fallback, gated on a prior direct
   failure.
5. `POST /v1/presence` for the phone's own candidates, off by default per D3.
6. Error mapping, specifically: `409 target_offline` becomes a
   `macNotReachable` state whose sentence is "Your Mac is not reachable right
   now" with the reason, not a generic HTTP error. `403` on turn-credentials
   distinguishes "relay is disabled for this account" from "this pairing was
   revoked" — the reasons differ and so does what the person should do.
7. The granted `permission` from the rendezvous response updates the record the
   same way `refreshPermission()` already does, and downgrades on an
   unrecognized value exactly as `PairingConfirmation.Device` already does.
8. Unit tests against the existing `ControlPlaneTests` stub-client pattern. No
   simulator.

**Done when** the phone can determine reachability, exchange candidates, and
report each failure as a sentence a person can act on.

### Phase E — the datapath off the LAN (objective 5)

D4 is decided, so this phase implements ICE rather than choosing a mechanism.
The two sub-decisions D4 leaves open — where the ICE agent lives on each end,
and whether the helper becomes its own binary — are settled first, in writing,
because they change the shape of the code rather than its behaviour.

1. Amend `DECISION_REMOTE_ACCESS_TRANSPORT.md` with the Cloudflare selection,
   the two sub-decisions, and the note that the relay is a named third party.
   Mark `probe_direct_path` and the `OpaqueRelay` family superseded, and say
   what replaces the quota rules they carried.
2. Split the helper into the `latch-remote` binary (D4a) and build the shared
   Rust transport core there: `webrtc-ice` → `webrtc-dtls` → `webrtc-sctp` →
   `webrtc-data`, with Noise XX established over the resulting data channel.
   Composed directly rather than through `RTCPeerConnection`, so no SDP is
   generated and signaling stays structured.
3. Expose the same core to the phone over FFI, behind the `GatewayTransport`
   seam Phase B introduced, so the loopback shim and everything above it is
   untouched by the change of path. Keep the FFI in its own target so the rest
   of `LatchMobileKit` still tests with plain `swift test`.
4. Relay only after a direct failure, enforced by withholding the TURN URLs:
   the first agent gets STUN only, and a failure triggers
   `POST /v1/turn-credentials` and an ICE restart with the relay URLs added.
   This mirrors `fallback_to_relay` (`remote_access.rs:254`), which refuses
   relay without a recorded direct failure.
5. Path migration and relay-to-direct recovery, holding the new path until
   capability discovery completes. `AppModel.rediscover()` and
   `LatchGateway.invalidateDiscovery()` are the phone's half and already exist.
6. Honour the limits that are actually real: Cloudflare's credential TTL
   (`turnCredentialTtlSeconds`, default 120s) and the re-request it forces on a
   long relayed session, the control plane's per-device rate limit
   (`rateLimitPerMinute`, default 240), and the `MAX_RECORD` framing the Noise
   transport already imposes. The `RelayLimits` ticket/window numbers do not
   apply to a Cloudflare relay and must not be copied into the phone as if they
   did.
7. Backgrounding handled honestly. iOS suspends the app, the sockets die, the
   ICE agent dies, and the shim's listener goes with them. On resume the phone
   re-establishes and **then** re-runs discovery before sending anything,
   rather than assuming the pre-suspension capabilities still hold. Nothing is
   auto-replayed without a protocol idempotency key; `LatchGateway.send`
   already carries one for `message` and `resolve`, and `keys` deliberately has
   none, so terminal input is never replayed.

**Done when** a phone on cellular reaches a Mac behind a home NAT over a direct
ICE path, a symmetric-NAT case falls back to Cloudflare TURN only after direct
fails, `relay disable` blocks that fallback while leaving the direct path
working, and a Wi-Fi to cellular transition migrates without duplicating a
submission.

### Phase F — closeout (objective 7)

1. End-to-end coverage across both suites: pair, publish presence, rendezvous,
   handshake with the pinned key, list sessions, send a message, revoke, and
   confirm the phone is cut off.
2. Rewrite `REMOTE_ACCESS_PHASE_4.md`'s verification matrix. It currently
   states the platform is headless and ships no phone UI, which stops being
   true. The matrix rows must say what was actually run:
   - The "Same-LAN path on the CI host" row currently reads `Pass` on the
     strength of a readiness-bookkeeping test and a UDP probe. It becomes an
     honest row once Phase A's round-trip test exists.
   - The deferred preproduction rows — physical NAT traversal, cellular,
     captive portal, sleep/wake, sustained relay availability — stay deferred
     and stay labelled as such. They do not become `Pass` because a phone
     client now exists.
3. Update `REMOTE_ACCESS_IMPLEMENTATION_PLAN.md`'s status and
   `REMOTE_ACCESS_DESKTOP.md` for the presence lifecycle.
4. Restate the release gates that remain open regardless of this work:
   independent security review, Apple signing and notarization in the release
   pipeline, and measured load and soak results against the deployed
   Cloudflare relay — provisioning the relay is not the same as measuring it.
   Remote access and relay stay off by rollout policy until those are
   satisfied.

## Test strategy

- **Cross-language fixtures, not parallel snapshots.** Noise transcripts,
  presence bodies, and rendezvous bodies live under `fixtures/remote-access/`
  and are consumed by both the Rust and Swift suites. A test that snapshots its
  own implementation's output proves only that the implementation is stable.
- **The Rust round trip is the anchor.** Phase A's test is the only place both
  halves of the Mac's datapath are exercised together, and every later phase
  regresses against it.
- **Swift tests stay simulator-free.** `LatchMobileKit` has no dependencies and
  `swift test` runs the whole suite; the Noise implementation, the signaling
  client, and the shim's framing are all testable that way. Only the
  Info.plist/ATS check and Bonjour discovery need a device.
- **Negative tests carry the same weight as positive ones**: identity mismatch
  is terminal, a revoked device is refused at rendezvous and mid-stream, an
  `observe` phone cannot send, relay is refused without a prior direct failure,
  and presence never publishes while remote access is off.

## What this plan does not do

- It does not operate a relay. Cloudflare Realtime TURN is provisioned and
  configured (D4), so the relay is a third-party service Latch pays for rather
  than one it runs. The Latch-operated `OpaqueRelay` is superseded and will not
  be deployed.
- It does not add account recovery. Losing the Mac's Keychain item still means
  re-approving devices, by design.
- It does not make the Mac reachable without the desktop app running (D1).
- It does not retire the Bonjour and TCP LAN path once ICE lands. That path is
  the only one that works with no control plane configured at all, which
  `SettingsView.swift:30` documents as supported. ICE host candidates do not
  replace it, because reaching them still requires signaling.
- It does not weaken the loopback-only gateway, the pinned identity, or
  immediate revocation. If a phase appears to require that, the phase is wrong.
