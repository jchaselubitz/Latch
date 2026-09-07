# Secure relay remote access implementation plan

Date: 7 September 2026. Status: approved direction; implementation not started.

Overlord mission: **coo:952 — Replace remote access with an encrypted relay and resilient mobile sessions**. Objective order: `coo:952.ay7b`, `coo:952.95p7`, `coo:952.wfc0`. The first objective is draft, the later objectives are future, and auto-advance is disabled for all three.

## 1. Goal, timing, and decisions

Replace Latch's required internet ICE/STUN/TURN path with reusable, outbound WSS connections through an opaque relay. A phone must be able to discover, create, observe, and control sessions on its paired Mac across ordinary internet connections, recover from interruptions, and receive attention notifications. Execution, authoritative session state, device grants, and plaintext stay on the endpoints.

**Execution prerequisite:** finish the current connection investigation first and record repeated physical-device success with source/build/network provenance. Read `REMOTE_CONNECTION_DEBUGGING_HANDOFF.md`, its successor findings, and the current agent's delivery before implementation. The historical investigation is tracked under Overlord `coo:940`, but that mission's board status alone does not prove the current issue is resolved. This plan does not authorize interrupting that investigation or launching replacement implementation now.

**Single-user clean replacement:** the owner is currently the only user. Upgrade Desktop, the signed CLI/helper payload, Mobile, control plane, and relay together. Require new pairing. Do not implement mixed-version operation, dual transport feature flags, old pairing migration, old wire-format negotiation, a v1 gateway compatibility adapter, or automatic fallback to ICE. Preserve actual sessions, repositories, conversations, and user files. Remove obsolete remote code and dependencies when the replacement works in the implementation checkout. Retain an archived release as an operational rollback option, not an active compatibility layer.

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

Planning inspection included HEAD `1d73ecc7500f5a9abc9096850ac1e2bcd952b237` and a concurrently changing worktree. This is a design baseline, not a verified installed-build statement. Re-read the affected source at execution; preserve the other agent's edits, including transport dependency patches. Never edit, stage, revert, or delete `.overlord/project.json`.

| Existing component | Implementation use |
| --- | --- |
| `crates/latch-remote/src/main.rs`, `ice.rs` | Replace ICE responder and offer polling with relay/LAN link supervision; keep the dedicated helper process. |
| `crates/latch-transport`, `crates/latch-transport-ffi` | Replace RTC API with shared Rust secure-link and stream API; regenerate UniFFI and XCFramework. |
| `crates/latch/src/cli/remote_access.rs` | Retain local device store, fixed gateway supervision, request authorization, and audit; refactor proxy input to authenticated logical streams. |
| `apps/LatchDesktop/.../RemoteAccessController.swift`, `RemoteAccessSupervisor.swift` | Pairing UI, local grant management, helper lifetime, typed readiness/status, and remote-access settings. |
| `apps/LatchMobile/.../GatewayTransport.swift`, `PairedRoute.swift`, `NativeRemoteTransport.swift` | Single link owner and logical stream adapter for the existing gateway clients. |
| `apps/LatchMobile/.../Noise.swift`, `Support/Blake2s.swift` | Retire duplicated phone-side protocol implementation after shared-core parity tests. Remove helpers only after confirming no other consumer. |
| `services/control-plane/src/api.ts`, `store/`, `credentials.ts` | Enrollment metadata, room admission, leases, durable revocation, entitlement, and push routing. |
| `crates/latch/src/conversation/{hub,cache,projection}.rs` | Reuse durable operations and Hub-owned recovery; fill demonstrated gaps only. |
| `crates/latch/src/cli/create.rs`, `cli/serve/http.rs` | Extend existing request-ID-based remote shell creation and crash reconciliation. |
| `ConversationSocket.swift`, `ConversationStore.swift`, `TerminalSession.swift` | Reuse conversation recovery and terminal ownership behavior. |
| `schemas/remote-access/v2`, `fixtures/conversation`, `packages/client` | Keep schema generation, clients, and gateway fixtures aligned. |

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

Use maintained WSS/TLS and token-signature libraries. Pin versions after checking current platform support and advisories during implementation. Use system certificate validation; never ship an insecure TLS option. For test certificates, use an explicit test CA in test-only configuration. Disable WebSocket compression and body/header logging on relay traffic.

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

