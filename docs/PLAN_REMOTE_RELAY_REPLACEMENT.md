# Secure relay remote access implementation plan

Date: 7 September 2026, updated 8 September 2026. Reviewed after the connection fixes in this session, then revised by the planning-review objective `coo:952.tskr` (see section 13). Status: Objectives 1 and 2 implemented and locally verified (sections 14 and 15); Objective 3 deployment and cutover performed on 8 September (section 16); the physical matrix, soak, and live APNs delivery are in progress there.

Overlord mission: **coo:952 — Replace remote access with an encrypted relay and resilient mobile sessions**. Objective order: `coo:952.tskr` (plan review), `coo:952.ay7b` (secure transport), `coo:952.95p7` (resilience), then `coo:952.wfc0` (deployment and physical verification). Auto-advance remains disabled.

## 1. Goal, timing, and decisions

Replace Latch's required internet ICE/STUN/TURN path with reusable, outbound WSS connections through an opaque relay. A phone must be able to discover, create, observe, and control sessions on its paired Mac across ordinary internet connections, recover from interruptions, and receive attention notifications. Execution, authoritative session state, device grants, and plaintext stay on the endpoints.

**Execution prerequisite passed on 7 September:** after the final Mac-only handover fix, the owner completed four physical iPhone 16 runs on cellular with Wi-Fi disabled. Session listing, shell creation, terminal attachment, input, and rendered `pwd` output succeeded on every pass, with approximately one-to-three-second steps. The phone reported `Direct 193`; the Mac-side evidence is [the recorded cellular field run](field-runs/cellular-to-home-nat-20260907T095841Z.json). Exact helper/source fingerprints and the evidence limits are in [REMOTE_ICE_STALL_FINDING.md](REMOTE_ICE_STALL_FINDING.md). Source `a2ab11dd44c8f68a887f6d276daa3af8b0ca7e97` is archived as `remote-ice-baseline-2026-09`. The owner explicitly launched Objective 1 after that confirmation. This evidence closes the start gate only; it is not WSS release evidence or a p95 measurement.

**Single-user clean replacement:** the owner is currently the only user. Upgrade Desktop, the signed CLI/helper payload, Mobile, control plane, and relay together. Require new pairing. Do not implement mixed-version operation, dual transport feature flags, old pairing migration, old wire-format negotiation, a v1 gateway compatibility adapter, or automatic fallback to ICE. Preserve actual sessions, repositories, conversations, and user files. Remove obsolete remote code and dependencies when the replacement works in the implementation checkout. Retain an archived release as an operational rollback option, not an active compatibility layer.

**Production is not preserved during the build (owner decision, 7 September):** all three objectives complete before the app is used in production again, so implementation proceeds on `main` in the shared checkout. `.github/workflows/control-plane.yml` runs on every push to `main` touching `services/control-plane/`, and the Railway control plane deploys from `main`; pushes during Objectives 1 and 2 will therefore redeploy the production control plane and may break the currently installed phone/helper remote path. That is accepted. The only preservation steps are the ones the build itself needs: tag the source of the installed tested builds (`remote-ice-baseline-2026-09`) before removing old code, and keep the archived signed release as the deliberate rollback reference from section 11. Local sessions, repositories, conversations, and user files are still preserved throughout.

Decisions:

| Area | Decision |
| --- | --- |
| Internet transport | WSS on TCP 443; both devices initiate outbound connections. No public gateway listener or port forwarding. |
| Cloud layout | Separate control-plane and relay deployables, as required by `ARCHITECTURE_RULES.md`. |
| Endpoint link | One encrypted, multiplexed link per paired phone/Mac while that phone is active. Each pair uses its own opaque relay room and two WSS sockets. The Mac keeps its side available while remote access is enabled. |
| Encryption | Shared Rust `snow` implementation of `Noise_XX_25519_ChaChaPoly_BLAKE2s`, with pinned identities and explicit protocol binding. Fresh handshake on every new link. |
| Multiplexing | A pinned Rust `yamux` implementation inside the encrypted link; bounded adapters expose reliable logical byte streams. No new custom multiplexer or cryptographic primitive. |
| Authorization | Mac-owned device grants and current permission checks for streams and actions. Relay admission never grants gateway access. |
| Application contract | Extend the existing schema-first `/v2` gateway and Conversation Hub where needed. Do not create a competing remote-session protocol. |
| LAN | Retain authenticated LAN access using the same new encrypted link and stream protocol. No ICE on LAN. |
| Direct internet paths | Deferred. No NAT traversal or active relay-to-direct migration in this mission. |
| History and recovery | Existing local Hub projections, revisions, generations, operation epochs, and durable outcomes. No cloud transcript store. |
| Notifications | Generic APNs attention notifications through the control plane; fetch details from the Mac after authenticated reconnection. |

The shared Noise link followed by multiplexing intentionally replaces the earlier suggestion of retaining a Noise handshake per gateway stream. With no legacy requirement, one implementation and one authenticated link lifecycle are simpler. This is a changed trust boundary requiring the negative tests below, not an assertion that connection authentication alone authorizes every request.

## 2. Source baseline and existing foundations

The initial plan inspected `1d73ecc7500f5a9abc9096850ac1e2bcd952b237`. This review inspected clean HEAD `a2ab11dd44c8f68a887f6d276daa3af8b0ca7e97`, which contains this session's fixes and updates Cargo's version to `0.2609070836.0`. The tested phone and helper were built before that version bump and report `0.2609061156.0`; do not identify an installed binary from the current Cargo version alone. Re-read source and installed artifacts at execution, preserve concurrent edits, and never edit, stage, revert, or delete `.overlord/project.json`.

### Connection work already completed

| Area | Current behavior and replacement obligation |
| --- | --- |
| Agent/FFI ownership | Abandoned agents close on their owning Tokio runtime; close cancels pending work, and regather releases old endpoints. Preserve equivalent cancellation/cleanup across Swift and Rust in the new link. |
| Discovery and request ownership | Providers share one sequencer per pinned Mac; path callbacks invalidate discovery without opening a request behind themselves. The new app-scoped link replaces ICE sequencing but must retain single ownership and avoid callback/self-wait deadlocks. |
| STUN scheduling | Vendored `webrtc-ice` uses bounded per-candidate STUN queues so a stalled TURN permission does not block healthy paths. The replacement must isolate blocked logical streams and bound queues; it need not retain ICE machinery. |
| Final response delivery | Normal HTTP EOF drains queued SCTP records before teardown, with a deadline; error/revocation cleanup remains immediate. Preserve final-response delivery and distinguish graceful stream completion from cancellation. |
| IPv6 reachability and gathering | TURN client sockets now attempt IPv4 and IPv6; an IPv6 TURN connection can yield an IPv4 relay. UDP STUN probes skip TCP/TLS-only URLs. TURN over TCP/TLS itself remains unsupported. The new WSS client must resolve/connect across address families and must not inherit an IPv4-only assumption. |
| Helper handover | Offers wait in bounded background work for up to eight seconds for a replacement agent, with at most four pending/connecting offers. This closes an observed rejection gap; directory presence is still not a transactional reservation. The replacement removes candidate/agent handover entirely. |
| Diagnostics | Opt-in local traces are capped at 8 MiB and filter credentials and upstream packet-byte dumps. Preserve content-free stage/timing diagnostics; classify normal client closure separately from generic proxy rejection. |

Current field evidence: the final supplied phone run uses `turn-ipv6-v1`, contains relay candidates in every attempt, and establishes six of seven transports in approximately 1.3–1.5 seconds. The seventh is the subsequently fixed Mac handover rejection and its retry connects. The owner reports that it works, although startup is still slow. These times cover transport connect only, not tap-to-capabilities, session creation, first terminal output, p95, or the release matrix below. A selected IPv4 relay address does not identify whether its client socket used IPv4 or IPv6.

Installed baseline fingerprints at the end of the session:

- Helper SHA-256: `7bfc23be1c0cae159b38e9109c782d640a563f06f7dc1d39eb4e6497e42923eb`; helper marker `offer-handover-v1`.
- Rebuilt iOS native archive SHA-256: `9e6ce2494233da38e8b30bac855e2a1fc84ab338a656701462657e9d37743d17`; phone trace marker `turn-ipv6-v1`. This archive fingerprint is build provenance, not an independently extracted installed-phone binary hash.
- Thirty transport/FFI/helper tests, the rebuilt native Swift boundary test, five XCFramework architectures, and the iPhone build passed for the IPv6 change. The later helper handover change passed all four helper tests and a signed release build. Earlier response-drain work passed 39 host remote-access tests. These suites validate their recorded revisions; none validates the unimplemented WSS replacement.

The architecture decision remains a reusable WSS/Noise/Yamux link. The justification is reduced per-request setup and retry ownership, usable TCP-443 reachability where UDP is blocked, and coherent lifecycle/recovery—not a claim that ICE cannot work or that these source defects remain unfixed. Do not expand this mission into further ICE optimization or silently weaken its acceptance gates.

