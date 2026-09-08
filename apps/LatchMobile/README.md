# Latch Mobile

Latch Mobile is the native iOS client for Latch's schema-first `/v2` gateway.
It discovers, creates, observes, and controls sessions on one explicitly paired
Mac without storing session state in the cloud.

## Architecture

`LatchMobileKit` owns the generated gateway models, pairing state, current
grant, session/conversation clients, terminal lifecycle, and a transport-neutral
`GatewayTransport` boundary. `LatchTransportNative` is the required adapter
to the shared Rust Remote Link core. The Xcode app target owns SwiftUI,
SwiftTerm, camera scanning, and platform lifecycle.

The production path is:

```text
Swift gateway client
  -> random capability on a loopback-only phone adapter
  -> Rust authenticated logical stream
  -> Noise XX + Yamux
  -> authenticated LAN or outbound WSS carrier
  -> opaque relay room when off-LAN
  -> Mac fixed loopback authority
  -> local /v2 gateway
```

There is no manual gateway address/token mode and no compatibility fallback to
an older pairing or transport. If the saved record lacks current Remote Link
grant metadata, the app fails closed and asks the owner to pair again.

## Pairing

In Latch Desktop, configure the control plane, turn Remote Access on, and
select **Pair a Device**. Scan or paste the five-minute Remote Link JSON code.
It contains:

- the HTTPS control-plane address and opaque enrollment ID;
- the Mac's pinned static public key;
- a short-lived service-visible admission code;
- an independent 256-bit QR-only enrollment secret;
- an expiry and bounded Mac display name.

The phone first validates all fields and lifetime. The native transport claims
the provisional enrollment, opens WSS, and performs Noise XX with the QR-only
secret mixed into the prologue. Both endpoints then show words derived from
the authenticated transcript. Verify they match and approve the named phone on
the Mac. The phone persists a record only after it receives the encrypted
receipt containing the exact enrollment, both endpoint keys, permission, and
grant revision.

Camera permission is optional because the same new Remote Link code can be
pasted. The app never accepts an old phrase, gateway bearer, or caller-supplied
network destination.

## Connectivity and authorization

The native provider races authenticated LAN with WSS and keeps the first
authenticated link. Both carriers use the same Rust Noise/Yamux implementation
and pinned keys. WSS uses platform certificate validation; the relay sees only
bounded ciphertext and an identity-free room claim.

Each logical stream is authorized by the Mac's current `observe`,
`interact`, or `control` grant. The app also checks the latest grant before
showing actions. A missing or stale grant fails closed. Terminal access
requires `control` plus iOS device-owner authentication and takes the
session's exclusive terminal surface.

The phone exposes its loopback adapter only on `127.0.0.1`, protects it with
a fresh random 256-bit capability, validates the first HTTP request within a
bounded header budget, and lets the Rust provider choose the remote stream.

## Recovery and lifecycle

`RemoteLinkCoordinator` is the one owner of the paired link. Screens ask it
for gateway channels and read its snapshot; they never open connections. It
retries with full-jitter backoff (250 ms to 15 s, reset after 30 s healthy),
treats a foreground or network-path change as one immediate attempt (a link
that looks alive is probed with a bounded discovery and replaced if the probe
fails), and stops for anything a retry cannot change: authentication failure
(`pairingRequired`) or a control plane that no longer knows the pairing
(`revoked`). A relay that admits the phone without the Mac present is shown as
`macOffline` and retried. Discovery runs once per authenticated link; cached
session rows stay on screen marked stale while the link is down.

Backgrounding releases everything: the loopback adapter and its random
capability, the native link, conversation sockets, and any held terminal
surface. Foreground starts a new adapter with a new capability before the same
owner resumes. Push is not used to keep anything alive.

Terminals never replay input. If the transport drops under a held surface the
screen becomes *interrupted*; the gateway's bounded resume capability (from
the `attached` frame) lets the same device take the surface back for 60 s,
and only if nothing else has attached since, otherwise the person must Take
Control deliberately. Keystrokes typed just before the drop are reported as
possibly undelivered. Conversation sends reuse their durable operation ids,
ambiguous outcomes are reconciled through `operation_status`, and a receipt
the gateway no longer holds is reviewed rather than resent. Session creation
retries reuse the same request id, which the Mac scopes to this device.

Attention notifications are generic: the control plane pushes a fixed
sentence when a watched session finishes or needs input; the app fetches real
state over the authenticated link when it opens. The APNs token is the only
thing registered, and it is removed on revoke or unpair.

Settings → Diagnostics runs real suspend/resume cycles through the owner and
writes content-free per-attempt stage timings to Files › Latch ›
latch-diagnostics. Its skip-LAN switch measures the relay path from a network
where the Mac is also nearby; it is a diagnostics setting, not a transport
mode. The same runner starts without a tap when the app is launched with
`-latchDiagnosticsCycles N` (plus optional `-latchDiagnosticsSkipLAN 1`,
`-latchDiagnosticsTerminal 1`, `-latchDiagnosticsPause S`), and
`-latchDiagnosticsColdOpen 1` arms one `cold_open` record for the process,
measured from the kernel's process start to a usable gateway;
`scripts/phone-diagnostics.sh` drives both over USB for the physical matrix.

## Contracts

- `apps/LatchMobile/Contract/schemas/remote-link.schema.json` is the Swift
  mirror checked into the mobile package.
- `schemas/remote-link/` is the repository source contract.
- `fixtures/remote-link/` holds positive and negative wire examples.
- `scripts/check-remote-link-contract.sh` verifies the copies and fixtures.

## Build and test

Generate the native framework after Rust transport changes:

```sh
scripts/build-latch-transport-xcframework.sh
```

Then run:

```sh
swift test --package-path apps/LatchMobile
xcodebuild -project apps/LatchMobile/App/LatchMobile.xcodeproj \
  -scheme LatchMobile -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build
```

The Swift suites cover pairing validation, exact encrypted receipts, current
grant behavior, capability enforcement, request bounds, cancellation, gateway
operations, terminal lifecycle, and native provider behavior. A signed
physical-device build and deployed relay verification remain later cutover
gates.