A redeemed connection gets a ten-minute lease; endpoint renewal starts at five minutes. Renewal only extends the same room/role/generation after authorization succeeds. Deliver the renewal to the relay through the authenticated private channel. On expiry, the relay closes the socket even if the control plane is unavailable. A control-plane outage therefore prevents new admissions, while established links last only to their current lease deadline. Local authorization continues throughout. These are chosen initial limits, not measured guarantees.

Revocation updates authorization and writes an outbox entry in one PostgreSQL transaction. Workers send idempotent room/lease invalidations to the relay and retain entries until acknowledged or a justified hard lease expiry. Retry through process restart and partial failures. The relay closes active rooms on invalidation; stale signed tickets remain unusable because redemption rechecks current state. Include issuance-versus-revocation races in database tests.

Close public anonymous funded account creation. For this single-user product, provision the owner's account through an operator-authenticated path and one-use enrollment invitations; no general OAuth product is required. Enforce per-owner, per-device, per-IP, and deployment-wide admission/concurrency/bandwidth budgets. Identity churn must not reset aggregate budgets. An entitlement kill switch stops new leases and invalidates existing ones. Configure provider spending alerts in addition to application limits.

## 5. Pairing and live authority

Re-pair all devices at cutover. Never automatically import old cloud directory rows into the local grant store.

1. On the unlocked Mac, the owner starts pairing. Generate an enrollment ID and one-use high-entropy admission code, and open a short-lived provisional room. The QR contains the link version, trusted control-plane origin, enrollment ID, Mac static public key, and admission code. Treat the code as relay admission only: disclosure to the service must not convey local authority.
2. The phone validates the supported origin/version, pins the Mac key from the scanned QR, generates or loads its private identity from Keychain, claims provisional admission, and establishes the enrollment Noise link. The phone verifies the Mac static key before sending sensitive enrollment content. The Mac knows this is a provisional, unauthenticated-for-gateway phone.
3. Inside that encrypted link, the phone submits its identity and requested grant bound to the exact enrollment ID. Both devices display comparison words derived from the Noise handshake hash and the canonical proposed grant. Bind purpose, version, enrollment ID, endpoint roles, and requested grant to the enrollment exchange. Use a fixed reviewed comparison encoding with at least 64 bits of comparison entropy; owner confirmation must compare both displays, not approve an arbitrary directory row.
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

Represent each Noise message as one bounded WSS binary message. For LAN TCP use a two-byte big-endian length prefix with the same validated limits. Allow at most 65,535 bytes per Noise message, with a smaller 16 KiB plaintext write quantum for normal traffic. The encrypted-stream adapter may split/reassemble Yamux bytes across records; it must not assume a Yamux frame equals a WebSocket message. Verify the selected Yamux library's limits and scheduler behavior with tests.

Reserve one encrypted control stream for link status and grant revision. Other streams begin with a length-bounded `OpenService` header allowing only `gateway` in session mode or `enrollment` in enrollment mode. The Mac returns a typed accept/refusal before gateway bytes are forwarded. No payload may name a destination host/port. Enrollment links cannot open gateway streams even after local approval; close them and establish a newly authenticated session link.

Initial bounds per endpoint link: 32 application streams, 256 KiB receive window per stream, 8 MiB total buffered plaintext/ciphertext including transport queues, and a separate bounded control budget. Configure window growth so 32 streams cannot allocate beyond the total cap. Backpressure the producer and grant receive credit only as bytes are consumed. Round-robin bounded writes prevent bulk output starving terminal input or control traffic; TCP head-of-line blocking still exists and must be measured. Cancel stalled streams after a bounded no-progress interval while permitting truly idle sessions with no queued data.

Shared FFI exposes `RemoteLink` lifecycle/status, `openGatewayStream`, `read`, `write`, `closeStream`, and `closeLink`; bounded reads/writes and cancellation are part of the contract. It no longer exposes candidates, ICE credentials, gather/connect sequencing, or raw Noise state. Rust owns the implementation for both macOS and iOS; Swift owns UI and Keychain integration.

