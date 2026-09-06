# Remote connection fixes — 6 September 2026

Base: `5dbbb6887b5cf4d4831802f7e53aa8642ede785c`.

## Evidence and scope

Read `REMOTE_CONNECTION_DEBUGGING_HANDOFF.md`, the earlier Fable field report,
and the local Mac trace. The latest inspected trace had 21,475 lines. Its final
run starts at line 14,199 and contains three completed transports and one ICE
timeout, zero CreatePermission errors, and no integrity rejection. This still
does **not** demonstrate why the phone failed to nominate that attempt.

The running helper started at 12:02:41 CEST, but its executable's modification
time was 09:56:56. The installed Desktop executable was modified at 11:53:36;
the pre-existing iOS native archive at 11:48:51. These timestamps identify a
mixed build history, not exact provenance. CoreDevice lists the paired iPhone
but its connection times out, so the installed phone build cannot be verified.

No authentication, pairing grants, terminal ownership, or control-plane
policy was weakened or changed. The TURN TCP/TLS capability gap remains.

## Source-backed fixes

- **Abandoned ICE agents:** the pinned ICE library requires explicit close;
  dropping an agent does not shut down its network tasks. Gathering errors,
  invalid candidates, ICE timeout, cancelled connects, and discarded endpoint
  handles previously dropped ownership without closing. An owner now schedules
  close on the creating Tokio runtime, including destruction from a Swift
  thread. Ownership moves into successful connections. Explicit connection
  close attempts every layer even if an earlier layer reports an error.
- **FFI lifecycle:** close now cancels a pending connect or relay gather,
  releases an unused gathered endpoint, and prevents future use. Connect,
  regather, and close cannot publish conflicting states. Regather closes the
  replaced endpoint. Only the actual ICE check timeout authorizes the existing
  connectivity retry; DTLS/SCTP errors no longer masquerade as that timeout.
- **Queued cancellation:** cancelling a serialized mobile opening reaches its
  operation task. A cancelled opening cannot post an offer after leaving the
  queue or replacement wait.
- **Discovery self-wait:** the path callback used to fetch capabilities through
  a new channel queued behind the opening channel invoking the callback.
  It now invalidates cached capabilities; the gateway's existing `require`
  path refreshes them on its next operation. Rediscovery holds the gateway
  weakly to break the route/provider/gateway ownership cycle.
- **Dual-stack candidate publication:** the bounded phone candidate list now
  reserves both IPv4 and IPv6 variants before redundant host candidates can
  consume the remaining slots.

These fixes address demonstrated source defects. They are not evidence that
one of those defects caused the recorded nomination timeout.

## Phone and helper diagnostics

Settings → Connection diagnostics provides an opt-in recording switch and a
share action. Recording resumes on launch only if explicitly enabled. Disable
recording after the reproduction before sharing the file.

The file is `Documents/ice-debug.log` in the phone app container. The helper
continues to use `~/.latch/remote-access/ice-debug.log` as its opt-in marker.
The shared writer caps either log at 8 MiB (the next record after the cap
starts a fresh segment), accepts only ICE/TURN and transport diagnostic
categories, and suppresses the pinned dependency's password-bearing startup
record. It never logs terminal traffic, DTLS records, or Noise records.
Deleting the marker now stops output from an already running helper too.
Historical logs written before this fix may contain ICE passwords; keep them
local and do not share them unredacted.

Both sides log an `attempt` fingerprint of the phone's random ICE username
fragment. The helper associates it with the rendezvous request ID. The
transport logs role, credential fingerprints, nomination, completion, failure
stage, and cancellation; underlying ICE logs show checks and pair states.
Fingerprints never use passwords or pairing keys. The helper intentionally
still reuses credentials across idle agents, so its credential fingerprint is
**not** an agent-generation identifier. Compare the candidate socket addresses
in the local traces to test stale presence/agent mismatch; signaling still
returns a presence snapshot rather than reserving an offer-specific agent.

## Remaining physical verification

After installing the rebuilt phone app, enable diagnostics and record Wi-Fi
or cellular explicitly. Exercise gateway discovery, session list, terminal
attach/output/input, and repeated fresh requests. Include cancellation and
background/foreground transitions. Do not automatically replay terminal input
or take over an existing attachment for testing.

For a failed attempt, match `attempt` on both sides and determine whether the
phone received successful checks, selected a nominatable pair, sent
USE-CANDIDATE, and received its response. Keep overlapping attempts separate.
This is the missing evidence required before calling the cellular issue fixed.

## Validation and installation

Completed in this work session:

- Rust: 19 transport tests, 4 FFI tests, and 2 helper integration tests passed.
  The transport tests include real ICE/DTLS/SCTP over simulated NAT and TURN,
  cancelled checking agents, a full 15-second timeout, invalid candidates,
  and destruction from a non-Tokio thread.
- Swift: 239 MobileKit tests, 5 emulator tests, and 1 generated native-boundary
  test passed (245 total). The native test starts the actual Rust ICE connect
  through UniFFI, closes it, checks prompt cancellation, and checks that its
  deliberately distinctive password never reaches the trace.
- Rebuilt all five Rust targets and the generated XCFramework/Swift API.
- The actual iPhone Xcode scheme built successfully for `generic/platform=iOS`,
  with signing disabled for this compile check. Product:
  `/tmp/latch-connection-ios-build/Build/Products/Debug-iphoneos/Latch.app`.
  The app is **not installed on the phone**. Normal Xcode signing/install is
  still needed. An existing `IceConfiguration.swift` Sendable warning and
  Xcode's no-AppIntents metadata warning remain unrelated to these changes.
- `git diff --check` passed.

Installed the rebuilt Mac helper at `~/.local/bin/latch-remote` and restarted
it under the user's explicit permission. The first Cargo-signed development
binary waited on Keychain access. Re-signed it using the previous helper's
Developer ID and `latch-remote` identifier; verified that its designated
requirement exactly matches the original. The signed helper successfully
started its private gateway, listener, and ICE gathering. No Keychain access
rules were changed. Desktop itself was not modified or replaced.

Installed helper SHA-256:
`9e13c136b0661b59a78f1ce6dff1695d36f47f2c9f737a9d1af55efc330b4c64`

Built iOS native archive SHA-256:
`a58e7d416a7be00d1afd225d60ea3f174e3898b5a5c397d9966f37c9f386e5bb`

Built universal macOS native archive SHA-256:
`55a30a35ab5d958199dffaa1ecbc96507e7d7654c840b3ce4d5c6a1fcea6b3f6`

Rollback helper: `/private/tmp/latch-remote-before-connection-fix-20260906`.
Test/build logs for this session are `/tmp/latch-rust-tests-final.log`,
`/tmp/latch-swift-tests-final.log`, `/tmp/latch-xcframework-build.log`, and
`/tmp/latch-ios-build.log`. The runtime trace remains local. Note that the
workspace sandbox denies the process-liveness probe used by `latch
remote-access status`; run that read with normal process permissions before
interpreting a missing listener as an actual outage.