| Existing component | Implementation use |
| --- | --- |
| `crates/latch-remote/src/main.rs`, `ice.rs` | Replace ICE responder and offer polling with relay/LAN link supervision; keep the dedicated helper process. |
| `crates/latch-transport`, `crates/latch-transport-ffi` | Replace RTC API with shared Rust secure-link and stream API; regenerate UniFFI and XCFramework. |
| `crates/latch/src/cli/remote_access.rs` | Retain local device store, fixed gateway supervision, request authorization, and audit; refactor proxy input to authenticated logical streams supplied by the helper; remove its own Noise handshake, LAN listener, and Bonjour code once the shared link replaces them. |
| `apps/LatchDesktop/.../RemoteAccessController.swift`, `RemoteAccessSupervisor.swift` | Pairing UI, local grant management, helper lifetime, typed readiness/status, and remote-access settings. |
| `apps/LatchMobile/.../GatewayTransport.swift`, `PairedRoute.swift`, `NativeRemoteTransport.swift` | Single link owner and logical stream adapter for the existing gateway clients. |
| `apps/LatchMobile/.../Noise.swift`, `Support/Blake2s.swift` | Retire duplicated phone-side protocol implementation after shared-core parity tests. Remove helpers only after confirming no other consumer. |
| `services/control-plane/src/api.ts`, `store/`, `credentials.ts` | Enrollment metadata, room admission, leases, durable revocation, entitlement, and push routing. |
| `crates/latch/src/conversation/{hub,cache,projection}.rs` | Reuse durable operations and Hub-owned recovery; fill demonstrated gaps only. |
| `crates/latch/src/cli/create.rs`, `cli/serve/http.rs` | Extend existing request-ID-based remote shell creation and crash reconciliation. |
| `ConversationSocket.swift`, `ConversationStore.swift`, `TerminalSession.swift` | Reuse conversation recovery and terminal ownership behavior. |
| `schemas/remote-access/v2`, `fixtures/conversation`, `packages/client` | Keep schema generation, clients, and gateway fixtures aligned. |

