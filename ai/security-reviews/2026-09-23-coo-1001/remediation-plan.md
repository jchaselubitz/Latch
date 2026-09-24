# Latch Security Review — Remediation Plan

Mission `coo:1001`, objective `coo:1001.gvrk`. Written 2026-09-24 against revision `4b7e463`.

## Inputs

This plan consolidates the results of the two review objectives on this ticket:

- `report.md` / `findings.json` (objective `coo:1001.aep3`): four source-validated findings, F1–F4.
- `transport-native-follow-up.md` (objective `coo:1001.z2qn`): five source-backed findings, T1–T5, plus a "checked and sound" list.
- `coverage.json` / `scan-manifest.json`: 75 fully reviewed files, coverage marked partial.
- The report's **Open Questions And Follow Up** section.

Every cited file and line range was re-checked against HEAD `4b7e463` on 2026-09-24. All still exist and match the descriptions in the reports. No application code was changed while writing this plan.

## Consolidated findings register

| ID | Finding | Severity | Component | Fix effort | Phase |
| --- | --- | --- | --- | --- | --- |
| F1 | Observe paired device escalates to Interact via bare-LF header smuggling through the Remote Link proxy | **High** | `latch` helper proxy + gateway | M (1–2 days) | 0 |
| F2 | One admitted client crashes the shared relay via an unhandled `ws` error event | **High** | `services/relay` | S (hours) | 0 |
| T1 | Pairing approval sentence interpolates the peer-chosen device name; grant can be misread | **Medium** | `latch-remote` host + Desktop | S–M (1 day) | 1 |
| F4 | Concurrent `/v1/connect` upgrades bypass relay connection budgets | Low | `services/relay` | S (hours) | 1 |
| T5a | Phone connector opens a cleartext `ws://` relay URL if the control plane returns one | Low | `latch-transport` (+FFI) | S (hours) | 1 |
| T2 | Pre-handshake relay records buffered with no cap in `wait_for_peer` | Low | `latch-transport` | S (hours) | 1 |
| F3 | `install-cli.sh` executes downloaded binaries without pinning the publisher Team ID | Low | `scripts/install-cli.sh` | S (hours) | 2 |
| T5b | Desktop updater trusts the GitHub tag as the version; allows signed-downgrade | Low | Desktop `Updater.swift` | S (hours) | 2 |
| T3 | One unauthenticated LAN connection stalls the only LAN acceptor for 10 s | Low | `latch-remote` LAN listener | M (1 day) | 2 |
| T4 | Bonjour LAN targets not restricted to private IPs; one auth failure abandons the LAN list | Low | Mobile `GatewayTransport` / `NativeRemoteTransport` | S (hours) | 2 |

Phase 3 covers the open questions and coverage gaps that are not yet findings.

## Sequencing and rationale

**Phase 0 — ship this week.** F1 and F2 are both High, both reachable by an authenticated but low-trust party, and both have small, self-contained fixes. F2 is a relay-only deploy with no client compatibility impact and should go first. F1 touches the helper binary that runs on every host, so it ships in the next CLI release, with the gateway-side defense in depth landing in the same change.

