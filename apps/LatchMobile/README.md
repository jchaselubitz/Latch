# Latch Mobile

A SwiftUI iPhone app for discovering and opening Latch sessions away from the
desk. Phase 0 establishes the protocol-major-2 contract while the Conversation
Hub is built in later phases.

Everything the app needs lives in this folder, so it can be moved to its own
repository without leaving a dependency on the Latch checkout behind.

## What it does

- **Sessions** lists the sessions on the linked computer, with state, working
  directory, and idle time.
- **Settings** links the phone to one `latch serve` gateway and shows what that
  gateway reports it can do, and holds **Remote access**: pairing this phone
  with a Mac's own identity by scanning the code it shows.
- Tapping a session opens either its **terminal** or its **chat**, decided by
  the *Session view* setting and by what the session and the grant actually
  allow. Chat, composer, and interaction controls appear only after a host
  advertises its v2 Conversation Hub; this client does not probe or fall back
  to protocol major 1.

## Layout

```text
Package.swift                     LatchMobileKit: everything that is not a view
Sources/LatchMobileKit/
  Generated/LatchContract.swift   generated from the schemas; never hand-edited
  DeviceIdentity.swift            the device key, Secure Enclave and fallback
  PairingPayload.swift            the QR payload and every reason to refuse one
  PairingPhrase.swift             the phrase both screens show; a shared contract
  ControlPlane.swift              enrollment, permission refresh, revocation
  PairedDevice.swift              the paired-device record and its keychain home
  PairingModel.swift              the flow the pairing screen binds to
  LatchGateway.swift              protocol-v2 discovery and sessions client
  GatewayCompatibility.swift      protocol-major-2 discovery rules
  LinkStore.swift                 keychain storage for the address and token
  AppModel.swift                  observable session-list model
  SessionPresentation.swift       the Session view and Terminal size settings,
                                  and the pure tap-routing table
  TerminalSocket.swift            the terminal WebSocket and its close reasons
  TerminalSession.swift           attach/detach/send/resize, output as a stream
  TerminalKey.swift               logical keys and their encodings
  TerminalGeometry.swift          which grid to attach at, and when to resize
App/LatchMobile.xcodeproj         the iOS app target
App/LatchMobile/*.swift           the SwiftUI screens
App/LatchMobile/QRScannerView.swift the camera preview that reads QR codes
App/LatchMobile/SessionTerminalSurface.swift the renderer seam, and a stub
App/LatchMobile/SwiftTermSurface.swift the only file that names SwiftTerm
App/LatchMobile/TerminalKeyBar.swift the key row above the keyboard
App/LatchMobile/TerminalView.swift  the terminal screen and its states
Contract/                         vendored schemas and their digests
Tools/generate-contract.py        the contract generator and drift gate
```

The kit is a plain library so `swift test` exercises the client on a Mac with no
simulator involved. The app target consumes it as a local package rather than
compiling a second copy of the sources.

## Building and running

```bash
swift test                                     # the client; 6 more run only live
open App/LatchMobile.xcodeproj                 # then run on a simulator or device
```

From the command line:

```bash
xcodebuild build -project App/LatchMobile.xcodeproj -scheme LatchMobile \
  -sdk iphonesimulator -destination 'platform=iOS Simulator,name=iPhone 17 Pro'
```

Signing is left unset, so a device build needs a development team selected in
Xcode. The simulator needs none.

SwiftTerm ships a Metal renderer and processes `Shaders.metal` as a package
resource, so a machine without the Metal toolchain cannot build the app at all.
`xcodebuild -downloadComponent MetalToolchain` installs it (~838 MB). `swift
test` needs it too, because `TerminalEmulatorTests` compiles SwiftTerm — it is
a separate target from `LatchMobileKitTests` precisely so the kit never sees
the emulator. Run `swift test --filter LatchMobileKitTests` to exercise the
client alone.

SwiftTerm is pinned `.upToNextMinor(from: "1.18.0")`. `Package.swift` says why
and what to check before lifting the pin.

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

A paired Mac (see *Pairing with a Mac* below) reaches the same gateway without
any of this: the app tries Bonjour on the local network first, and on a miss —
or a LAN connect that fails — falls through to reading the Mac's published
presence and connecting over WebRTC ICE, requesting STUN and (unless the Mac
has relay disabled) Cloudflare TURN credentials together so a relay attempt is
available from the first try rather than only after a direct one fails.
Settings shows a **Path** row under the linked computer — Local network,
Direct, or Relay — naming whichever one the current connection actually used.