Security-finding status at `a2ab11d`: no source under `crates/`, `services/`, or `apps/` references SEC-01–SEC-04 or VAL-01/VAL-02, and the remediation-planning mission `coo:949` has not delivered. Anonymous `POST /v1/accounts` (used by Desktop's account bootstrap in `ControlPlaneHost.swift`), the directory-selected enrollment key, the periodic route-minimum grant check, `DELETE ... RETURNING` credential take, and the uncapable loopback listener are all still present. Treat every finding as open; credit nothing until a regression proves it.

Component boundary for the replacement: `latch` (the window-path CLI) must not depend on `latch-transport`, as `crates/latch-remote/src/main.rs` already states. Today `crates/latch/src/cli/remote_access.rs` owns the Noise proxy (`snow`), the LAN `TcpListener`, and Bonjour advertisement (`mdns-sd`); the helper injects only the ICE agent through the `PeerTransport` trait. In the replacement, one Rust Noise/Yamux/WSS/LAN-framing implementation lives in `latch-transport` and is driven by `latch-remote` (helper) and `latch-transport-ffi` (phone). `latch` keeps the device store, grant authority, fixed-gateway supervision, per-stream/per-action authorization, proxy, and audit, consuming already-authenticated logical streams through an injected trait, and drops its own handshake/transport Noise code so there is exactly one handshake implementation in Rust and none in Swift. Identity keypair generation may remain in `latch`. The LAN accept loop and Bonjour advertisement move with the link into the helper-owned code.

The current Hub already tracks durable operations and treats interrupted execution as ambiguous. It already supports a generation/revision resume position with snapshot fallback. This mission must exercise and strengthen those behaviors through the real relay path, rather than introducing a second journal, harness-event cursor, or client-side conversation reducer.

## 3. Topology and ownership

```text
Phone UI / gateway clients
  -> capability-protected local adapter
  -> logical gateway stream
  -> Yamux -> Noise records -> WSS
                            |
                      opaque relay room
                            |
Mac helper <- Yamux <- Noise records <- WSS
  -> local device/permission validation for each stream and action
  -> authorized request to a fixed loopback gateway
  -> existing Conversation Hub / latchd sessions

Both endpoints -> control plane: enrollment, opaque room admission, lease renewal
Mac -> control plane -> APNs: generic attention notification for paired phone
```

The relay transports no HTTP gateway semantics. It cannot choose a target address, mint a Mac grant, inspect stream headers, or read session names. It may observe peer IPs, an opaque room identifier, admission role, timing, and traffic sizes. Infrastructure correlation is still possible; do not advertise anonymity or protection from traffic analysis.

### Endpoint ownership

- `latch-remote` owns WSS/TLS connections, LAN listeners, Noise state, multiplexing, and all their cancellation. Internet protocol dependencies remain outside the ordinary `latch` execution/window startup path.
- The Rust transport core owns one task tree per pair, including read/write pumps, handshake deadline, keepalive, flow control, and stream teardown. Every exit cancels and joins child tasks. No detached forwarding survives an owner error.
- Desktop starts one helper and manages user-facing enrollment/settings. Replace candidate/file polling with a versioned owner-only local IPC channel for configuration, pending enrollment decisions, readiness, grant invalidation, and status. Preserve fixed loopback gateway supervision in the helper. Never pass tokens in argv or publish the plaintext gateway.
- Mobile has one app-scoped connection coordinator with one link owner per pair. Screens request streams; they do not open relay connections. Swift sends events through a bounded subscription. The Rust owner must never await a UI callback that may open another stream.
- A network-link failure closes logical streams but does not terminate a latchd session or kill an agent. An application stream failure closes that stream unless the framing or authentication failure makes the whole link unsafe.

### Single-user deployment topology

Start with one relay instance and one region near the user's primary networks, deployed separately from the existing control plane. Each approved pair gets a cryptographically random 256-bit room ID. Both sides receive the same exact relay hostname and room assignment from the authenticated control plane. The relay sees only that opaque assignment.

Do not place the relay behind round-robin replicas: two sockets for one room must reach the same process. Configure one replica explicitly. A future multi-region/sharded design must assign a concrete shard and re-establish both ends together. Multi-region availability is outside this mission. A relay restart is an expected reconnect event and never requires re-pairing.

Each pair is independently bounded and isolated; an idle paired phone consumes one waiting Mac socket, not a running gateway stream. Start with a maximum of eight paired controllers per Mac. One active controller-role socket per room; a freshly admitted higher role-generation replaces an older socket. Close both paired socket sides on replacement or peer loss so bytes from different generations cannot mix. The unaffected Mac reconnects and waits. A waiting Mac socket may remain without a phone indefinitely while renewing its lease; no data is buffered for an absent peer.

## 4. Schemas and relay admission

Add `schemas/remote-link/v1/` and cross-language fixtures for enrollment messages, signed admission claims, relay framing, encrypted link hello, limits, status/error categories, and local helper IPC. This version is independent of the existing `/v2` application API. Accept only the implemented link version and fail closed on mismatch. Generate data models; do not hand-copy contract enums across Rust, TypeScript, and Swift.

Use maintained WSS/TLS and token-signature libraries. Pin versions after checking current platform support and advisories during implementation; at review time crates.io lists `tokio-tungstenite` 0.30, `yamux` 0.14, `snow` 0.10 (the workspace pins 0.9), and `rustls-platform-verifier` 0.7. Use system certificate validation on both Apple platforms: either `tokio-tungstenite` with `native-tls` (Security.framework, already a workspace dependency) or rustls with `rustls-platform-verifier`. Do not use `webpki-roots` or `rustls-native-certs`; neither reads the iOS trust store. Never ship an insecure TLS option. For test certificates, use an explicit test CA in test-only configuration, and make the composed integration harness run a real TLS listener so the client's verification path is exercised. In production the hosting edge terminates TLS and the relay process serves plain WebSocket on its assigned port; the relay must therefore also accept a direct-TLS configuration for tests and any non-edge deployment. Disable WebSocket compression and body/header logging on relay traffic; reject tickets presented anywhere except the upgrade Authorization header.

### Control-plane API changes

Exact final JSON shapes belong in the schemas, but implement these responsibilities and method boundaries:

| Method/path | Purpose and enforcement |
| --- | --- |
| `POST /v1/enrollments` | Authenticated Mac opens a five-minute provisional enrollment and receives opaque relay admission. |
| `POST /v1/enrollments/{id}/claim` | A one-use QR admission code gives a phone provisional device credentials and enrollment-room access only. It does not authorize any gateway route. |
| `POST /v1/enrollments/{id}/complete` | Authenticated Mac mirrors a locally committed exact phone key/grant; promote provisional credentials only for that enrollment. |
| `DELETE /v1/enrollments/{id}` | Cancel and close the provisional room; expire abandoned provisional credentials. |
| `GET /v1/remote-links` | Device-authenticated pair/room configuration and protocol version; keys returned here cannot overwrite local pins. |
| `POST /v1/relay-admissions` | Issue short-lived role-scoped room admission to an active, entitled paired device, or a restricted provisional enrollment participant. |
| `POST /v1/relay-leases/{id}/renew` | Recheck device, pairing, entitlement, grant generation, and configured remote-access state before renewing the active lease. |
| Private relay admission/lease API | Relay authenticates with a service credential; atomically redeem tickets, activate/renew leases, and consume revocations. No endpoint identities are returned to the relay. |
| Existing device/pairing revoke and account relay-disable routes | Deny admission immediately and atomically enqueue durable lease invalidations. |

Use signed Ed25519 admission claims with a pinned issuer/audience/key ID and: opaque `roomId`, `role` (host/controller), room purpose (enrollment/session), unique ticket ID, per-role monotonic generation, issue/not-before/expiry times, and resource limits. No account ID, device key, session ID, gateway token, or permission grant belongs in these claims. The relay gets only issuer verification keys, its own service credential, and opaque lease state.

Admission tickets expire after 60 seconds and are atomically single-use in the control-plane database. The endpoint sends the ticket in the WSS upgrade Authorization header, never a URL. Relay validates signature and purpose, then redeems through the private API before admitting. An uncertain redemption can be queried/retried idempotently by the same authenticated relay attempt ID; it cannot create two leases. Redemption failure refuses the new connection. Persist the spent-ticket record until its expiry plus clock-skew allowance so restarting either service cannot reopen it.

A redeemed connection gets a ten-minute lease; endpoint renewal starts at five minutes. Renewal only extends the same room/role/generation after authorization succeeds. The control plane returns the renewal as a signed lease-extension claim (same lease ID, room, role, generation, new expiry); the endpoint forwards it over the WSS control channel and the relay verifies it with the pinned issuer keys, so the common renewal path needs no relay-to-control-plane call. Invalidation travels the other way, from the control-plane outbox to the relay's authenticated private endpoint. On expiry, the relay closes the socket even if the control plane is unavailable. A control-plane outage therefore prevents new admissions, while established links last only to their current lease deadline. Local authorization continues throughout. These are chosen initial limits, not measured guarantees.

Revocation updates authorization and writes an outbox entry in one PostgreSQL transaction. Workers send idempotent room/lease invalidations to the relay and retain entries until acknowledged or a justified hard lease expiry. Retry through process restart and partial failures. The relay closes active rooms on invalidation; stale signed tickets remain unusable because redemption rechecks current state. Include issuance-versus-revocation races in database tests.

Close public anonymous funded account creation. For this single-user product, provision the owner's account through an operator-authenticated path and one-use enrollment invitations; no general OAuth product is required. Concretely: an operator command in `services/control-plane` (authenticated by an operator secret from the deployment secret store) mints a one-use, short-lived owner invitation; Desktop's account bootstrap exchanges that invitation for the account credential instead of calling anonymous `POST /v1/accounts`, which is removed. Desktop needs a small settings entry for the invitation. Enforce per-owner, per-device, per-IP, and deployment-wide admission/concurrency/bandwidth budgets. Identity churn must not reset aggregate budgets. Size the per-device admission budget for legitimate reconnect bursts (a relay restart makes every waiting Mac and active phone request a fresh ticket within seconds) so the budget throttles abuse rather than recovery. An entitlement kill switch stops new leases and invalidates existing ones. Configure provider spending alerts in addition to application limits.

## 5. Pairing and live authority

Re-pair all devices at cutover. Never automatically import old cloud directory rows into the local grant store.

1. On the unlocked Mac, the owner starts pairing. Generate an enrollment ID, a one-use high-entropy admission code, and a separate 256-bit enrollment secret, and open a short-lived provisional room. The QR contains the link version, trusted control-plane origin, enrollment ID, Mac static public key, admission code, and enrollment secret. The admission code is sent to the service and buys relay admission only. The enrollment secret is never sent to any service; both endpoints mix it into the enrollment Noise prologue, so a party holding the admission code (including a compromised control plane or relay) cannot complete the enrollment handshake with the Mac without having scanned the physical QR. This is the primary SEC-01 control; the owner comparison below is confirmation, not the only barrier.
2. The phone validates the supported origin/version, pins the Mac key from the scanned QR, generates or loads its private identity from Keychain, claims provisional admission, and establishes the enrollment Noise link with the QR enrollment secret in its prologue. The phone verifies the Mac static key before sending sensitive enrollment content. The Mac knows this is a provisional, unauthenticated-for-gateway phone.
3. Inside that encrypted link, the phone submits its identity and requested grant bound to the exact enrollment ID. Both devices display comparison words derived from the Noise handshake hash (which already commits to the enrollment secret and both static keys) and the canonical proposed grant. Bind purpose, version, enrollment ID, endpoint roles, and requested grant to the enrollment exchange. Use a fixed reviewed comparison encoding with at least 64 bits of comparison entropy; owner confirmation must compare both displays, not approve an arbitrary directory row.
4. The Mac owner explicitly approves that exact key and permission before persistence. Cancellation, timeout, conflicting proposals, or a changed permission require a fresh approval. At most one proposal can be committed for an enrollment.
5. Commit the Mac-local grant durably, then send an authenticated `PairingApproved` receipt containing the enrollment ID, both keys, granted permission, and local grant revision. Only then does Mobile persist an active pairing. The Mac mirrors this exact committed result to the control plane; a failed mirror is retried durably and shown as incomplete remote setup.
6. If the receipt is lost, the same phone key can recover the committed result through the still-authorized provisional exchange, after reauthentication. Once the provisional window expires, retain the local committed grant but require the owner to restart/confirm setup if the phone has no receipt. Do not guess whether pairing succeeded or auto-enroll another key.

Normal session links authenticate both static keys against local records. Cloud directory responses cannot replace pins, elevate grants, or recover a lost identity. Key loss/rotation requires re-pairing. Keep private keys in endpoint secure storage, supply Rust only bounded in-memory key material, zeroize copies where supported, and never log it.

Move authorization context from a per-physical-socket assumption to a device-scoped context with local grant revision. Resolve current authority for every opened logical stream. A stream carries one authorized HTTP request or its explicitly authorized WebSocket upgrade. Refuse pipelined second requests, arbitrary destinations, and client-supplied trusted authorization headers. The gateway token is injected only on the Mac's fixed internal hop.

Before each conversation mutation reaches a connector, recheck the current device grant through an authoritative device-scoped context rather than a subscriber's stale initial permission. Serialize grant updates and action admission so a new mutation cannot slip through after revocation acknowledgement. On any downgrade, invalidate existing streams for that device and reconnect with the lesser grant. Terminal streaming requires Control and continues the existing exclusive-attach rule.

For local revoke/downgrade: persist the change, invalidate the in-process authorization context, and close affected streams before acknowledging success. Stop forwarding within 250 ms of a successful local change; new action admission is refused immediately by the serialized authority check. Already-dispatched actions may have irreversible effects and are not claimed to be undone. A persistence/check failure closes the affected links, refuses new actions, and reports that the change could not be durably acknowledged. Audit logging failures must never detach forwarding tasks from enforcement.

Reconcile every finding in `REMOTE_ACCESS_SECURITY_FINDINGS.md` against current source and any work delivered under `coo:949`. Include SEC-01 pairing authority, SEC-02 stale conversation grants, SEC-03 funded relay abuse, SEC-04 durable revocation, VAL-01 fail-closed cleanup, and VAL-02 mobile adapter isolation in this implementation's acceptance evidence. Credit existing fixes and test them; do not duplicate their implementation or wait for a separate planning mission before enforcing the new design.

## 6. Encrypted link, multiplexing, and gateway adapter

Use this layering on both platforms:

```text
WSS ordered binary messages (or framed LAN TCP)
  -> bounded Noise handshake/transport records
  -> authenticated reliable byte stream
  -> Yamux connection
  -> logical stream: small typed OpenService header, then existing /v2 bytes
```

WSS relay control messages are limited to admission status, peer-ready/peer-gone, and heartbeat. Treat them as untrusted reachability hints. They cannot mark a link authenticated, approve pairing, or acknowledge application work. Forward endpoint handshake and ciphertext bytes unchanged. No buffering when the opposite socket is absent; report peer unavailable and keep only the waiting host admission.

The normal Noise prologue is a canonical binary encoding of `latch-remote-link`, version 1, purpose `session`, phone/Mac roles, and the locally pinned key pair in a fixed role order. Do not send that prologue or its static-key contents to the relay. Enrollment has a separate purpose and enrollment-ID binding and does not assume a preauthorized phone pin. Protocol/purpose confusion must fail closed.

Do not send application data in handshake payloads. After successful XX completion and key validation, exchange an encrypted LinkHello containing fresh endpoint nonces, link version, selected limits, and the Mac's grant revision. Both sides verify the final context before opening application streams. Use that exchange to define a local link-generation identifier. Reconnect creates new ephemeral keys, cipher states, link nonces, and streams; never serialize/reuse Noise counters or replay ciphertext after transport loss.

Represent each Noise message as one bounded WSS binary message. For LAN TCP use a two-byte big-endian length prefix with the same validated limits. Allow at most 65,535 bytes per Noise message, with a smaller 16 KiB plaintext write quantum for normal traffic. The encrypted-stream adapter may split/reassemble Yamux bytes across records; it must not assume a Yamux frame equals a WebSocket message. Verify the selected Yamux library's limits and scheduler behavior with tests. With the `yamux` crate this means one owner task drives the connection (streams stall if it is not polled), an explicit maximum stream count, and an explicit connection receive window mapped to the 8 MiB total below; per-stream credit is the library's initial window. Write-fairness across streams must be measured, not assumed.

Reserve one encrypted control stream for link status and grant revision. Other streams begin with a length-bounded `OpenService` header allowing only `gateway` in session mode or `enrollment` in enrollment mode. The Mac returns a typed accept/refusal before gateway bytes are forwarded. No payload may name a destination host/port. Enrollment links cannot open gateway streams even after local approval; close them and establish a newly authenticated session link.

Initial bounds per endpoint link: 32 application streams, 256 KiB receive window per stream, 8 MiB total buffered plaintext/ciphertext including transport queues, and a separate bounded control budget. Configure window growth so 32 streams cannot allocate beyond the total cap. Backpressure the producer and grant receive credit only as bytes are consumed. Round-robin bounded writes prevent bulk output starving terminal input or control traffic; TCP head-of-line blocking still exists and must be measured. Cancel stalled streams after a bounded no-progress interval while permitting truly idle sessions with no queued data.

Normal logical-stream EOF must flush the final response before releasing the stream. Define and test the actual write/flush/FIN guarantees across Yamux, Noise, and WSS/TCP; a successful enqueue is not proof that the reader received the bytes. Preserve bounded graceful completion while cancellation, malformed traffic, and revoke/downgrade promptly interrupt blocked I/O. This carries forward the response-loss regression without adding a second application acknowledgement protocol or keeping SCTP code.

Shared FFI exposes `RemoteLink` lifecycle/status, `openGatewayStream`, `read`, `write`, `closeStream`, and `closeLink`; bounded reads/writes and cancellation are part of the contract. It no longer exposes candidates, ICE credentials, gather/connect sequencing, or raw Noise state. Rust owns the implementation for both macOS and iOS; Swift owns UI and Keychain integration.

Retain the mobile loopback adapter only as an internal bridge for URLSession-based clients. Bind to loopback, mint a random 256-bit capability for each listener, require it on every HTTP request/WebSocket upgrade, and strip it before forwarding. Protect it like a credential, exclude it from URLs/logs, reject untrusted callers before opening a remote stream, and rotate it on listener replacement. Test a controlled second app/process where platform access permits; protection must not rely on an assumption of iOS process isolation. This adapter is not a legacy protocol layer.

LAN uses the exact same Noise/multiplexing contract, pairing pins, current grant checks, and terminal rules. A cached/Bonjour LAN endpoint gets at most 300 ms for an authenticated attempt when opening a link; on failure cancel and join it before using the relay. The one link owner may start the relay admission (control-plane ticket request and WSS connect) concurrently with that LAN attempt and keep whichever link authenticates first, cancelling and joining the other before any application stream opens; that is a bounded race under one owner, not a second owner. Once a route is chosen, retain it until failure or deliberate disconnect. No active route upgrades, duplicate link owners, or raw `/v2` forwarding before authentication/discovery. A diagnostics-only "skip LAN attempt" setting lets the physical matrix exercise the relay path from a network where the Mac is also visible on LAN; it selects between two entry points of the same protocol and is not a dual-transport flag. Relay denial must never bypass local grants through LAN; a relay-only entitlement switch may still permit explicitly enabled LAN access.

## 7. Recovery, application delivery, and phone lifecycle

One connection state machine owns `disabled`, `connecting`, `waitingForPeer`, `authenticating`, `ready`, `backoff`, `suspended`, and terminal `revoked`/`unsupportedVersion`/`pairingRequired` states. Publish immutable state snapshots to UI. Reachability probes are hints, not proof of a working gateway.

Initial defaults: 10-second connect/handshake deadline after both peers are present; 15-second transport heartbeat; 45-second dead-peer threshold; full-jitter exponential retry from 250 ms to 15 seconds, reset after 30 seconds of healthy operation. A real path-change or foreground event triggers one immediate retry through the same owner. Authentication/pin/version/revocation failures stop automatic retry until the corresponding state changes. No nested retry owners creating parallel relay links. Planned lease renewal does not require a new Noise handshake or interrupt streams.

Foreground: acquire/restore the link, authenticate, perform `/v2` capability discovery once per new link/gateway instance, then resume eligible screens. Background: finish only permitted short work, persist UI recovery state securely, detach terminal control and stop interactive streams; allow the OS to suspend the phone. Push is not a mechanism for keeping sockets alive. Mac sleep closes availability until wake; do not promise wake-on-LAN. Provide an explicit keep-awake-while-plugged-in setting and preserve its opt-in semantics. A waiting relay socket alone must not silently keep the Mac awake.

| Operation | Interruption and recovery rule |
| --- | --- |
| Session list / capabilities / preview | Read again after reconnection; show cached results as stale while disconnected. |
| Conversation observation | Reuse `generation`, `afterRevision`, and `operationEpoch` through the existing server-first socket. The Hub chooses bounded replay or a fresh snapshot. Do not introduce a harness-event cursor. |
| Create session | Reuse one request ID for the same intent. Persist receipt before dispatch and reconcile against the actual daemon/session before permitting retry. Same ID with different payload is a conflict. |
| Send message / resolve approval | Reuse existing durable operation IDs/epochs and outcome reconciliation. An unknown outcome remains visibly uncertain; it is not a reason to submit under a fresh ID. |
| Terminal input / paste | Never replay after a lost connection. Drop unsent input on disconnect and display that input may not have been delivered. |
| Terminal attach | Reconnect only if an authenticated, bounded resume capability proves the same holder can resume without displacing a newer holder. Otherwise show Reconnect/Take Control and require the user to initiate the exclusive attach. Never steal automatically. |
| Terminal render / resize | Rebuild from the existing daemon's complete-frame/snapshot contract after a valid attach. Apply current geometry, not replayed old input or an invented output cursor. |

Do not promise exactly-once arbitrary process/connector side effects. Persisting acceptance and executing an external action are not one atomic transaction. Crash windows must yield a reconciled outcome or `uncertain`, never blind redispatch. Reuse the existing Hub journal and remote-shell request-ID machinery; extend those owners instead of adding a parallel command database.

Scope receipts to authenticated device, operation kind, target session, operation epoch, and canonical payload digest. Require durable acceptance before reporting Accepted; distinguish local draft, submitting, accepted, completed, rejected, and uncertain states. Ensure receipt retention and client retry horizons agree: do not evict a receipt while its ID is automatically retryable. Introduce an explicit not-after retry deadline and retained tombstone or advance the relevant operation epoch when eviction would otherwise make an old ID appear new. Reject stale IDs/epochs; do not turn an expired receipt lookup into a new execution. Test the existing 512-record/24-hour Hub limits under eviction and restart.

For new remote-shell requests, add a schema-first receipt status lookup if the existing API cannot reconcile a disconnected request without resubmitting it. Return only outcomes accessible to the current authorized device; revoked clients cannot query old receipts. Store no launch secrets in receipt metadata. Preserve the rule that process state is queried from latchd, not inferred from a stored receipt or PID.

APNs is new capability, not existing plumbing: the iOS target (`dev.cooperativ.latch.mobile`) has no push entitlement or entitlements file today, and the control plane has no APNs client. Objective 2 needs an APNs authentication key from the Apple Developer portal, the Push Notifications capability in the Xcode project, an environment choice (sandbox for development-signed builds, production for App Store/TestFlight signing) that matches how the tested build is signed, and the key/team/topic configuration in the deployment secret store. Creating the key and enabling the capability are owner actions; record them as such. APNs registration uses device-authenticated control-plane endpoints and stores push tokens as sensitive metadata. The Mac submits a bounded random event ID plus a generic attention notification for an approved phone when work finishes or needs input. No prompt, session title/ID, path, output, or approval details leave the endpoints. Rate-limit and deduplicate by event ID, discard invalid APNs tokens, and stop notifications on revoke/unpair. Use a visible generic notification; opening it connects to the Mac and fetches current authorized state. Treat delayed or dropped push as normal and refresh on foreground. No plaintext cloud inbox or deferred command queue is added.

The Mac notification producer observes existing Hub-normalized task/attention transitions for explicitly watched sessions, even while the phone is disconnected. Keep that observation alive through a bounded local subscription owned by the helper/gateway, not by the mobile socket. It must not take a terminal surface or parse raw harness transcripts independently. Check the phone's current Observe-or-higher grant before each event; remove watchers on unpair or disable. Persist only a bounded owner-only event-deduplication record so helper restart does not re-notify every historic transition. Where a connector cannot report a reliable completion transition, do not infer one from arbitrary output; document supported events and test them with connector fixtures and live sessions.

## 8. Services, release, and observability

Create `services/relay/` as an independent TypeScript/Node service using a maintained WebSocket server library (`ws`, with compression disabled and `maxPayload` bounded to one Noise message plus framing); own manifest, tests, container/deploy config, and operational README. Do not depend on root `packages/`. Keep its implementation to ticket verification/redemption, role-slot ownership, byte forwarding, quotas/backpressure, lease expiry, and authenticated invalidation. PostgreSQL remains in the control plane; relay buffers are memory-only. No ciphertext history store is needed.

Use the existing hosting environment unless deployment validation establishes a blocker. Confirm WSS upgrades, TCP 443 ingress, proxy idle limits, Authorization-header passthrough on upgrade, bounded message handling, disconnect propagation, health probes, and restart behavior with a real deployed test. Configure the relay and control plane with exactly one replica and with any hosting "app sleeping"/scale-to-zero feature disabled: a sleeping control plane adds its wake time to every cold open, and a sleeping relay drops every waiting Mac socket. Check whether the relay hostname publishes an AAAA record; if the hosting edge is IPv4-only, an IPv6-only phone reaches it through NAT64 and the report must say so rather than claiming native IPv6 client-to-relay. Do not assume the hosting proxy preserves sockets indefinitely. Provide `/health/live` and `/health/ready` without room enumeration. Readiness refuses new admissions during drain; shutdown closes sockets and lets endpoints reauthenticate. Deploying the relay cannot terminate local sessions.

Secrets: control-plane ticket-signing key; relay-to-control-plane service credential; authenticated invalidation channel credential; APNs key/team/topic configuration. Store server secrets in the deployment secret store and endpoint credentials in owner-only/Keychain storage. Plan key rotation with a bounded overlap of verification keys for unexpired admissions; this is credential rotation, not legacy protocol support. Operator-funded deployment limits are configured before public admission is reachable.

Record non-content endpoint metrics that separate queue/admission wait, link establishment/authentication, capability discovery, logical-stream opening, response completion, and first terminal output. Distinguish expected stream/client closure from authentication rejection, and link-ready from application-ready. Also record attempts, stage failures, authenticated connection success, reconnect time, active streams, buffer high-water marks, cancelled task counts, and bytes. Relay metrics use aggregate counts and short-lived diagnostic correlation IDs; do not log room tickets, identity keys, Authorization headers, gateway paths with session IDs, or payloads. Keep detailed correlation local/opt-in and redact exported field reports. Monitor relay egress, connection counts, admission denials, lease expiry, revocation-outbox lag, CPU/RSS, and APNs errors.

Document the cost model as measured active link-hours plus relayed bytes. Measure representative chat, idle observation, interactive terminal, and high-output command workloads before estimating a monthly cost. Do not invent prices or assume text traffic makes relay cost negligible.

## 9. Verification and acceptance gates

Run meaningful tests alongside each objective. Use actual WebSockets, shared native transport, Mac proxy, and gateway in the composed integration suite; mocks remain useful for fault injection but are not proof of the phone path.

Required automated coverage:

- Rust core and FFI: carry forward cancellation/foreign-thread destruction, blocked-writer isolation, callback/self-wait, final-response-before-close, and bounded admission-wait regressions using the new protocol; handshake vectors, both pin checks, enrollment/session purpose separation, tamper/replay/truncation rejection, wrong-version failure, key/counter freshness after reconnect, stream bounds, cancellation during every phase, and no deadlock through Swift callbacks.
- Relay/control plane: one-use ticket races, duplicate role generations, expiry/renewal, wrong issuer/audience, room cross-routing, replay after restart, absent peers, backpressure, global quota enforcement, denial on revoked/unenrolled identities, issuance-versus-revocation races, and durable outbox recovery. Run PostgreSQL tests against a disposable database, not just the in-memory store. The existing suite is gated on `TEST_DATABASE_URL` and CI provides `postgres:16`; locally the Docker daemon (OrbStack) was not running at review time, so the executing agent starts it or another throwaway PostgreSQL before claiming this coverage, and the delivery records the database it ran against.
- Host authority: malicious directory cannot enroll a key; approval precedes persistence; canceled enrollment cannot authorize; raw gateway headers cannot override identity/grant; downgrade/revoke and store/audit failure stop active traffic; every new privileged action uses current permission.
- Recovery: lose acknowledgements before/after durable acceptance and external dispatch, restart each endpoint/service, exercise receipt eviction/stale epochs, and verify no duplicate session/message/approval. An ambiguous external effect must remain ambiguous.
- Terminal: physical input/output and full-frame restore; no input replay or automatic displacement of another surface; disconnect leaves the daemon/agent running.
- Privacy: forbidden metadata absent from service schemas, DB rows, request logs, traces, errors, and push payloads; endpoint keys never enter relay claims; loopback capability enforced before any remote stream opens.
- Build/distribution: schema/fixture generation checks, Rust and Swift tests, TypeScript service/client checks, rebuilt device/simulator XCFramework, actual iPhone build, and signed/notarized coordinated Mac payload. Verify installed build identities, not just successful compilation.

Physical-device matrix: same LAN, phone cellular, unrelated Wi-Fi, and a controlled network with UDP blocked but HTTPS/WSS allowed. Include a verified IPv6-only or IPv4-degraded network with working IPv6 WSS, and record the actual client-to-relay address family separately from the peer-facing endpoint. Repeat the cold-open/reconnect counts on that scenario as well. On each perform at least 30 cold opens and 30 foreground/reconnect cycles, including session listing, creation, conversation send/approval, preview, and terminal use across the run. Separately test 20 Wi-Fi/cellular switches, 20 background/foreground cycles with long suspensions, 10 Mac sleep/wake cycles, and 10 each relay/helper/gateway restarts. Exercise at least one lease expiry, normal renewal, and control-plane outage. Record exact counts and per-attempt failure stage.

Make the matrix executable rather than a hand-tally. Definitions: a cold open is the app process launched from not-running to a usable gateway; a foreground/reconnect cycle is a return from suspension or a deliberately dropped link followed by recovery. Objective 2 ships an opt-in diagnostics runner in the app that performs foreground/reconnect cycles and the listed operations in a loop and writes content-free per-attempt stage timings; Objective 3 drives cold opens with an XCUITest harness launching the app over USB (Wi-Fi off for the cellular rows) so the counts are real launches. Report which attempts were automated and keep a manual subset per network for the terminal and approval interactions. Network recipes: macOS Internet Sharing's "Create NAT64 Network" option provides the IPv6-only scenario; a Mac or router hotspot with a firewall rule dropping UDP other than DNS provides the UDP-blocked/HTTPS-allowed scenario, with the diagnostics-only skip-LAN setting from section 6 so the relay path is the one measured. Existing `scripts/field-run.sh` and `docs/field-runs/` remain the evidence format for Mac-side coarse stream events; extend them with Objective 2 timings rather than inventing a second record.

Initial release gates (engineering targets to measure, not current promises): all eligible attempts in that defined matrix eventually succeed without re-pairing or manual app restarts; p95 cold-open to usable gateway <= 5 seconds on healthy tested networks; p95 recovery <= 5 seconds from an explicit foreground/network-restored event, and <= 60 seconds for an otherwise silent broken path. Report pairing/first launch separately. Revoked/unsupported/offline attempts are expected refusals and must be counted separately rather than silently excluded as failures. Report Mac wake recovery from the point networking is usable.

Run a 24-hour real-device/host soak with repeated streams and at least one high-output workload. After streams close, active handles and task/socket counts return to their defined baseline; RSS must plateau after warmup rather than grow with reconnect count. Capture quantitative graphs/counts and terminal latency under load. No plaintext leakage, duplicate side effects, stale privileges, or lost running sessions is acceptable. A finite passing sample is release evidence, not proof of a universal availability percentage. If a gate cannot be met, document the measured cause and proposed change; do not quietly relax it to mark the objective complete.

## 10. Ordered Overlord objectives

Mission `coo:952` contains the planning-review objective `coo:952.tskr` followed by the three large, sequential implementation objectives below; revise them in place rather than creating a duplicate. Keep auto-advance off and do not queue or launch them during review. Each objective includes its implementation, regression tests, and documentation. Do not split contracts, individual platforms, security, or ordinary test work into separate objectives when they belong to the same delivered behavior.

### Objective 1 — Replace remote transport and pairing end to end

Prerequisite: complete the specific baseline confirmation in section 1, archive it, and receive explicit replacement execution authorization; revalidate against current source and the completed fixes in section 2. Deliver sections 3–6 and the relay/control-plane portions of section 8 as one integrated slice: new schemas, separate relay service, admission/leases/outbox/entitlement, explicit endpoint pairing, shared Rust Noise/Yamux core, FFI/XCFramework, Mac helper/IPC/proxy, mobile adapter/link ownership, same-protocol LAN path, live authorization, and basic connect/disconnect UI. Address/revalidate all relevant security findings. Establish basic close/cancel/reconnect primitives needed to test the new path. Work on `main`; tag the archived baseline first. Suggested internal order (checkpoints within one delivery, not separate objectives): schemas and fixtures; Rust link core with its negative tests; relay and control-plane admission with PostgreSQL tests; helper/Desktop IPC and proxy; mobile link owner and capability-protected adapter; LAN entry point; composed integration; then removal. Remove superseded ICE/STUN/TURN source, candidate signaling, `vendor/webrtc-ice` and the `webrtc-*`/TURN dependencies, old QR trust flow, the Swift Noise/BLAKE2s implementation, `latch`'s own handshake code, obsolete remote compatibility shims, and their exclusive dependencies from the repository after composed integration passes and after porting the transport-independent regressions listed in section 11 item 7. Update source docs alongside removal; preserve irreplaceable fixtures and historical investigations. The installed production remote path may break during this work; local sessions and user data may not. The capability-protected loopback adapter and VAL-02 evidence belong here; Objective 2 owns only its lifecycle across background/foreground. Completion requires real loopback WSS integration through both endpoints to the gateway, malicious/unauthorized-path and PostgreSQL regressions, bounded resources/cancellation, successful native builds, and a reviewable staged deployment configuration. No separate protocol-only or service-only delivery counts as completion.

### Objective 2 — Complete resilient mobile sessions and attention notifications

Depends on Objective 1. Deliver section 7 on the new link: centralized retry/foreground/network state, capability refresh, Hub revision/snapshot recovery, durable create/message/approval reconciliation and safe retention, current-grant action admission throughout recovery, terminal resume/ownership/input rules, loopback-adapter replacement and capability rotation across background/foreground (the adapter itself is Objective 1 work), Mac sleep/wake status and opt-in keep-awake behavior, generic APNs registration/delivery/revocation with the owner-side prerequisites named in section 7, clear offline/uncertain/delivery UI, and the diagnostics runner from section 9 that Objective 3 needs to execute the matrix. Extend existing owners rather than duplicate them. Complete composed fault-injection tests for each interruption/crash boundary and endpoint/service privacy checks. Completion requires repeatable recovery without duplicate execution, automatic terminal takeover, or stale permissions, with notification behavior tested against APNs where credentials/device access are available; missing live evidence remains explicitly outstanding for Objective 3.

### Objective 3 — Deploy, verify on physical devices, and retire old infrastructure

Depends on Objective 2. Deliver sections 8–9 and the coordinated cutover below: provision the independently deployed relay and secrets/quotas/alerts, deploy the compatible control plane, ship/rebuild/install matching native clients and signed Mac payload, re-pair the owner's devices, run the complete physical matrix (with the XCUITest cold-open harness and diagnostics runner) and soak, fix demonstrated failures, publish measured performance/reliability/cost/privacy evidence and operations/runbooks, remove old deployed TURN resources, provider credentials, hosting variables, control-plane TURN/presence/rendezvous tables and endpoints by forward migration, and stale setup instructions, and verify the final repository/deployment has one supported implementation. Source-level ICE removal already happened in Objective 1; here confirm it and that the ported regressions exist rather than deleting code again. Completion requires actual device and deployed-service evidence, not a checklist of commands for the user to run later. Record unavoidable human steps such as iPhone interaction distinctly and keep the objective open until required evidence exists. Do not delete resources until their ownership and lack of other consumers are confirmed.

Three implementation objectives keep the transport/authentication change coherent, then establish application recovery on a stable boundary, then validate and cut over the whole system. The planning-review objective `coo:952.tskr` produced this revision; there is no further planning objective.

## 11. Coordinated cutover and removal checklist

1. Retain the archived source tag and section 1 baseline field report. The four-run physical retest of the final handover helper is complete. Save an owner-only backup of remote configuration needed for deliberate rollback; do not copy session secrets into reports.
2. Finish implementation and local/staging verification before installing the new Mac payload and phone build. Control-plane pushes during the build already redeploy the production service; that is accepted. No long-lived production feature flags or dual protocol servers are required.
3. Install the independent relay with default-deny admission, provision secrets and budgets, and validate TLS/WSS connectivity. Update control-plane schema/configuration. Keep destructive legacy credential/table removal until the verification point below; migrations remain forward-only.
4. In a coordinated maintenance window, disable remote access, update all endpoints and the signed helper/CLI/daemon bundle, and enable the new service. A brief remote-access outage is acceptable. Existing latchd sessions and conversation data must remain intact.
5. Invalidate old remote pairings/credentials and show Pairing required. Enroll the owner through the new explicit approval flow. Run smoke tests, then the complete matrix/soak. Remove old remote configuration by narrowly scoped migration; never wipe `~/.latch` or session directories.
6. If the new release fails a gate, repair it or deliberately restore the archived coordinated release with its appropriate configuration. Do not opportunistically downgrade a live authenticated connection. Do not resurrect revoked credentials/grants from backup. Any necessary re-enrollment is explicit; active sessions remain protected.
7. After passing, remove old deployed TURN resources, unused secrets/variables, ICE endpoints/presence storage, helper offer files, and stale build inputs. In-repository removal of `webrtc-*`/TURN dependencies, transport-only patches, and `vendor/webrtc-ice` with its STUN-queue/IPv6/gathering patches happened in Objective 1; verify actual consumers before deleting any remaining crate or vendor tree. Port transport-independent regression intent (cleanup, queue isolation, complete response delivery, family reachability, and admission handover) to the new implementation before removing old transport tests. Preserve unrelated vendor fixes and terminal fixtures. Remove obsolete v1/legacy remote adapters only after updating every in-repository consumer.
8. Update `DECISION_REMOTE_ACCESS_TRANSPORT.md`, `ARCHITECTURE_RULES.md`, remote Desktop/Mobile setup, threat model, SDK docs, service READMEs, release scripts, and field-verification docs to describe the final supported system. Mark old plans as historical/superseded with links rather than claiming old test results validate the replacement.

Deferred deliberately: direct internet optimization, multi-region relay sharding/failover, cloud transcripts, cloud offline command queues, cloud execution, transparent terminal byte replay, VPN integration, and compatibility with old installations. None is needed to meet this mission's completion criteria.

## 12. Research basis and evidence limits

- [OpenAI remote connections](https://learn.chatgpt.com/docs/remote-connections): documents explicit device pairing, host-local execution, and a secure relay. It does not establish OpenAI's wire protocol or endpoint encryption implementation.
- [Claude Remote Control](https://code.claude.com/docs/en/remote-control): documents outbound HTTPS, scoped credentials, and server-stored transcripts for synchronization. Latch adopts outbound reachability but keeps session content on endpoints.
- [Claude Dispatch](https://support.claude.com/en/articles/13947068-assign-tasks-from-anywhere-in-claude-cowork): documents mobile-triggered local work and attention notifications.
- [Tailscale connection types](https://tailscale.com/docs/reference/connection-types): demonstrates encrypted relay connectivity with direct paths as an optimization; embedding Tailscale is not selected here.
- [Apple background strategies](https://developer.apple.com/documentation/BackgroundTasks/choosing-background-strategies-for-your-app): foreground recovery and notifications must respect OS-controlled background execution.
- [Noise specification](https://noiseprotocol.org/noise.html), [snow](https://docs.rs/snow/latest/snow/), and [yamux](https://docs.rs/yamux/latest/yamux/): implementation references for established protocol machinery. Pin and validate actual selected versions during Objective 1; this plan does not claim a new cryptographic audit.

The relay design, timing/buffer limits, objective boundaries, and acceptance thresholds above are Latch design decisions. Vendor documentation does not prove they will meet the targets. Implementation tests and physical-device evidence must establish that.

## 13. Review findings from `coo:952.tskr` (7 September 2026)

Problems found in the earlier revision and how this revision resolves them:

| # | Problem | Resolution in this revision |
| --- | --- | --- |
| 1 | The control plane deploys from `main` on every push touching `services/control-plane/`, so "keep production untouched" was not achievable while committing Objective 1 work to `main`. | Owner decision: production need not be preserved while building; all objectives complete before production use resumes. Section 1 now says so, work proceeds on `main`, and only the baseline tag and archived release remain as rollback references. (An earlier revision required a separate branch/worktree; withdrawn.) |
| 2 | Objective 1 and Objective 3 both claimed removal of `vendor/webrtc-ice` and ICE code, with contradictory timing. | Sections 10 and 11: Objective 1 removes source after composed integration and regression porting; Objective 3 removes deployed/cloud resources and verifies. |
| 3 | Objective 1 and Objective 2 both claimed the capability-protected loopback adapter. | Objective 1 owns the adapter and VAL-02 evidence; Objective 2 owns its lifecycle/rotation. |
| 4 | SEC-01 protection against a malicious control plane depended on the owner comparing words; the admission code was the only secret and it is disclosed to the service. | Section 5: a QR-only enrollment secret mixed into the enrollment Noise prologue; comparison words become confirmation. |
| 5 | Lease renewal delivery to the relay was unspecified. | Section 4: signed lease-extension claim forwarded by the endpoint over the WSS control channel; invalidation via outbox to the relay's private endpoint. |
| 6 | "System certificate validation" had no iOS-viable library named, and the relay's TLS position behind a terminating edge was unstated. | Section 4 and 8: `native-tls`/Security.framework or `rustls-platform-verifier`; relay serves plain WS behind the edge and direct TLS for tests. |
| 7 | Closing anonymous account creation breaks Desktop's existing bootstrap with no replacement named. | Section 4: operator-minted one-use owner invitation exchanged by Desktop. |
| 8 | APNs was written as if the plumbing existed; the app has no push entitlement and the owner must create the key. | Section 7: prerequisites and owner actions recorded; Objective 2 text updated. |
| 9 | The physical matrix (roughly 300 cold opens/cycles plus soak) was a manual tally with no method for the IPv6-only or UDP-blocked networks. | Section 9: definitions, in-app diagnostics runner, XCUITest cold-open harness, NAT64 and firewall recipes, skip-LAN diagnostics setting. |
| 10 | Component ownership was ambiguous: `latch` currently holds Noise, the LAN listener, and Bonjour, while the plan gave LAN to the helper and Noise to the shared core. | Section 2: one Rust handshake implementation in `latch-transport`; `latch` consumes authenticated streams and drops its own. |
| 11 | Disposable PostgreSQL coverage assumed an available database; the local Docker daemon was not running. | Section 9: gate and CI facts recorded; executing agent provides a throwaway database and records it. |
| 12 | Security-finding credit for `coo:949` was left to the implementer. | Section 2: verified at `a2ab11d` that nothing was delivered; all findings remain open. |
| 13 | Hosting details that affect the 5-second gate (scale-to-zero, replica count, header passthrough, AAAA) were not called out. | Section 8: explicit deployment checks. |

Unchanged on purpose: the three-objective structure, the acceptance thresholds, the single-user clean replacement, and the execution prerequisite. The later owner confirmation passed that prerequisite and launched Objective 1.

## 14. Objective 1 implementation record

Objective `coo:952.ay7b` delivered the secure transport and enrollment slice on
7 September 2026:

- shared Rust WSS/LAN + Noise XX + Yamux implementation in `latch-transport`,
  driven by `latch-remote` and exposed to iOS through the generated FFI;
- a separate bounded opaque relay with single-use redemption, heartbeat,
  lease extension, role replacement, backpressure, draining, and authenticated
  invalidation;
- control-plane enrollment, admission, owner invitations, entitlement, grant
  revision, durable invalidation outbox, and atomic PostgreSQL persistence;
- exact-key Desktop approval, current-only helper supervision, live local grant
  enforcement, and the fixed capability-protected phone loopback adapter;
- schema/fixture mirrors and real local IPv4 and IPv6 TLS/WSS composed paths
  through both authenticated endpoints to the authorized gateway; and
- source removal of the previous transport, candidate signaling, duplicated
  Swift cryptography, compatibility adapters, exclusive dependencies, and
  `vendor/webrtc-ice`.

Verification passed for Rust unit/integration/doc tests (including 117 `latch`
tests, all 15 `latchd` end-to-end tests, 5 transport tests, 2 FFI tests, and 2
composed WSS tests), Desktop's 60 Swift tests, Mobile's 127
Swift/native/terminal tests, relay typecheck/build/5 tests,
control-plane typecheck and 36 tests against disposable PostgreSQL 17 in
Docker at `127.0.0.1:55439/latch_test`, contract fixtures, and an unsigned iOS
Simulator app build linking the generated XCFramework.

This is local implementation evidence, not deployed WSS or physical-phone
release evidence. Objective 2 remains responsible for recovery, lifecycle,
notifications, and the timing runner. Objective 3 remains responsible for
service deployment, signed payloads, re-pairing, the physical network matrix,
soak, p95 results, and retired cloud-resource deletion.

## 15. Objective 2 implementation record

Objective `coo:952.95p7` delivered the recovery and mobile-lifecycle slice on
8 September 2026. Material design changes against sections 6–7, recorded here
so section 7 is read together with this list:

- **Transport liveness is in the Noise layer, not the relay.** Every link
  sends an empty authenticated record after 15 s idle and declares the peer
  dead after 45 s of silence, on WSS and LAN alike; the relay's WebSocket ping
  remains a second, independent check. Timings are per-link so tests run them
  in milliseconds. LAN framing is now cancel-safe.
- **Peer-wait precedes the handshake deadline.** Both roles wait for the
  relay's `peer_ready` before the 10-second Noise deadline starts: the host
  waits without bound (its lease is the bound), the phone for 12 s and then
  reports the Mac offline. This removed the helper churn that Objective 1's
  host loop had (a helper alone in its room expired every 10 s and Desktop
  restarted it on a 1–30 s schedule, so a phone arriving between windows
  waited up to 30 s).
- **The helper survives link loss.** `latch-remote` keeps its gateway child,
  LAN listener, and Bonjour record for its whole life; a lost or replaced link
  closes only that link's streams and the helper prints `admission_needed`.
  Desktop answers with a fresh single-use `admission` over the existing stdin
  IPC (with bounded backoff while the control plane is unreachable) rather than
  restarting the process. A newly authenticated link on either carrier
  replaces the current one; `status` lines (`lan_ready`, `connecting`,
  `waiting_for_peer`, `authenticating`, `ready`, `link_closed`, `offline`)
  drive Desktop's connected-peer count.
- **Link closure is observable without a stream.** `SecureLink::closed()` and
  the FFI `waitClosed()`/`isClosed()` let the app-scoped owner schedule
  reconnection; FFI errors are typed (`PeerUnavailable`, `Authentication`,
  `Timeout`) and stage timings (`connect`, `peerWait`, `authenticate`) are
  returned for diagnostics.
- **One iOS link owner.** `RemoteLinkCoordinator` owns connect, full-jitter
  backoff (250 ms to 15 s, reset after 30 s healthy), foreground and
  network-path triggers (one immediate retry; a link that looks alive is
  probed with a bounded discovery and replaced if it fails), typed states
  (`connecting`, `ready`, `backoff`, `macOffline`, `suspended`, `revoked`,
  `pairingRequired`), and channel requests that wait for a connecting link
  instead of opening their own. Authentication and revocation stop automatic
  retry. Discovery runs once per authenticated link generation; a changed
  gateway instance id re-bases conversation sockets. Cached session rows stay
  on screen marked stale while the link is down.
- **Capability rotation.** Backgrounding stops the loopback adapter (its
  random capability and ephemeral port die with it) and closes the native
  link; foreground starts a new adapter with a new capability before the same
  owner resumes.
- **Terminal resume is a bounded capability.** The gateway sends an
  `attached` control frame with a 64-hex resume capability before any pane
  byte. It is honoured for 60 s after the gateway loses the socket without a
  close frame, only for the same device, and only if the daemon reports no
  live surface; a refusal is close code 4411 `resume_refused`, which never
  steals. A deliberate detach, a reasoned close, or a downgrade discards the
  capability. Input typed at an interrupted surface is dropped and flagged as
  possibly undelivered; nothing is ever replayed. Schema: `terminal-connection`
  `resume` query, `attachedFrame`, `resume_refused`.
- **Device- and payload-scoped receipts.** The loopback proxy injects
  `x-latch-device-id` on the fixed internal hop (trusted from loopback only).
  Hub operation records carry the device and an action digest: another device
  or a different payload reusing an id is refused, the owner gets the recorded
  outcome, and `operation_status` returns a retained receipt or `unknown`
  (which clients treat as needing review, never as new work). Evicting a
  record inside the 10-minute client retry horizon rotates the operation epoch
  and persists it, so an old id can never look new. Remote-shell creation
  writes a durable receipt (device, directory, status) before dispatch;
  another device presenting the id gets 403 `request_id_foreign`.
- **Attention notifications.** The gateway keeps a bounded Hub subscription
  for sessions a phone has opened (16 per device, 8 devices, persisted) and
  spools one content-free event per Working→Idle/Exited or →AwaitingInput
  transition, deduplicated across restarts, with 10-minute expiry. Desktop
  forwards spool entries through `POST /v1/attention`; the control plane
  stores the opaque APNs token per controller (`PUT/DELETE
  /v1/push-registrations`), deduplicates by host and event id, sends the fixed
  sentence over HTTP/2 with ES256 provider tokens, removes tokens Apple reports
  invalid, and removes them on revoke or unpair. Migration
  `0006_push_attention.sql`. The iOS target has `aps-environment` development
  entitlements and the Push capability; the app registers after alert
  permission and re-registers on foreground.
- **Keep-awake.** Desktop's opt-in setting prevents idle sleep only while the
  Mac is on external power and a helper reports `ready`; a waiting relay
  socket never holds the assertion.
- **Diagnostics runner.** Settings → Diagnostics runs real suspend/resume
  cycles through the owner and records per-attempt stages (`admission`,
  `connect`, `peerWait`, `authenticate`, `linkReady`, `discovery`,
  `applicationReady`, `streamOpen`, `responseComplete`, `sessionList`,
  `preview`, optional `terminalFirstOutput`) as JSON Lines under the app's
  Documents/latch-diagnostics, with the diagnostics-only skip-LAN setting.

Verification passed locally: transport 8 unit tests (keepalive/dead-peer,
closed signal, cancel-safe LAN), FFI 2, composed real-TLS WSS 2 plus recovery
4 (relay kill and re-admission through the same gateway, controller
replacement, Mac-offline bound, foreign-thread close with a blocked writer),
`latch` 125 plus 15 kernel tests (terminal resume/refusal, Hub scoping and
epoch rotation, creation receipts, attention spool), relay 5, control plane 40
in memory and 44 on disposable PostgreSQL 17 (`postgres:17-alpine` at
`127.0.0.1:55439/latch_test`), Desktop 64, Mobile kit suites including the
coordinator, recovery, and app-model recovery tests, plus the regenerated
XCFramework build.

Not delivered here and carried to Objective 3 explicitly: live APNs delivery
evidence (the owner must create an APNs authentication key, keep the Xcode
Push capability on the App ID, and place `APNS_KEY_ID`/`APNS_TEAM_ID`/
`APNS_PRIVATE_KEY_PEM`/`APNS_TOPIC`/`APNS_ENVIRONMENT` in the deployment
secret store; the sandbox environment matches the development-signed build),
and every physical-device measurement. Nothing here is deployed; the working
tree is uncommitted.

## 16. Objective 3 implementation record

Objective `coo:952.wfc0` performed the coordinated cutover on 8 September
2026 after the owner granted commit, push, and deployment permission for the
shared checkout. Material facts and design changes against sections 8–11:

- **Deployed services.** Railway project `latch`: the control plane (`Latch`)
  redeployed from `main` at `0e5d4c5` and reports 7 applied migrations with
  `relayConfigured: true`; a new `latch-relay` service (root `services/relay`,
  one replica, app sleeping disabled, readiness on `/health/ready`) at
  `wss://latch-relay-production.up.railway.app/v1/connect`, source connected
  to `main`. Secrets (Ed25519 admission key `latch-remote-link-2026-09a`,
  relay service token, invalidation secret, operator secret) were minted for
  this deployment and placed only in Railway's variable store. Verified from
  the Mac: relay live/ready; WebSocket upgrade answers 401 without a token and
  403 with a forged one, so the `Authorization` header traverses the edge
  intact (note: `curl` must use HTTP/1.1 for this probe; over HTTP/2 the edge
  does not attempt an upgrade); the certificate validates with the system
  trust store; the relay hostname publishes an A record and no AAAA, so an
  IPv6-only phone reaches it through carrier NAT64 and the field report
  reports that family, not native IPv6.
- **Forward migration `0007_retire_ice_signaling.sql`** drops the
  `turn_credentials`, `rendezvous_offers`, `presence`, `relay_tickets`, and
  `pairing_requests` tables and revokes every pairing and phone identity with
  no `remote_links` row, keeping the Mac identity and owner account. The
  Desktop therefore kept its stored account and host tokens and needed no
  owner invitation; every phone must re-enrol. A PostgreSQL regression proves
  the drop and the revocation predicate. The `pg_dump` taken for rollback
  ran after the platform's own deploy had already applied the migration, so
  it is a post-cutover backup; the retired tables exist only in Railway's
  platform backups, and the rollback runbook says so.
- **Mac payload.** Version `0.2609080610.0` was built, Developer ID signed,
  notarized, installed with the supervising Desktop quit first, and tagged
  (`v0.2609080610.0`, CLI archives published by the release workflow). A
  concurrent session in the shared checkout then bumped the workspace to
  `0.2609080625.0` while the Desktop poll fix below was being rebuilt, so the
  coordinated installed release is `0.2609080625.0`: `latch`
  `3501b38a3d329d44b0f873cff9fe3b00eee36e9e6e2dcfc67fa17863bba68e92`,
  `latch-remote`
  `8e6900a82eab41a5f8c7a9547e7002ea6bc5e810095e45441316db618f46d5b5`,
  `latchd` `e8a903fd2a04075ea7fe4ce607f17852b8cce796eee6116c2f3c082e9c1ebbb9`,
  Desktop executable
  `6cb882f1132a3f2a34d32856bb8bf9d1e77755eb9614d0a89291ff4f4cf6df66`
  (notarized, stapled). All running `latchd` sessions survived both swaps.
  The ICE-baseline binaries and app bundle and the intermediate
  `0.2609080610.0` bundle are kept under `~/.latch/remote-access/rollback/`.
  The locally built payload is the installed one and is identified by these
  hashes, not by the CI archive of the same tag. The two remaining active
  retired-protocol device records on the Mac were revoked with
  `latch remote-access revoke`, matching the directory.
- **Phone build.** The iPhone build is signed by the wildcard team profile,
  which lacks Push Notifications, and Xcode has no signed-in account to
  update it. An interim build without the `aps-environment` entitlement was
  installed (`dev.cooperativ.latch.mobile` 0.1.0, iOS 27.0 SDK). Live APNs
  remains blocked on the owner actions in section 7 and is carried in the
  field report as outstanding, not waived.
- **Cold-open harness.** The plan named an XCUITest harness; the delivered
  `scripts/phone-diagnostics.sh` launches the real app from not-running over
  USB with `xcrun devicectl` and the app records one `cold_open` line per
  process from the kernel's process start time (`ColdOpenRecorder`, stage
  `launch`). Reconnect cycles are started by launch argument so the in-app
  runner needs no tap. `scripts/diagnostics_summary.py` produces per-stage
  p50/p95/max and `scripts/field-run.sh finish --phone-log` embeds them in
  the Mac-side record. The reasons for the substitution are in the field
  report.
- **Demonstrated failure at first pairing.** The owner's first four pairing
  attempts ended with the Mac reporting success and the phone reporting "the
  secure connection closed before the request completed". The helper
  Desktop launched for the new link exited immediately with `revoked
  controller`: the phone keeps its identity key across reinstall and
  re-enrollment, the retired-protocol record for that same key had been
  revoked during cutover, and `lookup_device` returned the first record
  matching the key, so the authority refused a controller the owner had just
  approved. The fix makes a key resolve to its active record (enrollment
  already refuses a second active record per key) and only to the newest
  revoked one when no active record exists; a regression test re-enrols a
  revoked key and checks both outcomes. This shipped as the follow-up payload
  recorded below; nothing in the relay, control plane, or phone changed.
- **Demonstrated fix.** The new Desktop polled `GET /v1/remote-links` every
  two seconds while no phone was linked (observed in the control-plane request
  log after install); the idle re-check is now 20 seconds and enrollment
  still restarts supervision immediately.
- **Workflow fix.** The control-plane GitHub Actions deploy step uploaded the
  repository checkout to a service whose root is `services/control-plane`, so
  Railpack found nothing and the explicit deploy failed on every push while
  the platform's own GitHub integration deployed correctly; the step now
  passes `--path-as-root`.
- **Relay key rotation** gained the bounded previous-key overlap
  (`ADMISSION_PREVIOUS_KEY_ID`/`ADMISSION_PREVIOUS_PUBLIC_KEY_PEM`) the
  runbook needs.
- **Retired cloud resources.** The control plane's TURN provider variables,
  presence and rendezvous TTLs, and the Cloudflare TURN key are deleted only
  after the matrix passes, per section 11 item 7; until then they are
  inert (no code reads them) and listed in the operations document.

Operations, runbooks, and the cost model are in
`docs/REMOTE_LINK_OPERATIONS.md`; the matrix procedure, gates, identities,
and results table are in `docs/REMOTE_ACCESS_FIELD_VERIFICATION.md`. The
physical matrix, soak, and APNs delivery require the owner at the phone and
are recorded there as they run.
