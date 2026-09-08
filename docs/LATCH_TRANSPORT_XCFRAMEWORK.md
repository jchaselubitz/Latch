# Latch transport XCFramework

`latch-transport` is the single production implementation of Remote Link on
the Mac and iPhone. It owns platform-validated TLS/WSS, the authenticated LAN
entry point, Noise XX identity authentication, bounded Noise records, Yamux
multiplexing, cancellation, and lease-extension control messages. The iOS app
consumes it through the `latch-transport-ffi` UniFFI surface.

The ordinary `latch` crate does not depend on `latch-transport`. It remains the
local authority and accepts only already-authenticated logical streams from
`latch-remote` through a fixed, capability-protected loopback gateway.

## Pinned inputs

- `Cargo.lock` pins `snow`, `yamux`, `tokio-tungstenite`, `native-tls`, and the
  complete Rust dependency graph.
- TLS verification uses the operating system trust store. Do not replace it
  with a bundled root set.
- `apps/LatchMobile/Contract/schemas/remote-link.schema.json` and
  `schemas/remote-link/` are the cross-language wire contract.
- `fixtures/remote-link/` contains accepted and rejected examples.

## Local generation

Install the pinned targets once:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios \
  aarch64-apple-darwin x86_64-apple-darwin
```

Then run:

```sh
./scripts/build-latch-transport-xcframework.sh
```

The script builds device, universal-simulator, and universal-macOS static
libraries, generates the Swift surface from the compiled Rust metadata, and creates
`apps/LatchMobile/Native/LatchTransportFFI.xcframework`. It never downloads or
links a third-party prebuilt framework.

`apps/LatchMobile/Package.swift` declares the binary target unconditionally,
so the framework is a hard build dependency rather than an optional extra:
without it SwiftPM fails at resolution instead of quietly producing an app
whose remote access works on the local network and nowhere else. Run the
script before `swift build`, `swift test`, or an Xcode build in a fresh
checkout — the generated framework and Swift surface are both gitignored.

The macOS slice exists so SwiftPM can validate the binary target and run the
simulator-free `LatchMobileKit` tests after the framework is generated. The
shipping app still consumes only the iOS device/simulator slices.

CI runs the same command before the iOS build. A release must be produced from
a clean checkout with `cargo build --locked`; changes to generated Swift or the
XCFramework without the corresponding `Cargo.lock`/Rust source change are
review failures.

## Security invariants

- A relay admission claim is bearer routing authority, not endpoint
  authentication.
- Enrollment also requires the independent 256-bit QR-only secret in the
  Noise prologue; possession of the admission claim alone fails.
- Both endpoints pin the peer static key before application streams open.
- Only bounded binary records cross the relay; service logs never receive
  application plaintext or gateway credentials.
- LAN and WSS use the same authenticated link and stream implementation.
