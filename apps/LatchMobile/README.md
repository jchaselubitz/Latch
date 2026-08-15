# Latch Mobile

A SwiftUI iPhone app for watching and replying to Latch sessions from away from
the desk. It is the first experiment in remote Latch access, deliberately small:
two tabs, and a chat screen per session.

Everything the app needs lives in this folder, so it can be moved to its own
repository without leaving a dependency on the Latch checkout behind.

## What it does

- **Sessions** lists the sessions on the linked computer, with state, working
  directory, and idle time.
- **Settings** links the phone to one `latch serve` gateway and shows what that
  gateway reports it can do.
- Tapping a session opens a **chat** view: the harness transcript as it streams
  in, a composer for sending a message, and buttons for answering a permission
  prompt or question when the session is blocked on one.

It does not implement the terminal. The transcript is the v1 history view;
terminal access stays on the desktop for now.

## Layout

```text
Package.swift                     LatchMobileKit: everything that is not a view
Sources/LatchMobileKit/
  Generated/LatchContract.swift   generated from the schemas; never hand-edited
  LatchGateway.swift              HTTP client, discovery, and sending
  EventStream.swift               events WebSocket, cursor, resync, reconnect
  Transcript.swift                harness events folded into chat rows
  GatewayCompatibility.swift      the /v1 compatibility rules
  LinkStore.swift                 keychain storage for the address and token
  AppModel.swift, ChatModel.swift the two observable models the views bind to
App/LatchMobile.xcodeproj         the iOS app target
App/LatchMobile/*.swift           the SwiftUI screens
Contract/                         vendored schemas and their digests
Tools/generate-contract.py        the contract generator and drift gate
```

The kit is a plain library so `swift test` exercises the client on a Mac with no
simulator involved. The app target consumes it as a local package rather than
compiling a second copy of the sources.

## Building and running

```bash
swift test                                     # the client: 49 tests, 6 more live
open App/LatchMobile.xcodeproj                 # then run on a simulator or device
```

From the command line:

```bash
xcodebuild build -project App/LatchMobile.xcodeproj -scheme LatchMobile \
  -sdk iphonesimulator -destination 'platform=iOS Simulator,name=iPhone 17 Pro'
```

Signing is left unset, so a device build needs a development team selected in
Xcode. The simulator needs none.

Six of the tests check the client against a real gateway instead of a stub, and
skip when there is not one to talk to:

```bash
latch serve &
LATCH_GATEWAY_URL=http://127.0.0.1:4610 \
  LATCH_GATEWAY_TOKEN="$(cat ~/.latch/serve.token)" swift test
```

## Connecting it to a computer

```bash
latch serve          # on the computer with the sessions
latch serve token    # the bearer token to paste into Settings
```

`latch serve` binds loopback and speaks plaintext, so the address in Settings is
a tunnel to it, not the gateway itself:

- an SSH forward — `ssh -L 4610:127.0.0.1:4610 your-mac`, then
  `http://127.0.0.1:4610` (works from the iOS Simulator directly, since it
  shares the Mac's network)
- a Tailscale address on the tailnet
- a reverse proxy terminating TLS, for anything crossing the public internet

Rotating the token with `latch serve token` invalidates it for new connections
immediately; re-link in Settings afterwards.

## Staying compliant with the code contract

Latch's wire contract is schema-first. `schemas/remote-access/v1/*.schema.json`
and `fixtures/harness/*.v1.json` own it, and each client generates its types
from those documents instead of hand-copying them. This app is the Swift target
of that rule, and it keeps working across contract versions in three ways.

### 1. The Swift types are generated, not written

```bash
Tools/generate-contract.py                       # sync from the Latch repo above
Tools/generate-contract.py --upstream ~/src/Latch  # or from somewhere else
```

`Sources/LatchMobileKit/Generated/LatchContract.swift` is derived from the
schemas: the protocol major, the endpoint and feature maps, the send operations,
the idempotency key's own constraints, and every harness event variant with its
fields all come from the documents rather than from a copy someone typed. The
generator refuses to run when a schema's `$id` drifts, when an endpoint or
feature stops being a boolean, or when the event schema grows a property that no
variant claims — cases where quietly emitting Swift would be a lie.

### 2. Drift fails a check rather than surfacing at runtime

```bash
Tools/generate-contract.py --check                        # offline self-check
Tools/generate-contract.py --check --upstream ~/src/Latch  # the drift gate
```

`Contract/schemas/` holds the exact schemas the committed Swift was generated
from, and `Contract/manifest.json` records their sha256 digests. The plain
`--check` verifies the committed Swift still matches those vendored schemas,
which works offline and after this folder has moved. Adding `--upstream` also
compares the vendored copies against the canonical ones, so a new contract
version fails the check until the app is regenerated against it.

`ContractFreshnessTests` asserts the same digests from inside `swift test`, so a
hand edit to the generated file fails the suite even when nobody runs the script.

### 3. The app degrades honestly at runtime

`docs/REMOTE_SDK.md` in the Latch repository fixes the client rules, and
`GatewayCompatibility` implements them:

- `GET /v1/capabilities` is the mandatory discovery step, and its answer is
  cached rather than re-derived per screen.
- An optional endpoint is used only when the map reports it as `true`. The app
  never probes an endpoint and infers support from the error — `GatewayTests`
  asserts that no request is even sent to an undiscovered endpoint.
- A 404 on discovery identifies the pre-discovery gateway: sessions and terminal
  stay available, everything introduced alongside discovery stays off.
- A `protocolVersion` other than the supported major disables everything and
  reports which two versions disagree, rather than guessing at field meanings.
- Additive changes are survivable. Unknown fields are ignored, an unknown
  harness event type is kept and shown as unrecognized instead of being dropped,
  and an unrecognized member of a closed value set degrades to `nil` rather than
  discarding the event around it.

Discovery's answer is also visible to the user, in Settings under *What this
gateway offers*, so a missing control has a stated reason.

### When the contract changes

1. Regenerate: `Tools/generate-contract.py`.
2. Build. New required fields and new event variants surface as compile errors
   in the reducer's `switch`, which is what makes them impossible to forget.
3. Run `swift test`. The freshness tests confirm the digests match.

## Retries and idempotency

Messages and prompt resolutions carry an `Idempotency-Key`, so a retry after an
ambiguous network failure is deduplicated by the gateway for ten minutes rather
than sent twice. A retry must reuse the same key, which is why `send` takes one
instead of always minting a fresh value. Raw `keys` submissions deliberately
have no retry contract and are sent without the header — the gateway rejects it
there.

The key is only sent when discovery reports `features.idempotencyKeys`. The
gateway's `gatewayInstanceId` changing means the process restarted and its
in-memory dedupe window went with it.

## Known gaps

- No terminal view. Discovery reports the endpoint; the app does not use it yet.
- Hook responses beyond the `awaiting_input` prompt are not modeled.
- One linked computer at a time.
- The session list does not refresh on its own; pull to refresh.