**Phase 1 — next release train.** T1 is the only Medium and is a human-facing trust boundary (the owner's approval prompt). F4, T5a and T2 are all small hardening changes in code that Phase 0 already touches (relay upgrade path, transport link), so they ride with it.

**Phase 2 — supply chain and LAN robustness.** F3 and T5b are two halves of the same story (publisher pinning on first install, version pinning on update). T3 and T4 are availability issues on the LAN path only; the relay path is unaffected.

**Phase 3 — close the review's own gaps.** These are investigations, not fixes: deployed DB TLS, revocation/redemption race, archive extraction, `--allow-remote` intent, conversation rendering audit, dependency audit.

## Phase 0: High-severity fixes

### F1 — Header smuggling through the Remote Link authority proxy

**Where.** `crates/latch/src/cli/remote_access.rs:760-831` (`authorize_and_inject`), `crates/latch/src/cli/serve/http.rs:278-320` (grant and device-id extraction).

**Root cause.** The proxy validates headers by splitting on CRLF and then copies the *original request bytes* ahead of its injected `Authorization`, device-grant and device-id headers. hyper/httparse on the gateway side also accept bare LF as a line terminator, so a value containing `\n<grant header>: control` becomes a second header that precedes the proxy's, and `HeaderMap::get` returns the first one. The Hub then stores that grant on the subscriber and authorizes Interact actions against it.

**Fix design.** Two independent layers so that either one alone closes the hole.

1. **Proxy: never forward caller bytes.** Parse the incoming request with `httparse` (already in the dependency tree through hyper) into method, target, version and a list of `(name, value)` pairs. Reject the request if any of the following hold:
   - any header name or value contains a byte outside visible ASCII plus SP/HTAB, in particular `\r`, `\n`, or NUL;
   - any header name is one of the forbidden set (`authorization`, `proxy-authorization`, `transfer-encoding`, `DEVICE_GRANT_HEADER`, `DEVICE_ID_HEADER`), compared case-insensitively against the *parsed* name;
   - the header block ends without exactly `\r\n\r\n`, or `httparse` reports partial or error.
   Then **serialize a fresh request** from the parsed fields (`{method} {target} HTTP/1.1\r\n` + each validated `name: value\r\n` + injected trusted headers + `\r\n` + body). The original bytes are never copied. Keep the existing target checks (`/v2/` prefix, no `..`, no `%2e`) and the route grant check unchanged.
2. **Gateway: refuse ambiguity.** In `serve/http.rs`, replace `headers().get(DEVICE_GRANT_HEADER)` and `headers().get(DEVICE_ID_HEADER)` with `get_all(...)`; if the iterator yields more than one value, return 400 and log it. Additionally reject any value whose `to_str()` fails or contains `\r`/`\n`. Do this *before* the loopback trust decision so a smuggled duplicate is rejected even from loopback.
3. **Hub (defense in depth, optional but recommended).** `conversation/hub.rs:739-769` authorizes actions against the grant captured at subscribe time. Add an audit log line when a subscriber's grant is stronger than the route's required grant for its transport (Remote Link subscribers with `Interact` or `Control`), so a future regression is visible in `audit.jsonl` even if the parsing layers fail.

**Tests.**
- Unit tests in `remote_access.rs` `#[cfg(test)]` (tests already exist at lines 1157+):
  - a request with `X-Foo: a\n<DEVICE_GRANT_HEADER>: control` is rejected;
  - a request with bare `\r` inside a value is rejected;
  - a request with the grant header spelled in mixed case, or preceded by whitespace, is rejected;
  - a legitimate WebSocket upgrade request (with `Connection: Upgrade`, `Sec-WebSocket-Key`, etc.) is accepted and the serialized output contains exactly one grant header and one device-id header, and they appear after all caller headers;
  - a serialized request round-trips through `httparse` with the same header set the proxy validated.
- Integration test at the gateway (`crates/latch/tests/`): send a request directly to the loopback gateway containing two grant headers and assert 400; send one with a single grant and assert the existing behaviour.
- End-to-end regression in `crates/latch-remote/tests/remote_link_composed.rs`: an Observe-paired controller that sends a smuggled grant on the conversation upgrade gets the socket refused, and an Observe subscriber that reaches the Hub cannot execute `send_message`.

**Acceptance.** Observe device cannot perform any Interact action through Remote Link by any header construction. Legitimate Interact/Control flows and WebSocket upgrades still pass the existing e2e suites.

**Compatibility.** No wire change. The proxy is stricter, so any client sending non-ASCII or folded headers will now be refused; the Latch phone app does not do this (verified in the follow-up review's loopback section).

### F2 — Unhandled WebSocket error terminates the relay

**Where.** `services/relay/src/server.ts:161-203` (`handleUpgrade` callback and `admit`).

**Root cause.** `admit` attaches `pong`, `message` and `close` listeners but no `error` listener. With `maxPayload: MAX_RECORD_BYTES` set, an oversized frame makes `ws` emit `error` on the socket; with no listener, Node throws from the emitter and the process exits, dropping every room. The stale-generation branch closes the socket without any listeners at all.

**Fix design.**
1. In the `handleUpgrade` callback, attach `ws.on('error', ...)` **as the first statement**, before calling `admit`. The handler logs (room id, role, attempt id, error code) and calls `ws.terminate()`. Because `close` follows `error`, the existing `close` handler performs room cleanup.
2. Also attach `socket.on('error', () => socket.destroy())` on the raw `net.Socket` at the top of the `server.on('upgrade')` handler. A reset during the `await options.redeem(...)` window otherwise has no listener either.
3. Keep `maxPayload` as is. Do not add a process-level `uncaughtException` swallow as the fix; if one is added for logging, it must still exit so the supervisor restarts a known-good process.

**Tests** (add to `services/relay/src/server.test.ts`, which already has a harness with `redeem` stubs):
- Admit two rooms. From one peer, send a binary frame of `MAX_RECORD_BYTES + 1` bytes. Assert that peer's socket closes with code 1009, the other room still forwards records, and the process is alive.
- Send a frame with an invalid RSV bit or a fragmented control frame from an admitted peer and assert the same containment.
- Simulate `redeem` rejecting after the raw socket has been destroyed by the client; assert no unhandled error.

**Acceptance.** No single admitted or half-admitted client can take down another room. The test suite exercises the parser error path, not just the application `forward()` size check.

**Deploy.** Relay-only. Deploy to the Railway relay service immediately after merge; no client coordination required.

## Phase 1: Medium and adjacent hardening

### T1 — Pairing approval prompt can be led by the device name

**Where.** `crates/latch-remote/src/link.rs:738-754` (`validate_proposal`), `link.rs:668-675` (`authorize_enrollment`), `crates/latch/src/cli/remote_access.rs:204-207`, `apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessView.swift:406-416`.

**Fix design.**
1. **Host name policy.** In `validate_proposal` and again in `authorize_enrollment`, apply the same allowlist the phone already enforces in `PairingModel.enrollableName` (`PairingModel.swift:322-323`, `372-386`): letters, digits, space, and `. ' _ ( ) -`; 1–80 bytes; no leading/trailing whitespace; reject anything else including `\n`, `\r`, tabs, bidi controls and other non-printing code points. Reject, do not sanitize, so the approval UI never sees a modified name.
2. **Desktop prompt.** Replace the single interpolated `Text` with separate labeled rows: `Device` → name (rendered in a fixed-width or bordered field, `lineLimit(1)`), `Access requested` → `permission.label` with a visual weight that cannot be confused with body text, then the comparison code. The permission label must never be on the same line as user-supplied text.
3. **Comparison code (recommended, larger).** Show the requested permission alongside the code on both devices, so the owner has a second channel that binds the grant. This is a phone and Desktop UI change and can trail the first two items.

**Tests.**
- Rust: `validate_proposal` rejects a name with `\n`, with a second `requests` phrase containing control characters, with a zero-width joiner, and with 81 bytes; accepts `Jake's iPhone (work)`.
- Desktop `RemoteAccessTests.swift`: the approval view model exposes name and permission as separate fields and does not concatenate them.
- Contract check (`scripts/check-remote-link-contract.sh`): the host name policy and the phone name policy are the same regex, or a shared fixture list is asserted on both sides.

**Acceptance.** No proposal whose name violates the allowlist reaches the Desktop approval prompt, and the prompt cannot present the grant on a line the peer controls.

### F4 — Relay capacity is checked before redemption, not reserved

**Where.** `services/relay/src/server.ts:147-165`, `172-203`.

**Fix design.** Introduce `pendingByIp` and `pendingTotal` counters. In the upgrade handler: check `active + pending` against both limits, **increment pending before** `await options.redeem(...)`, and decrement in every exit path (verify failure, redeem failure, `handleUpgrade` callback, stale-generation close). In `admit`, convert pending to active atomically. Wrap `redeem` in `AbortSignal.timeout(10_000)` (or a configured `redemptionTimeoutMs`) so a slow control plane cannot pin reservations indefinitely. Consider a small per-IP pending cap (e.g. 4) separate from the active cap.

**Tests.** Hold N redemption promises pending; assert request N+1 gets 429 before `redeem` is called. Reject, time out, and destroy sockets mid-redemption; assert counters return to zero. Existing budget tests must still pass.

### T5a — Phone will open a cleartext relay URL

**Where.** `crates/latch-transport/src/link.rs:327-359` (`WssRecordIo::connect*`), `crates/latch-transport-ffi/src/lib.rs`.

**Fix design.** In the shared `connect_with` path, parse the URL and return `LinkError::Config("relay url must use wss://")` unless the scheme is `wss` **before** the admission bearer is attached. Allow `ws://` only behind the existing `connect_with_test_ca` test-only entry point, gated on loopback host. Because this lives in `latch-transport`, the fix covers the phone (through FFI) and any future Rust caller in one place; Desktop and `latch-remote` already refuse non-`wss` URLs.

**Tests.** Unit test that `connect("ws://relay.example/…", "token")` fails without opening a socket (use an unresolvable host and assert the error is `Config`, not a DNS error). FFI lifecycle test (`NativeLifecycleTests.swift`) asserting the typed error surfaces to Swift.

### T2 — Unbounded pre-handshake buffering in `wait_for_peer`

**Where.** `crates/latch-transport/src/link.rs:423-434`, `575`.

**Fix design.** Track `pending_bytes` alongside the `VecDeque`. Cap at `MAX_RECEIVE_WINDOW_BYTES` (8 MiB, already defined at line 35) **or** 128 records, whichever is hit first; on overflow return `LinkError::Limit("pre-handshake buffer exceeded")` and close the socket. Do not add a time bound to the host's `wait_for_peer(None)`, which is intentional.

**Tests.** Feed a fake `RecordIo` that emits binary frames without `peer_ready`; assert the error fires at the cap and memory does not grow past it.

## Phase 2: Supply chain and LAN path

### F3 — Initial installer does not pin the publisher

**Where.** `scripts/install-cli.sh:25-41`.

**Fix design.** Embed the Latch Apple Team ID as a constant in the script (a Team ID is public and appears in `codesign -dvv` output of every shipped binary; the secret used by CI in `release-cli.yml:45` is only the source of truth for that value). Replace the bare `codesign --verify --strict` with:

```sh
codesign --verify --strict --test-requirement="=anchor apple generic and certificate leaf[subject.OU] = \"${LATCH_TEAM_ID}\"" "$binary"
spctl --assess --type open --context context:primary-signature "$binary"
```

*Implementation correction (coo:1001.twwt).* The originally planned `--requirement=` is the signing-time `--requirements` option and is silently ignored by `--verify`, so a wrong Team ID still passed; `--test-requirement` is the verify-time flag. `spctl --assess --type execute` rejects every bare CLI binary ("does not seem to be an app") even for a notarized release, so the Gatekeeper check uses the `open` type with the primary-signature context, which reports `source=Notarized Developer ID`.

Run this for `latch`, `latch-remote` and `latchd` **before** the `--version` invocations. Keep the checksum and manifest checks. On non-macOS paths, document that signature pinning is not available and consider publishing a minisign/sigstore signature for those archives as a follow-up objective.

**Tests.** A shell test (or CI job) that runs the verification function against an ad-hoc-signed binary and asserts failure, and against a release binary and asserts success. Add a CI step to `release-cli.yml` that runs the installer against the freshly built release assets before publishing.

### T5b — Desktop updater trusts the tag and installs the first `.app`

**Where.** `apps/LatchDesktop/Sources/LatchDesktop/Updater.swift:102-119`, `240`, `310-316`, `274-286`.

**Fix design.** In `applicationBundle(in:)`, require the bundle be named `Latch.app` and have the expected `CFBundleIdentifier`. After `verify(_:matching:)`, read the replacement's `CFBundleShortVersionString` and require it to equal the release tag (after stripping the `v` prefix); refuse if lower than or equal to the installed version. Keep the existing team comparison and Gatekeeper assessment.

**Tests** (`UpdaterTests.swift`): archive containing an older signed bundle under a newer tag is refused; archive with a bundle named differently is refused; matching bundle passes.

### T3 — Serial LAN acceptor is stallable

**Where.** `crates/latch-remote/src/link.rs:48-49`, `308-344`.

**Fix design.** Accept in a loop that spawns each handshake as a task, bounded by a `tokio::sync::Semaphore` of, say, 4 permits; wrap `SecureLink::establish` in a 3 s timeout for the Noise phase (the current 10 s covers the whole establish). A completed authenticated link replaces the current one exactly as today; failures are logged at debug and drop the permit. Optionally bind the listener to the interfaces Bonjour advertises rather than `0.0.0.0`.

**Tests.** Integration test in `crates/latch-remote/tests/`: open a TCP connection that never sends; within 3 s a second, legitimate handshake completes.

### T4 — Bonjour targets not filtered; one auth failure aborts the LAN list

**Where.** `apps/LatchMobile/Sources/LatchMobileKit/GatewayTransport.swift:436-450`, `apps/LatchMobile/Sources/LatchTransportNative/NativeRemoteTransport.swift:360-379`, `crates/latch-transport-ffi/src/lib.rs:191-195`.

**Fix design.** When parsing `lanAddrs`/`lanHost`, accept only IPv4/IPv6 literals in RFC 1918, link-local (`169.254/16`, `fe80::/10`) or unique-local (`fc00::/7`) ranges; drop anything else silently. In `connectFirstLanTarget`, on `authentication` errors continue to the next candidate; only return early on non-auth errors once the overall 1.5 s budget is spent. Mirror the IP-literal check in the FFI `connect_lan` so a non-Swift caller gets the same guard.

**Tests.** `GatewayTransportTests.swift`: TXT record with a public IP or hostname yields no LAN targets. `NativeLifecycleTests.swift` or a unit test on the candidate loop: a first candidate that fails Noise pinning does not prevent the second from connecting.

## Phase 3: Close the review's open questions

These become investigation objectives; each ends with either a finding or a written "no issue" note appended to `report.md`.

| # | Question (from `report.md` Open Questions) | Suggested action | Effort |
| --- | --- | --- | --- |
| Q1 | Do deployed control-plane DB connections use verified TLS? `DATABASE_SSL_REJECT_UNAUTHORIZED` defaults false and TLS is disabled by substring match on `DATABASE_URL` (`config.ts:144-146`, `store/postgres.ts:140-147`). | Check the Railway service variables; flip the default to `true`, replace the substring match with URL host parsing, and add a startup warning when TLS is off outside loopback. | S |
| Q2 | Can a redemption in flight during revocation be admitted after invalidation? | Add an integration test in `server.test.ts` that invalidates between `redeem` resolving and `admit`; if it admits, have `admit` re-check a `revokedRooms` set populated by invalidation. | S |
| Q3 | Concurrent generation updates or device-count races across accounts. | Focused Postgres concurrency tests (gated on `TEST_DATABASE_URL`) around pairing completion and generation bump. | M |
| Q4 | Does the native archive extractor in the CLI updater prevent traversal and symlink escape before publisher verification? | Read `update/mod.rs` extraction path fully; add tests with `../` entries and symlinks. | S |
| Q5 | Is the documented `--allow-remote` bearer path intended to work? Middleware currently denies non-loopback requests. | **Done 2026-09-24 (decision: remove).** The flag, the opt-in branch in `refuse_non_loopback`, the non-loopback Origin relaxation, and the `bind_is_loopback` state were removed; `latch serve` now refuses any non-loopback bind. Docs updated. | — |
| Q6 | Conversation rendering, generated UniFFI bindings, and remaining Desktop/Mobile UI were not fully audited. | Schedule a third review objective scoped to `ConversationMarkdown*.swift`, `ConversationMessageRow.swift`, `LatchClient.swift`, and the generated bindings. | M |
| Q7 | No dependency vulnerability audit was run. | Add `cargo audit` and `npm audit --omit=dev` (or `osv-scanner`) to CI for `crates/`, `services/`, and `packages/`; triage the first run. | S |
| Q8 | Desktop `ControlPlaneHost.normalize` accepts non-loopback `http` and relies on ATS to block it. | Reject non-loopback `http` in the validator so the check does not depend on ATS (noted as "checked and sound" only because of ATS). | XS |

## Cross-cutting work

**Regression suite.** Every fix above lists tests. Group the new tests under a recognizable name so they can be run as a gate: a `security` test module in `crates/latch` (mirroring `crates/latchd/tests/security.rs`), the relay `server.test.ts` cases tagged `containment`, and the Swift tests under the existing `RemoteAccessTests`/`UpdaterTests`/`GatewayTransportTests` targets. Extend `docs/LATCHD_SECURITY.md` or add `docs/REMOTE_LINK_SECURITY.md` to record the invariants these tests protect: one grant header per request, no caller bytes forwarded, every WebSocket has an error handler, capacity reserved before async work.

**CI gates.** Add to the PR workflow: `cargo audit`, `npm audit`, the new security test modules, and the installer self-check against ad-hoc-signed binaries. `scripts/check-boundaries.sh` can assert that `authorize_and_inject` no longer references the raw request slice.

**Release sequencing.**
1. Relay: F2, then F4 and Q2, deployed independently.
2. CLI/helper release: F1, T1 host side, T2, T5a. These ship in one `latch` version bump.
3. Desktop release: T1 UI, T5b, Q8. Desktop can ship before or after the CLI; T1 host and UI changes are independently safe.
4. Mobile release: T4, and the T1 comparison-code enhancement if pursued.
5. Installer script: F3 can merge any time; it takes effect on the next `curl | sh`.

**Verification after rollout.** Re-run the Remote Link e2e suites (`remote_link_composed.rs`, `remote_link_recovery.rs`, `latchd_kernel_e2e.rs`) and the phone/Desktop pairing flow on real devices, since the proxy now rejects a wider set of requests. Confirm the relay's memory and connection counters in Railway metrics after the F4 change.

## Proposed Overlord objectives

Suggested split so each can be assigned and delivered independently:

1. `relay: contain WebSocket errors and reserve capacity before redemption` (F2, F4, Q2).
2. `remote link proxy: rebuild requests from parsed headers; gateway rejects duplicate grant headers` (F1).
3. `pairing: enforce host name allowlist and separate name from grant in approval UI` (T1).
4. `transport: require wss and cap pre-handshake buffer` (T5a, T2).
5. `installer and updater: pin publisher Team ID; pin bundle name and version` (F3, T5b).
6. `LAN path: concurrent bounded acceptor; private-address filter and continue-on-auth-failure` (T3, T4).
7. `control plane: verified DB TLS default and URL-based TLS decision` (Q1).
8. `security CI: cargo audit, npm audit, security test modules` (Q7, cross-cutting).
9. `review follow-up: conversation rendering, generated bindings, archive extraction` (Q4, Q6, Q8). Q5 is already resolved.
10. `verification: pair a phone with a Mac and exercise Remote Link end to end after all fixes land` (closing gate).

## What this plan does not do

- It does not change any application code. All fixes above are proposals with file and line anchors, not diffs.
- It does not re-validate the findings at runtime. Severity ratings are carried from the source reports, which were static-analysis-only.
- It does not cover files outside the 75 fully reviewed in `coverage.json`; Q6 exists for that reason.