Retain the mobile loopback adapter only as an internal bridge for URLSession-based clients. Bind to loopback, mint a random 256-bit capability for each listener, require it on every HTTP request/WebSocket upgrade, and strip it before forwarding. Protect it like a credential, exclude it from URLs/logs, reject untrusted callers before opening a remote stream, and rotate it on listener replacement. Test a controlled second app/process where platform access permits; protection must not rely on an assumption of iOS process isolation. This adapter is not a legacy protocol layer.

LAN uses the exact same Noise/multiplexing contract, pairing pins, current grant checks, and terminal rules. A cached/Bonjour LAN endpoint gets at most 300 ms for an authenticated attempt when opening a link; on failure cancel and join it before using the relay. Once a route is chosen, retain it until failure or deliberate disconnect. No active route upgrades, duplicate link owners, or raw `/v2` forwarding before authentication/discovery. Relay denial must never bypass local grants through LAN; a relay-only entitlement switch may still permit explicitly enabled LAN access.

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

APNs registration uses device-authenticated control-plane endpoints and stores push tokens as sensitive metadata. The Mac submits a bounded random event ID plus a generic attention notification for an approved phone when work finishes or needs input. No prompt, session title/ID, path, output, or approval details leave the endpoints. Rate-limit and deduplicate by event ID, discard invalid APNs tokens, and stop notifications on revoke/unpair. Use a visible generic notification; opening it connects to the Mac and fetches current authorized state. Treat delayed or dropped push as normal and refresh on foreground. No plaintext cloud inbox or deferred command queue is added.

The Mac notification producer observes existing Hub-normalized task/attention transitions for explicitly watched sessions, even while the phone is disconnected. Keep that observation alive through a bounded local subscription owned by the helper/gateway, not by the mobile socket. It must not take a terminal surface or parse raw harness transcripts independently. Check the phone's current Observe-or-higher grant before each event; remove watchers on unpair or disable. Persist only a bounded owner-only event-deduplication record so helper restart does not re-notify every historic transition. Where a connector cannot report a reliable completion transition, do not infer one from arbitrary output; document supported events and test them with connector fixtures and live sessions.

## 8. Services, release, and observability

Create `services/relay/` as an independent TypeScript/Node service using a maintained WebSocket server library; own manifest, tests, container/deploy config, and operational README. Do not depend on root `packages/`. Keep its implementation to ticket verification/redemption, role-slot ownership, byte forwarding, quotas/backpressure, lease expiry, and authenticated invalidation. PostgreSQL remains in the control plane; relay buffers are memory-only. No ciphertext history store is needed.

Use the existing hosting environment unless deployment validation establishes a blocker. Confirm WSS upgrades, TCP 443 ingress, proxy idle limits, bounded message handling, disconnect propagation, health probes, and restart behavior with a real deployed test. Do not assume the hosting proxy preserves sockets indefinitely. Provide `/health/live` and `/health/ready` without room enumeration. Readiness refuses new admissions during drain; shutdown closes sockets and lets endpoints reauthenticate. Deploying the relay cannot terminate local sessions.

Secrets: control-plane ticket-signing key; relay-to-control-plane service credential; authenticated invalidation channel credential; APNs key/team/topic configuration. Store server secrets in the deployment secret store and endpoint credentials in owner-only/Keychain storage. Plan key rotation with a bounded overlap of verification keys for unexpired admissions; this is credential rotation, not legacy protocol support. Operator-funded deployment limits are configured before public admission is reachable.

Record non-content endpoint metrics for attempts, stage failures, authenticated connection success, time to capability discovery/first output, reconnect time, active streams, buffer high-water marks, cancelled task counts, and bytes. Relay metrics use aggregate counts and short-lived diagnostic correlation IDs; do not log room tickets, identity keys, Authorization headers, gateway paths with session IDs, or payloads. Keep detailed correlation local/opt-in and redact exported field reports. Monitor relay egress, connection counts, admission denials, lease expiry, revocation-outbox lag, CPU/RSS, and APNs errors.

Document the cost model as measured active link-hours plus relayed bytes. Measure representative chat, idle observation, interactive terminal, and high-output command workloads before estimating a monthly cost. Do not invent prices or assume text traffic makes relay cost negligible.

## 9. Verification and acceptance gates