In every case the Mac itself is the thing that has to be reachable: **Latch
Desktop must be running and the Mac must be awake.** There is no server
component independent of the desktop app, so a slept, shut-down, or
Latch-quit Mac answers nothing. A `409 target_offline` reaching for a paired
Mac renders as "Your Mac is asleep or Latch is not running" rather than a raw
protocol error, because that is the actual cause almost every time this
appears.

On a Mac with `latch remote-access relay disable` (or the desktop's "never
relay" switch) turned on, a Tailscale/tailnet address the Mac publishes in its
presence is used directly as a Noise target over plain TCP — no ICE gathering
required — so a tailnet remains a working path with relay refused outright.

## The terminal view

A session with no Claude or Codex connector — every plain shell, and every
`latch run -- <anything else>` — has no conversation to show. The phone now
opens its terminal instead of telling the user to walk to their Mac.

`docs/DECISION_MOBILE_TERMINAL_FALLBACK.md` in the Latch repository is why this
exists and why the emulator was never the hard part;
`docs/PLAN_MOBILE_TERMINAL_VIEW.md` is what was built.

### Which screen a tap opens

**Settings → Session view** chooses **Terminal** or **Chat**, and it ships
defaulting to Terminal. It is a *default presentation*, not a fallback rule: a
phone set to Terminal opens a Claude session's terminal too. Either screen can
still be impossible for a given session, and the resolution is:

| Setting | Session's connector | Where the tap lands |
| --- | --- | --- |
| Terminal | any | the terminal, attaching on arrival |
| Chat | `claude` / `codex` | chat, claiming the session surface on arrival |
| Chat | none (a plain shell) | the terminal, attaching on arrival |
| Chat | unknown (a Mac too old to report the field) | chat, exactly as before this feature existed |

Each row in **Sessions** carries a trailing glyph naming its destination, so
what a tap will do to the Mac is legible before the tap. Both screens carry the
switcher to the other one, so the global default is a default and not a trap.

### It requires the `control` grant

A terminal connection is the session's single exclusive surface. Opening it
takes the terminal from whatever is showing it on the Mac — an iTerm window, or
another device — which is why the gateway requires `control` and refuses
`observe` and `interact` before the socket is opened. A phone paired with a
lesser grant is told which of the two problems it has: too little permission,
or a Mac too old to advertise the route at all. A manually entered `latch
serve` link is unrestricted, because it carries no grant header and the gateway
grants loopback requests control.

### Opening it requires the device owner, not just the device

Holding `control` is necessary but not sufficient. `TerminalUnlock` runs
`LAContext.deviceOwnerAuthentication` — Face ID or Touch ID, passcode as a
fallback rather than a refusal — before a `TerminalSession` is handed out, and
caches one passed check for five minutes so attaching, reading something else,
and reattaching costs one prompt rather than three. A phone with no passcode
set at all is refused outright. Chat runs the same check because opening a live
conversation now claims the session's exclusive terminal surface too; its
terminal stream is drained in the background while the Conversation Hub drives
what the screen renders. A terminal held with nobody watching is also given up
on its own: after two minutes with no input while the app is not frontmost, the
session is released so the Mac's one terminal surface does not stay parked on a
backgrounded phone.

### The preview is a still, not a live view

Opening a session first asks for `GET /v2/sessions/{id}/preview`, which reads
the pane without attaching and therefore takes nothing from anyone. That is why
a phone paired at `observe` — one that may never attach — still sees the
screen it is being told it cannot type at.

The preview is a **still**. It does not update. It does not follow the session.
It shows what the pane held at the moment it was captured, with the time it was
captured, and it changes only when the user taps **Refresh** or **Attach**. A
still of a full-screen application carries no scrollback, because the alternate
screen has no history to read.

The still is also what decides the attach geometry. **Settings → Terminal
size** defaults to *Match the Mac*, which attaches at the grid the preview
reported: the pane does not resize at all — no `SIGWINCH`, no reflow, and a
paused prompt that cannot repaint transfers exactly as it stands. The phone
renders that grid at whatever font size fits and pans when it does not. The
other choices — *Readable*, *80 × 24*, *100 × 30* — set the grid here instead.
The soft keyboard and rotation never resize the pane; only a deliberate change
of this setting does.

### Scrollback, honestly

Once attached, the live surface's scrollback is only what has arrived since the
steal. Nothing that scrolled past before the phone attached can be scrolled
back to, and after a background/reattach cycle it is empty again.

That is a property of exclusive attach, not a defect of this feature: the first
frame after a steal is a paint of the pane's *current* screen, and everything
after it is the agent's own byte stream, unchanged. The preview can carry a
bounded tail of primary-screen history before the attach; the live surface
cannot reconstruct one after it.

### The key bar

An iPhone soft keyboard has no Escape, Control, Tab, or arrows, which are
exactly the keys the prompts this feature exists to reach are answered with. A
single 34pt row rides above the keyboard as its accessory view: `esc ctrl tab ←
↓ ↑ → ⌃C …`, the whole row scrolling horizontally, with a keyboard-dismiss
button pinned at the trailing edge so it never scrolls away. `ctrl` is sticky —
tap to arm for one key, long-press to lock — and it modifies the system
keyboard as well as the bar. Arrows repeat on hold.

At 375 pt, the narrowest supported width, everything through `→` is on screen
without scrolling; `⌃C` is the first key that costs a swipe, and stops costing
one at 390 pt.

The bar emits logical keys and the emulator encodes them against its live
cursor-key mode, because `↑` is `ESC [ A` normally and `ESC O A` under DECCKM,
and a bar with the sequence baked in would work in a shell and send garbage
into a TUI. The bar is not shown while the session is not attached: there is
nothing to type at during a preview.

### Lifecycle

Holding the desk's only surface is not something to do by accident, so all
three of these are deliberate: leaving the screen detaches, backgrounding the
app detaches, and returning to the foreground does **not** silently reattach —
it returns to a closed screen with a **Reattach** button, because reattaching
is another steal and the user should watch it happen.

## Pairing with a Mac

Linking to a `latch serve` gateway with an address and a token is one way to
reach a computer. Pairing is the other: the phone enrols its own long-lived
identity with the Mac, and from then on each side authenticates the other by
key rather than by a shared bearer token. `docs/REMOTE_ACCESS_IMPLEMENTATION_PLAN.md`
and `docs/REMOTE_ACCESS_THREAT_MODEL.md` in the Latch repository own the rules;
this is what the phone does about them.

### The device identity

On first launch the app creates an X25519 key — the identity the remote-access
transport's `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake needs — and only its
public half ever leaves the phone, as the 32-byte lowercase hex the Mac's
pairing record stores.

The Secure Enclave holds P-256 keys, not X25519 ones, so "the key is in the
Enclave" is not literally available. What the app does instead is keep the
X25519 private key sealed under a non-exportable Enclave P-256 key: the
ciphertext in the keychain is inert on any other device and unwrappable only by
this hardware. Where there is no Enclave — the simulator, older hardware — the
key falls back to the keychain alone, `WhenUnlockedThisDeviceOnly`, and the
difference is recorded on the identity and shown on the pairing screen rather
than being quietly papered over.

### The QR payload

The code is the Mac's `PairingMaterial`: `formatVersion`, `pairingId`,
`secret`, `macPublicKey`, `expiresAt`, and optionally `controlPlane` and
`macName`. It is one-time, and the Mac gives it five minutes.

Every scan is untrusted input, and the scanner re-reads the same code many
times a second, so validation is unconditional and up front: the version gate
comes before any other field, identifiers and keys must be hex of exactly the
right length, an expiry that has passed and an expiry further out than the Mac
is allowed to grant are both refused, and a `macName` headed for the
confirmation screen is length-bounded and stripped of control characters.
Expiry is then rechecked at confirmation, because reading the phrase takes time.

### The pairing phrase

Nothing the phone can check by itself rules out a control plane or relay that
substituted its own key for the Mac's: both machines would still agree, with
the attacker in between. The phrase is what closes that: six words derived from
the pairing transcript — the pairing identifier and both public keys — under a
domain-separated SHA-256, shown on both screens for the person to compare.

That derivation is a cross-client contract. `PairingPhrase` in this package and
`pairing_phrase` in `crates/latch/src/cli/remote_access.rs` implement it, and
both test suites assert the same fixed vector against the same inputs, so the
two cannot drift apart without a test saying so.

### Where to enroll

`controlPlane` is what tells the phone where to present the secret, and only
the Mac can put it there: Latch Desktop attaches it after registering the code
with the control plane it is configured for, and the CLI — which has no HTTP
client — cannot. A code that carries one is authoritative, because it came from
the Mac being paired with.

A Mac with no control plane configured produces a code that is complete in
every other way, and that used to be a dead end reading "this pairing code does
not say where to enroll, and no address was given." The confirmation screen now
asks for the address instead, remembers it, and reuses it for later codes. A
typed address never overrides one the code carries.

### Enrolling

The phone posts the one-time secret and its public key to the control plane:

```text
POST /v1/pairings/{pairingId}/confirm   enrol; answers with the granted device
GET  /v1/devices/{deviceId}             re-read the grant and revocation state
POST /v1/devices/{deviceId}/revoke      revoke this phone from this phone
```

The answer's Mac key is compared against the one in the QR code, and a
different key stops pairing rather than saving it. What comes back — the opaque
device id, the granted permission, and a short-lived control-plane token — is
persisted in the keychain, alongside the Mac's pinned key. The pairing secret is
not: it is one-time, enrollment consumed it, and keeping it would undo the point.

### Permission and revocation

The Mac decides what a phone may do — `observe`, `interact`, `control` — and
the phone displays that answer rather than deciding for itself; an unrecognized
grant degrades to `observe` rather than to the default. The grant is re-read
at startup, when the sessions list appears or is refreshed, when the pairing
screen appears, and before reconnecting after suspension. That keeps a route
from snapshotting the permission saved at pairing time after the Mac has
changed it. A control plane that no longer knows the device reads as a
revocation; a network failure does not. New pairings begin at `control`, so
terminal access is on until the Mac owner switches it off.

Unpairing from the phone deletes the record and the device identity whether or
not the control plane can be reached, so the local half of a revoke never
depends on the network. Revoking on the Mac remains what closes connections
that are already open.

The camera permission is modeled rather than read inline, because a first-run
prompt, a trip to Settings, and a device with no camera are three different
dead ends — and in all of them the code can still be typed in by hand.

## Staying compliant with the code contract

Latch's wire contract is schema-first. `schemas/remote-access/v2/*.schema.json`
owns the gateway, terminal, conversation item, state, and message protocol. This
app vendors those documents and generates its Swift wire types from that set.

### 1. The Swift types are generated, not written

```bash
Tools/generate-contract.py                       # sync from the Latch repo above
Tools/generate-contract.py --upstream ~/src/Latch  # or from somewhere else
```

`Sources/LatchMobileKit/Generated/LatchContract.swift` is derived from the
schemas: protocol major, endpoint and feature maps, conversation items and state,
snapshots and mutations, operation results, history, and client actions. The
generated source records a digest of the complete canonical schema set, so any
schema edit makes the freshness check fail until the client is regenerated.

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

- `GET /v2/capabilities` is the mandatory discovery step, and its answer is
  cached rather than re-derived per screen.
- An optional endpoint is used only when the map reports it as `true`. The app
  never probes an endpoint and infers support from the error — `GatewayV2Tests`
  asserts that no request is even sent to an undiscovered endpoint.
- A 404 on discovery is an unsupported gateway; there is no v1 fallback.
- A `protocolVersion` other than the supported major disables everything and
  reports which two versions disagree, rather than guessing at field meanings.
- Additive changes are survivable. Unknown fields are ignored and an unknown
  message status renders as `complete` rather than failing the conversation.

Discovery's answer is also visible to the user, in Settings under *What this
gateway offers*, so a missing control has a stated reason.

### When the contract changes

1. Regenerate: `Tools/generate-contract.py`.
2. Build. Required fields and message variants are explicit generated types.
3. Run `swift test`. The freshness tests confirm the digests match.

## Known gaps

- The terminal view has not been exercised on a physical device. It builds and
  its logic is under test — the eleven `fixtures/vt` streams replay through the
  emulator in `Tests/TerminalEmulatorTests`, and both cursor-key modes are
  asserted against recorded Claude and Codex traffic — but the accessory bar's
  behaviour above a real soft keyboard, the feel of sticky `ctrl` and
  hold-to-repeat, and paint throughput under `high-rate-output` are device
  facts and remain unverified.
- Terminal scrollback begins at the attach. See *Scrollback, honestly* above;
  it follows from exclusive attach rather than from this app.
- A paired phone uses the pinned Noise tunnel for its session list, reaching
  the Mac by Bonjour first and falling through to presence-plus-ICE off the
  local network; the manually entered `latch serve` link remains a coequal
  route. STUN and TURN are now requested together so relay is available from
  the first attempt rather than only after a direct failure, and ICE's own
  pair priority is what keeps a reachable direct path preferred. Physical NAT,
  cellular, captive-portal, sleep/wake, and relay-soak validation are release
  gates rather than completed device evidence — see
  `docs/REMOTE_ACCESS_PHASE_4.md` in the Latch repository for what has and has
  not been run.
- The terminal's Face ID/passcode gate has not been exercised on a physical
  device with biometric enrollment; the simulator has none. The permission
  logic around it (grant-only vs. grant-plus-owner-check) is under unit test.
- Conversation actions remain unavailable until the Hub implementation lands.
- One linked computer at a time.
- The session list does not refresh on its own; pull to refresh.