Run meaningful tests alongside each objective. Use actual WebSockets, shared native transport, Mac proxy, and gateway in the composed integration suite; mocks remain useful for fault injection but are not proof of the phone path.

Required automated coverage:

- Rust core and FFI: handshake vectors, both pin checks, enrollment/session purpose separation, tamper/replay/truncation rejection, wrong-version failure, key/counter freshness after reconnect, stream bounds, cancellation during every phase, and no deadlock through Swift callbacks.
- Relay/control plane: one-use ticket races, duplicate role generations, expiry/renewal, wrong issuer/audience, room cross-routing, replay after restart, absent peers, backpressure, global quota enforcement, denial on revoked/unenrolled identities, issuance-versus-revocation races, and durable outbox recovery. Run PostgreSQL tests against a disposable database, not just the in-memory store.
- Host authority: malicious directory cannot enroll a key; approval precedes persistence; canceled enrollment cannot authorize; raw gateway headers cannot override identity/grant; downgrade/revoke and store/audit failure stop active traffic; every new privileged action uses current permission.
- Recovery: lose acknowledgements before/after durable acceptance and external dispatch, restart each endpoint/service, exercise receipt eviction/stale epochs, and verify no duplicate session/message/approval. An ambiguous external effect must remain ambiguous.
- Terminal: physical input/output and full-frame restore; no input replay or automatic displacement of another surface; disconnect leaves the daemon/agent running.
- Privacy: forbidden metadata absent from service schemas, DB rows, request logs, traces, errors, and push payloads; endpoint keys never enter relay claims; loopback capability enforced before any remote stream opens.
- Build/distribution: schema/fixture generation checks, Rust and Swift tests, TypeScript service/client checks, rebuilt device/simulator XCFramework, actual iPhone build, and signed/notarized coordinated Mac payload. Verify installed build identities, not just successful compilation.

Physical-device matrix: same LAN, phone cellular, unrelated Wi-Fi, and a controlled network with UDP blocked but HTTPS/WSS allowed. On each perform at least 30 cold opens and 30 foreground/reconnect cycles, including session listing, creation, conversation send/approval, preview, and terminal use across the run. Separately test 20 Wi-Fi/cellular switches, 20 background/foreground cycles with long suspensions, 10 Mac sleep/wake cycles, and 10 each relay/helper/gateway restarts. Exercise at least one lease expiry, normal renewal, and control-plane outage. Record exact counts and per-attempt failure stage.

Initial release gates (engineering targets to measure, not current promises): all eligible attempts in that defined matrix eventually succeed without re-pairing or manual app restarts; p95 cold-open to usable gateway <= 5 seconds on healthy tested networks; p95 recovery <= 5 seconds from an explicit foreground/network-restored event, and <= 60 seconds for an otherwise silent broken path. Report pairing/first launch separately. Revoked/unsupported/offline attempts are expected refusals and must be counted separately rather than silently excluded as failures. Report Mac wake recovery from the point networking is usable.

Run a 24-hour real-device/host soak with repeated streams and at least one high-output workload. After streams close, active handles and task/socket counts return to their defined baseline; RSS must plateau after warmup rather than grow with reconnect count. Capture quantitative graphs/counts and terminal latency under load. No plaintext leakage, duplicate side effects, stale privileges, or lost running sessions is acceptable. A finite passing sample is release evidence, not proof of a universal availability percentage. If a gate cannot be met, document the measured cause and proposed change; do not quietly relax it to mark the objective complete.

## 10. Ordered Overlord objectives

Create one draft mission with the following three large, sequential objectives. Keep auto-advance off and do not queue or launch it now. Each objective includes its implementation, regression tests, and documentation. Do not split contracts, individual platforms, security, or ordinary test work into separate objectives when they belong to the same delivered behavior.

### Objective 1 — Replace remote transport and pairing end to end

Prerequisite: recorded successful conclusion of the current connection work; revalidate this plan against the resulting source. Deliver sections 3–6 and the relay/control-plane portions of section 8 as one integrated slice: new schemas, separate relay service, admission/leases/outbox/entitlement, explicit endpoint pairing, shared Rust Noise/Yamux core, FFI/XCFramework, Mac helper/IPC/proxy, mobile adapter/link ownership, same-protocol LAN path, live authorization, and basic connect/disconnect UI. Address/revalidate all relevant security findings. Establish basic close/cancel/reconnect primitives needed to test the new path. Remove superseded ICE/STUN/TURN, candidate signaling, old QR trust flow, duplicated Noise implementation, obsolete remote compatibility shims, and their exclusive dependencies from the replacement checkout. Update source docs alongside removal; preserve irreplaceable fixtures and historical investigations. Keep the production baseline untouched during implementation. Completion requires real loopback WSS integration through both endpoints to the gateway, malicious/unauthorized-path and PostgreSQL regressions, bounded resources/cancellation, successful native builds, and a reviewable staged deployment configuration. No separate protocol-only or service-only delivery counts as completion.

### Objective 2 — Complete resilient mobile sessions and attention notifications

Depends on Objective 1. Deliver section 7 on the new link: centralized retry/foreground/network state, capability refresh, Hub revision/snapshot recovery, durable create/message/approval reconciliation and safe retention, current-grant action admission throughout recovery, terminal resume/ownership/input rules, capability-protected adapter lifecycle, Mac sleep/wake status and opt-in keep-awake behavior, generic APNs registration/delivery/revocation, and clear offline/uncertain/delivery UI. Extend existing owners rather than duplicate them. Complete composed fault-injection tests for each interruption/crash boundary and endpoint/service privacy checks. Completion requires repeatable recovery without duplicate execution, automatic terminal takeover, or stale permissions, with notification behavior tested against APNs where credentials/device access are available; missing live evidence remains explicitly outstanding for Objective 3.

### Objective 3 — Deploy, verify on physical devices, and retire old infrastructure

Depends on Objective 2. Deliver sections 8–9 and the coordinated cutover below: provision the independently deployed relay and secrets/quotas/alerts, deploy compatible control plane, ship/rebuild/install matching native clients and signed Mac payload, re-pair the owner's devices, run the complete physical matrix and soak, fix demonstrated failures, publish measured performance/reliability/cost/privacy evidence and operations/runbooks, remove old TURN issuance/configuration/credentials/resources and stale setup instructions, and verify the final repository/deployment has one supported implementation. Completion requires actual device and deployed-service evidence, not a checklist of commands for the user to run later. Record unavoidable human steps such as iPhone interaction distinctly and keep the objective open until required evidence exists. Do not delete resources until their ownership and lack of other consumers are confirmed.

Three objectives keep the transport/authentication change coherent, then establish application recovery on a stable boundary, then validate and cut over the whole system. There is no separate planning objective: this document is that deliverable.

## 11. Coordinated cutover and removal checklist

1. Retain a known-good build/source reference and baseline field report from the current connection fix. Save an owner-only backup of remote configuration needed for deliberate rollback; do not copy session secrets into reports.
2. Finish implementation and local/staging verification before modifying the running installation. No long-lived production feature flags or dual protocol servers are required.
3. Install the independent relay with default-deny admission, provision secrets and budgets, and validate TLS/WSS connectivity. Update control-plane schema/configuration. Keep destructive legacy credential/table removal until the verification point below; migrations remain forward-only.
4. In a coordinated maintenance window, disable remote access, update all endpoints and the signed helper/CLI/daemon bundle, and enable the new service. A brief remote-access outage is acceptable. Existing latchd sessions and conversation data must remain intact.
5. Invalidate old remote pairings/credentials and show Pairing required. Enroll the owner through the new explicit approval flow. Run smoke tests, then the complete matrix/soak. Remove old remote configuration by narrowly scoped migration; never wipe `~/.latch` or session directories.
6. If the new release fails a gate, repair it or deliberately restore the archived coordinated release with its appropriate configuration. Do not opportunistically downgrade a live authenticated connection. Do not resurrect revoked credentials/grants from backup. Any necessary re-enrollment is explicit; active sessions remain protected.
7. After passing, remove old deployed TURN resources, unused secrets/variables, ICE endpoints/presence storage, helper offer files, build inputs, `webrtc-*`/TURN dependencies and transport-only patches. Verify actual consumers before deleting a crate or vendor tree. Preserve unrelated vendor fixes and terminal fixtures. Remove obsolete v1/legacy remote adapters only after updating every in-repository consumer.
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
