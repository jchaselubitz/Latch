# Plan: the mobile terminal view, the presentation setting, and the key bar

**Status:** implemented. All seven phases landed; the divergences from what is
written below are recorded in *Where the build diverged from this plan* at the
end of this document. One item remains unverified rather than undone: the
on-device checks in phase 5, which need a physical phone.
**Scope:** `apps/LatchMobile`; on the Mac, one additive field on the session
list, one new read-only route (`GET /v2/sessions/{id}/preview`), and one shared
helper in `crates/latch/src/conversation/connectors`.
**Implements:** the product shape recommended in
`[DECISION_MOBILE_TERMINAL_FALLBACK.md](DECISION_MOBILE_TERMINAL_FALLBACK.md)`,
plus two things that record did not cover — a user-chosen default presentation,
and the key accessory bar specified against Termius.

That record established what to build and what to build it on. This is the
build order, the types, the file list, and the decisions that are still open at
each step. It does not re-argue the emulator choice; it does hold that choice
behind a seam so the argument stays reopenable.

---



## What a person gets

1. **A terminal on the phone.** Tapping a session can open the session's real
  terminal, drawn natively, fed by `WS /v2/sessions/{id}/terminal` — the
   endpoint the Mac has advertised since protocol 2 and the phone has never
   opened.
2. **A setting that decides which screen a tap lands on.** Settings → *Default
  session view* → **Terminal** or **Chat**. It ships defaulting to Terminal.
3. **A keyboard that can answer a TUI.** A single-row, horizontally scrolling
  key bar above the soft keyboard carrying `esc`, `ctrl`, `tab`, arrows, and
   the punctuation an iOS keyboard buries, with a pinned button to put the
   keyboard away.
4. **A look at the session before taking it.** The terminal screen opens on a
   still of the pane — read over tmux, without attaching and therefore without
   stealing — so a permission prompt is legible before the user decides to take
   the surface, and so the attach can be made at the desk's own geometry.

---



## The one place this overrides the decision record

The record recommended landing on the terminal screen and making *attach* a
deliberate second action, because connecting steals the session's single
exclusive surface: it detaches iTerm with a `4409`, resizes the pane to the
phone, and `SIGWINCH`es the child.

The direction for this objective is explicit: when Terminal is the default
presentation, opening the session steals it immediately. That is the decision;
this plan implements it. Three things make it a defensible one rather than a
surprise, and all three are requirements below, not polish:

- **The steal is announced.** A transient banner on arrival — *"Took the
terminal from your Mac"* — so the effect on the other machine is visible from
the machine that caused it.
- **The setting is the consent.** A user who does not want a tap to be
destructive switches the default to Chat, and the terminal becomes a
deliberate per-session action from the view switcher.
- **Only running sessions auto-attach.** An `exited`, `lost`, or `stopping`
session lands on the screen without connecting, because there is nothing to
steal and a spawned attach against a dead pane would close with
`session_exited` and read as a failure.

Everything else in the record's "what holds regardless" section is carried
through unchanged: the `control` grant gate, the backgrounding rule, and the
key bar.

**The preview in Phase 1′ softens this without contradicting it.** The reason
auto-attaching looked necessary is that the alternative — landing on a screen
and waiting — meant landing on *nothing*, and a blank screen is not a decision
anyone can make. A no-steal preview removes that: the user sees the prompt
first, and attaching becomes a choice that costs nothing to defer. Terminal
still auto-attaches as directed; the preview is what makes every path that does
*not* auto-attach — the Chat default, a non-running session, an
`observe`-paired phone — useful rather than a polite dead end. Whether it
should change the auto-attach default is a product call, raised as open
question 1.

---



## The presentation model



### The preference

```swift
// Sources/LatchMobileKit/SessionPresentation.swift
public enum SessionPresentation: String, CaseIterable, Codable, Sendable {
    case terminal
    case chat

    public static let `default` = SessionPresentation.terminal
}
```

Stored the way the control-plane address is stored, and for the same reason —
it is a preference, not a credential:

```swift
public protocol SessionPresentationStoring: Sendable {
    func load() -> SessionPresentation
    func save(_ presentation: SessionPresentation)
}

public struct UserDefaultsSessionPresentationStore: SessionPresentationStoring { … }
public final class MemorySessionPresentationStore: SessionPresentationStoring { … }
```

This mirrors `ControlPlaneAddressStoring` exactly — protocol, `UserDefaults`
implementation for the app, in-memory implementation for tests. No new storage
idiom is introduced.

### Resolving a tap

Preference alone cannot decide, because either screen can be impossible: chat
needs a connector and the `conversation` endpoint; terminal needs the
`terminal` endpoint and the `control` grant. Resolution is a pure function so
it can be tested as a table rather than through navigation:

```swift
public enum SessionRoute: Equatable, Sendable {
    case terminal(autoAttach: Bool)
    case chat
    case unavailable(SessionRouteBlock)
}

public enum SessionRouteBlock: Equatable, Sendable {
    case needsControlGrant       // paired at observe/interact
    case noTerminalEndpoint      // Mac too old, or endpoint disabled
    case noConversation          // no connector and no terminal either
}

public static func route(
    preference: SessionPresentation,
    connector: SessionConnector,
    surface: SessionSurface,
    isRunning: Bool
) -> SessionRoute
```


| Preference | Connector             | Terminal available | Chat available | Route                                                       |
| ---------- | --------------------- | ------------------ | -------------- | ----------------------------------------------------------- |
| Terminal   | any                   | yes                | any            | `.terminal(autoAttach: isRunning)`                          |
| Terminal   | `.named` / `.unknown` | no                 | yes            | `.chat`                                                     |
| Terminal   | `.none`               | no                 | —              | `.unavailable(.needsControlGrant)` or `.noTerminalEndpoint` |
| Chat       | `.named`              | any                | yes            | `.chat`                                                     |
| Chat       | `.unknown`            | any                | yes            | `.chat`                                                     |
| Chat       | `.none`               | yes                | —              | `.terminal(autoAttach: false)`                              |
| Chat       | `.none`               | no                 | —              | `.unavailable(…)`                                           |


Two rows deserve their reasons stated.

`Chat` + `.none` routes to the terminal **without** auto-attaching. The user
asked for chat and is getting something else; the steal is not implied by that
tap. This is the record's original fallback, and it keeps its original manners.

`Chat` + `.unknown` still opens chat. An older Mac that omits the field must
keep behaving as it does today, including `ChatView`'s existing "connector is
null" screen — which gains an *Open terminal* button rather than staying a dead
end.

### Switching per session

Both screens carry a toolbar control (`Menu` behind an
`arrow.left.arrow.right.square` glyph) offering the other view when it is
available for that session. It changes the screen, not the default. This is the
escape hatch that keeps the global setting from being a trap, and it is how a
user opens a terminal on an agent session without changing their preference.

---



## Phase 0 — one additive field on the session list

Routing needs to know whether a session has a connector before any socket is
opened. It is not in `GET /v2/sessions` today, and probing is not an option:
the terminal socket is the only other source, and opening it would steal the
desk before the user chose anything.

### Mac side

The answer already sits in memory during `manage::list` — `meta::read` is
called for every session, and `Meta.harness` is the exact input
`connector_for_session` matches on. To guarantee the list and the Hub can never
disagree, extract the match rather than repeat it:

```rust
// crates/latch/src/conversation/connectors/jsonl.rs
/// The connector a session's persisted harness marker selects, or `None` when
/// the session has no conversation connector and never will.
pub fn connector_kind(harness: Option<&str>) -> Option<&'static str> {
    match harness {
        Some("claude") => Some("claude"),
        Some("codex") => Some("codex"),
        _ => None,
    }
}
```

`connector_for_session` is rewritten to call it. `manage::list` calls it too:

```rust
// crates/latch/src/cli/json.rs — SessionSummary
/// The conversation connector this session uses, or null when it has none.
/// Absent from older gateways, which is not the same as null.
pub connector: Option<String>,   // NOT skip_serializing_if
```

The `skip_serializing_if` omission is deliberate and is the only subtle part of
this phase. `absent` means *this Mac predates the field*; `null` means *this
session has no connector*. Collapsing them would make an old Mac look like a
fleet of shells and route every tap to a terminal.

`InspectReport` gains the same field in the same commit, so
`GET /v2/sessions/{id}` does not contradict the list.

**This also settles the transience worry the decision record raised.** The
record noted that `connector: null` in *conversation state* is permanent for a
shell but transient for an agent whose transcript has not bound yet. The
harness marker is not: `meta::new` writes it from launch argv at session
creation, before the session can appear in a list at all. A Claude session
tapped two seconds after `latch run` reports `"claude"` immediately.

### Phone side

```swift
// Sources/LatchMobileKit/Models.swift
public enum SessionConnector: Equatable, Sendable {
    case unknown            // the gateway omitted the field
    case none               // explicit null: this session is a terminal
    case named(String)      // "claude", "codex"
}
```

Decoded with the tri-state preserved, which `decodeIfPresent` cannot do:

```swift
if !container.contains(.connector) {
    connector = .unknown
} else if try container.decodeNil(forKey: .connector) {
    connector = .none
} else {
    connector = .named(try container.decode(String.self, forKey: .connector))
}
```

`command_label` is not a substitute and must not be used as one: it describes
what was launched, not whether a connector bound to it.

**Not a contract-schema change.** `SessionSummary` is hand-written in
`Models.swift` against `latch list --json`; the session list has no schema in
`schemas/remote-access/v2/`, so `Tools/generate-contract.py` and
`Contract/manifest.json` are untouched and `ContractFreshnessTests` stays
green. Giving the session list a schema is a reasonable separate change; it is
not a prerequisite for this one.

**Tests:** a Rust test that a shell session lists `connector: null` and a
`claude` session lists `"claude"`; a Swift decode test for all three states,
including a payload with the key entirely absent.

---



## Phase 1 — the terminal socket client

No UI. This phase ends with a testable client and a fake connection, the same
shape `ConversationSocketTests` uses.

### `LatchGateway.openTerminal`

Follows `openConversation` line for line — `require(.terminal)`, swap the
scheme to `wss`, set the path, carry the bearer token — with the query being
`cols` and `rows` instead of a resume tuple:

```swift
public func openTerminal(
    sessionID: String,
    cols: Int,
    rows: Int
) async throws -> any TerminalSocketConnection
```

Size goes on the URL rather than in a handshake frame. The gateway accepts
both, and the query form skips a round trip during which the socket is holding
a steal in reserve against a 10-second deadline. **A size is never guessed** —
see the geometry
rule below, which now prefers the desk's own grid over anything derived from
the phone.

### `TerminalSocket`

New `Sources/LatchMobileKit/TerminalSocket.swift`. Connection seam, byte
framing, and close-reason capture:

```swift
public protocol TerminalSocketConnection: Sendable {
    func receive() async throws -> Data
    func send(_ bytes: Data) async throws
    func sendControl(_ text: String) async throws   // {"type":"resize",…}
    func cancel()
    var closeCode: Int? { get }
}
```

`URLSessionTerminalSocketConnection` wraps `URLSessionWebSocketTask`, sends
input as `.data`, resize as `.string`, and reads `task.closeCode.rawValue`
after a receive failure so `TerminalCloseReason.forCloseCode(_:)` — already
generated in `LatchContract.swift` — turns `4409` into `.stolen` rather than a
generic transport error.

**The one place this deliberately diverges from** `ConversationSocket`**: there is
no automatic reconnect.** `ConversationSocket` retries with backoff because
reopening a conversation is free. Reopening a terminal is another steal.
Silent retry would let a phone in a pocket repeatedly take the surface back
from someone working at the desk. On close, the socket stops and reports why;
reattaching is an action with a button.

### `TerminalSession`

New `Sources/LatchMobileKit/TerminalSession.swift`. `@MainActor @Observable`,
one per session, retained by `AppModel` beside `conversationStores`:

```swift
public enum TerminalSessionState: Equatable, Sendable {
    case idle
    case connecting
    case attached
    case closed(TerminalCloseReason?)
    case failed(String)
}

@MainActor @Observable public final class TerminalSession {
    public private(set) var state: TerminalSessionState
    public private(set) var stoleSurface: Bool     // drives the arrival banner

    public func attach(cols: Int, rows: Int)
    public func detach()                            // releases the surface
    public func send(_ bytes: ArraySlice<UInt8>)
    public func resize(cols: Int, rows: Int)
    public var output: AsyncStream<Data> { get }
}
```

Output is a stream rather than a stored buffer: the renderer owns scrollback,
and a second copy of a fast agent repaint in an `@Observable` property would be
a per-byte view invalidation. Nothing about the terminal is re-rendered by
SwiftUI.

`AppModel` gains `terminalSession(for:)` — gated on `surface.terminal` the way
`conversationStore(for:)` is gated on `.conversation` — plus
`suspendTerminals()` and `detachAllTerminals()`.

**Tests:** attach sends the declared size; input reaches the connection as
binary; resize arrives as `{"type":"resize","cols":…,"rows":…}`; close code
`4409` surfaces as `.closed(.stolen)`; close code `4410` as
`.closed(.sessionExited)`; a closed socket does **not** reconnect on its own.

---



## Phase 1′ — the pane preview, which does not steal

This phase did not exist in the first draft. It comes from asking whether a
steal could paint some history so a user can see and answer a permission prompt
that paused the agent. Investigating that produced a better answer than the
question assumed.

### What the kernel already does

A stealing client does not arrive at a blank screen. The patched kernel attaches
in two phases: the client enters `LATCH_RAW_SNAPSHOT`, tmux's own redraw
machinery paints the pane's current grid to the new tty, and only once
`CLIENT_ALLREDRAWFLAGS` clears does it record a byte offset and flip to
`LATCH_RAW_LIVE` for the raw splice
(`patches/tmux/0001-latch-exclusive-raw-attach.patch`).

For a paused agent the current frame *is* the prompt — nothing has been painted
since it started waiting. `DECISION_EXCLUSIVE_ATTACH.md` has exactly this as its
worked example: iTerm shows the trust prompt, a phone steals ten minutes later,
and receives that prompt as the current frame.

### Why more frames would deliver nothing

`DECISION_SCROLLBACK.md` already measured it. Every recorded fixture was
replayed through the screen model and its scrollback ring serialized:

| Fixture                   | Lines in the ring |
| ------------------------- | ----------------- |
| `claude-code-startup`     | 0                 |
| `claude-code-turn`        | 0                 |
| `claude-code-trust-prompt`| 0                 |
| `codex-startup`           | 0                 |

Agent TUIs live on the alternate screen and overwrite in place. They never
scroll, so they produce no history at all, and for the case this feature exists
to serve there is no handful of frames to paint — there is one frame, which the
kernel already sends. Replaying alt-screen paints is separately rejected by the
record as slow, desynchronization-prone, and pointless because only the last
paint matters.

### The mechanism that is actually missing

`engine::capture_pane` (`crates/latch/src/engine.rs:783`) already shells
`tmux capture-pane -p -J`, and `engine.rs:134` sets `history-limit 50000`. The
property that matters is not the history:

> **`capture-pane` does not attach, so it does not steal.**

That is a read of the live pane available to a phone paired at `observe`, and
it is not exposed over the gateway today.

### `GET /v2/sessions/{id}/preview`

A new route in `crates/latch/src/cli/serve/routes.rs`, at `Grant::Observe` —
the first terminal-shaped thing an observing phone may do, and legitimately so,
because it takes nothing:

```rust
RouteSpec {
    id: RouteId::Preview,
    pattern: "/v2/sessions/{id}/preview",
    method: "GET",
    required_grant: Grant::Observe,
},
```

`engine::capture_pane_with_timeout` gains an options struct rather than more
positional arguments:

```rust
pub struct CapturePaneOptions {
    /// Emit SGR escapes (`-e`) so the preview keeps its colors.
    pub styled: bool,
    /// Lines of primary-screen scrollback above the viewport (`-S -<n>`).
    pub scrollback_lines: u32,
}
```

Response:

```json
{
  "content": "…escape-encoded pane…",
  "cols": 100,
  "rows": 30,
  "alternateScreen": true,
  "capturedAt": "2026-08-24T09:41:02Z",
  "scrollbackLines": 0
}
```

`cols`/`rows` come from `#{pane_width}`/`#{pane_height}` and `alternateScreen`
from `#{alternate_on}`, both ordinary tmux formats. All three earn their place:

- **`alternateScreen`** tells the client whether scrollback is meaningful at
  all. The alternate screen has none, so a preview of an agent is one screen and
  a preview of a shell can carry a tail above it. The client stops asking for
  scrollback it cannot receive.
- **`cols`/`rows` are the desk's current geometry**, which is what makes the
  attach non-destructive — see the geometry rule below.
- **`capturedAt`** because a preview is stale the moment it is taken, and a
  screen that shows a still must say so.

`content` is the pane's rows joined by newlines, with **no trailing newline**.
tmux emits one; the gateway strips it, because fed into a grid exactly `rows`
tall it scrolls the still up a line and costs the top row. See open question 3,
which this measurement settled.

Bounds and manners:

- The gateway's capture deadline is **2 seconds**, not the 30 the existing
  helper defaults to. A preview is a screen-open convenience; if tmux is too
  busy to answer, the screen says so and offers Attach.
- **Not polled.** One capture on screen open, one per explicit Refresh. This is
  a tmux round trip per call, and the session list must not fan out into one per
  row.
- `scrollback_lines` is capped server-side (200 lines, matching the number
  `DECISION_SCROLLBACK.md` chose for the same reason) and ignored when the
  alternate screen is active.
- Capture does not disturb the first-viewer gate. That hazard was already
  handled: `engine::raw_surface_acknowledged` deliberately reads
  `#{client_flags}` rather than `session_attached`, precisely because
  administrative tmux clients are clients too. A preview on a session that has
  never had a viewer will show an empty pane, because the agent has not started
  — which is correct, and the screen should say the session is waiting for its
  first viewer rather than render a blank grid.

### Contract consequences

Unlike Phase 0, this one **does** touch the generated contract.
`endpoints.preview` is a new field on `gateway-capabilities.schema.json`, so
`Tools/generate-contract.py` must be re-run and `Contract/manifest.json`
digests updated — `ContractFreshnessTests` fails until they are.

It is added as an **optional** property, not a required one, and
`GatewayEndpoints` decodes it through a hand-written `init(from:)` defaulting
to false. A Mac that predates this route omits the key; making it required
would fail the whole discovery document over one endpoint the app can do
without, which is the opposite of additive.
`GatewayEndpointsName` gains `.preview`, which flows automatically into
`SettingsView`'s capability rows through its existing `allCases` loop; it needs
only a `label(_:)` arm.

### Phone side

`LatchGateway.previewSession(id:scrollbackLines:)` returns a `SessionPreview`,
and `TerminalSession` gains a `.preview(SessionPreview)` state ahead of
`.idle`.

The preview is painted **by the same renderer as the live stream** — it is
escape-encoded pane content, so it goes through `SessionTerminalSurface.feed(_:)`
unchanged. That is the whole reason to capture with `-e`: no second display path
exists, and a still of the session and a live session look identical because
they are drawn by the same code.

One ordering rule, or the two will interleave: the surface gains `reset()`, and
attaching calls it before the first live byte. Otherwise the kernel's own
current-frame paint lands on top of a preview drawn at a different geometry and
the screen is a composite of two moments.

**Tests:** a Rust test that preview returns content for a running session
without changing `#{client_flags}` (nothing was stolen); that an `observe` grant
is accepted where the terminal route refuses it; that `scrollback_lines` is
ignored while `alternate_on` is true; and a Swift test that `.preview` precedes
`.connecting` and that `reset()` is called exactly once on attach.

### What this does not solve

The preview is a still. It does not update, so a session that repaints after
capture is misrepresented until Refresh or Attach. That is acceptable for its
job — deciding whether to take the surface — and it must not be sold as
anything else. A live observing terminal remains refused by
`ARCHITECTURE_RULES.md`, and this route does not reopen that: it reads the
grid once, exactly as the input-safety classifier already does.

---


## Phase 2 — the setting

`AppModel` gains `sessionPresentation`, read from the store at init and written
on change. `SettingsView` gains a row in a new section placed above *Remote
access*, because it is the setting a person will look for first:

```
Session view
  ( ) Terminal        The session's real terminal. Opening one takes it
                      from your Mac.
  ( ) Chat            Latch's conversation view. Available for Claude and
                      Codex sessions.
```

A `Picker` with `.pickerStyle(.inline)` inside a `Section`, footer:

> Terminal opens the session's live terminal and takes it from whatever is
> attached on your Mac. Sessions without a Claude or Codex connector — every
> plain shell — always open in the terminal.

That last sentence is the honest statement of the fallback: choosing Chat does
not make chat possible where there is no connector.

Also in this phase: `SessionSurface` gains `terminal`.

```swift
public struct SessionSurface {
    public let chat: Bool
    public let composer: Bool
    public let interactionControls: Bool
    public let terminal: Bool          // new
}
```

`GatewayCompatibility.sessionSurface(for:)` sets it from the `.terminal`
endpoint. `restricted(to:)` gains the grant rule the record called for:

```swift
public func restricted(to permission: DevicePermission?) -> SessionSurface {
    guard let permission else { return self }          // manual link: loopback grant is control
    let terminal = self.terminal && permission.permits(.control)
    guard !permission.permits(.interact) else {
        return SessionSurface(chat: chat, composer: composer,
                              interactionControls: interactionControls, terminal: terminal)
    }
    return SessionSurface(chat: chat, composer: false,
                          interactionControls: false, terminal: terminal)
}
```

`permission == nil` staying unrestricted is correct and worth a comment in the
code: a manually linked `latch serve` tunnel sends no `x-latch-device-grant`,
and `http.rs` grants loopback requests `Grant::Control`.

**Tests:** an `observe` phone and an `interact` phone both resolve
`surface.terminal == false`; a `control` phone resolves `true`; a manual link
resolves `true`; the route table above, exercised as a parameterised test.

---



## Phase 3 — the renderer, behind a seam

`planning/PROJECT_ARCHITECTURE.md` requires the emulator to be hidden behind
Latch's own session-view API, and the decision record made that the load-bearing
requirement rather than the library choice. The seam is small because the wire
contract is raw bytes both ways:

```swift
// App/LatchMobile/SessionTerminalSurface.swift
protocol SessionTerminalSurface: AnyObject {
    func feed(_ bytes: Data)
    func setFocus(_ focused: Bool)
    var onInput: (ArraySlice<UInt8>) -> Void { get set }
    var onSizeChange: (Int, Int) -> Void { get set }
    /// Encodes a logical key using the terminal's current modes.
    func encode(_ key: TerminalKey) -> [UInt8]
    func paste(_ text: String)
}
```

`encode(_:)` is on the seam rather than in the key bar on purpose — see the key
encoding section.

**Implementation:** `SwiftTermSurface`, a `UIViewRepresentable` over
SwiftTerm's iOS `TerminalView`, added to `Package.swift` as the app's first
third-party UI dependency. The delegate supplies input and size changes; bytes
are fed straight from `TerminalSession.output`. Exact SwiftTerm signatures are
pinned during the spike, not asserted here.

### The measurement gate before SwiftTerm is committed

`DECISION_EMULATOR.md` set the precedent: candidates were built against every
case in `fixtures/vt/` and measured against each fixture's hand-authored
`expected.json`, rather than compared on documentation. The eleven fixtures are
recorded Claude Code and Codex PTY streams — exactly the traffic this surface
will carry, including `claude-code-trust-prompt`, `claude-code-resize-alt-screen`,
`high-rate-output`, and `unicode-wide-combining-emoji`.

Add `Tests/LatchMobileKitTests/Fixtures/` — or a small on-device harness app,
if measuring paint rate needs one — that feeds each `input.bin` at the fixture's
declared size and asserts the `contains` strings, `alternate_screen`, cursor
visibility, and scroll region from `expected.json`. Run it against SwiftTerm
before the dependency is committed. If it disagrees with the shape argument,
the measurement wins, exactly as it did in `DECISION_EMULATOR.md`.

`high-rate-output` is the one to watch on a device rather than a simulator; it
is the fixture that answers whether the emulator keeps up with a repainting
TUI, which is the criterion the whole choice rests on.

---



## The geometry rule: the phone chooses the grid, and prefers the desk's

This is the detail most likely to be discovered late and be expensive, so it is
stated once, here, and referenced by the phases that depend on it. The first
draft of this plan derived `cols`/`rows` from the phone's pixels. That was
wrong in a way the preview makes fixable.

### Geometry is a choice, not a measurement

`cols` and `rows` are query parameters on the terminal socket. Nothing requires
them to describe the phone's screen. Deriving them from pixels means a ~50-column
grid on an iPhone in portrait, which resizes the pane, `SIGWINCH`es the child,
and asks a layout drawn for 100 columns to survive being halved.

Instead the phone picks a grid and renders it at whatever font size fits,
panning when it does not:

- **Default: the desk's current geometry**, read from the preview's `cols`/`rows`.
  Attaching at the size the pane already has means the pane does not resize at
  all — no `SIGWINCH`, no reflow, and a paused prompt that cannot repaint is
  transferred exactly as it stands. For the case this whole feature exists to
  serve, this is the safest possible attach.
- **Fallback when there is no preview: a readable fit.** The largest grid whose
  font size stays at or above 11 pt for the viewport width, floored at 60x20.
- **Override in Settings:** *Terminal size* — Match the Mac / Readable / 80x24 /
  100x30.

Font size follows from the grid (`viewportWidth / cols / advanceRatio`), not the
other way round. Below the readable floor the font stops shrinking and the
surface pans horizontally, with pinch-to-zoom adjusting font size only.

### Nothing about the viewport sends a resize

Three consequences, and they are the point:

- **The soft keyboard never resizes the pane.** It covers the bottom of a grid
  whose dimensions did not change; the surface scrolls to keep the cursor
  visible. Sizing from the visible region would `SIGWINCH` and reflow a
  full-screen agent TUI twice per keyboard interaction — a phone that opened a
  terminal would be visibly worse for the session than one that did not.
- **Rotation never resizes the pane** either. It changes the font size and the
  pan extent. This retires what the first draft listed as an open question.
- **Resize frames are sent only when the user changes the grid** — the Settings
  control, or a deliberate "fit to phone" action on the terminal screen — each
  debounced by ~150 ms.

### The reassuring measurement

`fixtures/vt/claude-code-resize-alt-screen` records Claude Code being resized
100x30 to 72x20 while the alternate screen is active, and its `meta.json` says
it "redraws in response"; `expected.json` confirms a correct grid at the new
size with the cursor re-parked. So for the harnesses Latch actually hosts, a
resize does produce a native repaint at the new geometry rather than a reflowed
grid.

That is why matching the desk is a default and not a hard requirement: it
protects the applications that *cannot* repaint, and Claude and Codex are not
in that set. It is also the measurement that would have to fail before any of
this needed rethinking.

---


## Phase 4 — routing and the terminal screen



### The tap

`SessionsView` stops pushing `ChatView` unconditionally and pushes the resolved
route instead:

```swift
NavigationLink(value: SessionDestination(session: session, route: route(for: session))) { … }
```

The row gains a small trailing glyph — `terminal` or
`bubble.left.and.bubble.right` — so the destination is legible **before** the
tap. On a build where the tap is destructive, telling the user where it goes is
not decoration.

**Opening order on the terminal screen**, and it is the same order whether or
not the tap auto-attaches:

1. Request the preview. It needs only `observe`, so this succeeds on phones
   that may never attach.
2. Paint it, and take the attach geometry from its `cols`/`rows`.
3. If the route said `autoAttach`, `reset()` the surface and connect. If it did
   not, stop here with an **Attach** button.

The preview and the socket are never in flight together: the geometry for step
3 comes from step 2, and issuing them concurrently would mean guessing the size
after all.

### `TerminalView`

New `App/LatchMobile/TerminalView.swift`:

```
┌─────────────────────────────────────┐
│ ‹ Back   session-name        ⇄  ⋯  │  navigation bar
├─────────────────────────────────────┤
│ Took the terminal from your Mac     │  transient, ~3s
├─────────────────────────────────────┤
│ Still from 09:41 · Attach to type   │  preview only, until attached
├─────────────────────────────────────┤
│                                     │
│  SwiftTermSurface                   │  full remaining area
│                                     │
├─────────────────────────────────────┤
│ esc ctrl tab ← ↓ ↑ → | ~ / …   ⌨︎↓ │  key bar (with keyboard)
└─────────────────────────────────────┘
```

States it must render, each with its own copy:


| State                               | Screen                                             |
| ----------------------------------- | -------------------------------------------------- |
| `.preview` (auto-attach pending)    | The still, dimmed, over "Taking the terminal…"     |
| `.preview` (no auto-attach)         | The still, with a *Captured just now* label and **Attach** |
| `.preview` failed                   | "Could not read the screen." + **Attach**          |
| `.connecting`                       | Progress over the still, if there is one           |
| `.attached`                         | The surface; steal banner if `stoleSurface`        |
| `.closed(.stolen)`                  | "Your Mac took the terminal back." + **Reattach**; re-previews so the screen is not frozen at the last byte |
| `.closed(.sessionExited)`           | "This session's program exited." + **Back**        |
| `.closed(.slowClient)`              | "The connection could not keep up." + **Reattach** |
| `.closed(.detached)`                | "Detached." + **Reattach**                         |
| `.closed(.kernelError)` / `.failed` | The reason, + **Reattach**                         |
| Not running, never attached         | The session's state, + **Attach anyway**           |


The `.needsControlGrant` block gets a full screen of its own, and it must say
what to do rather than what failed:

> **This phone can't open a terminal**
> It's paired to observe. Open Latch on your Mac, find this phone under Remote
> Access, and raise it to Control.

It shows the preview behind that message. An observing phone is allowed to
*read* the pane, so the screen that explains why it cannot type can still show
what it cannot type at — which is the difference between an explanation and a
dead end.

The current dead-end screens are what this feature removes; replacing them with
a subtler dead end is not progress.

### Lifecycle

Three release points, all mandatory, because while the phone holds the socket
the desk does not have the surface:

- **Back-navigation** calls `detach()`. Leaving the screen gives the terminal
back.
- **Backgrounding** — `RootView`'s existing `scenePhase` handler gains
`model.suspendTerminals()` alongside `suspendPairedTransport()` and
`suspendConversations()`. A phone suspended with the socket open holds the
session's only surface hostage from a locked pocket. The gateway's `4408`
slow-client eviction bounds the damage; relying on being evicted is not a
design.
- **Foregrounding does not silently reattach.** `resumeAfterSuspension()`
resumes conversations because that is free. A terminal returns to
`.closed(.detached)` with a **Reattach** button, because reattaching is
another steal and the user should watch it happen.

`ChatView`'s two `ContentUnavailableView` fallbacks are rewritten in this
phase: both keep their explanation and gain an **Open terminal** button when
`surface.terminal` allows one. That is the original objective, delivered.

---



## Phase 5 — the key accessory bar

An iPhone keyboard has no Escape, no Control, no arrows, no Tab. Those are
exactly the keys a directory-trust prompt, a permission modal, and a stopped
composer are answered with. Without this bar the feature does not do the thing
it was added for, and it is the largest piece of work here that has nothing to
do with terminal emulation.

### Shape

One row. 34 pt tall. Attached to the keyboard as the terminal view's
`inputAccessoryView` (SwiftTerm's iOS view ships its own default accessory; it
is replaced, not extended). The whole row scrolls horizontally in a
`ScrollView(.horizontal, showsIndicators: false)`; **the keyboard-dismiss
button is pinned to the trailing edge** on a `.bar` background with a hairline
divider, so it never scrolls away.

Keys are ordered so the ones that matter are visible without scrolling on the
narrowest supported device:

```
esc  ctrl  tab  ←  ↓  ↑  →  ⌃C  |  ~  /  -  _  `  {  }  [  ]  <  >  $  &  *  ⌃D  ⌃Z  ⌃R  ⌃L  ⇞  ⇟  ⇱  ⇲
```

Space conservation, since that was the requirement:

- 28 pt key height, 10 pt horizontal padding, 6 pt spacing, capsule background.
- `.system(size: 13, weight: .medium, design: .monospaced)` — glyphs, never
words. `esc`, not "Escape".
- One row only. A second row is not an option; it is a third of the visible
terminal on a small phone.
- No leading pinned group. Pinning both ends would cost ~90 pt of scrollable
width to save one swipe.



### Behavior

- `ctrl` **is a sticky modifier.** Tap to arm (filled background); the next key
is control-modified; it disarms itself. Long-press to lock until tapped
again. This is the difference between `⌃C` being one key and the bar needing
a `⌃` variant of every letter.
- **Arrows repeat on hold**, after a 400 ms delay at 60 ms intervals — scrolling
a long agent output one line per tap is not usable.
- **Light haptic on key down** (`UIImpactFeedbackGenerator(style: .light)`),
matching the system keyboard.
- **The dismiss button** (`keyboard.chevron.compact.down`) resigns first
responder. The bar goes with the keyboard, which is the point: a user reading
output gets the whole screen back, and tapping the terminal brings both back.
- The bar is hidden entirely when the session is not attached — there is
nothing to type at.



### Key encoding is the emulator's job, not the bar's

This is the correctness trap in the whole phase. Arrow keys are `ESC [ A` in
normal mode and `ESC O A` when the application sets DECCKM — which every
full-screen TUI this feature exists to reach does set. `Home` and `End` shift
the same way. A bar that hardcodes CSI sequences works in a shell and sends
garbage into Claude Code's prompt.

So the bar emits *logical* keys and the surface encodes them against live
terminal state:

```swift
// Sources/LatchMobileKit/TerminalKey.swift — in the kit, so it is testable
public enum TerminalKey: Equatable, Sendable {
    case escape, tab, backTab, backspace, delete
    case up, down, left, right
    case home, end, pageUp, pageDown
    case function(Int)
    case control(Character)      // ⌃C, ⌃D, ⌃Z, ⌃R, ⌃L
    case literal(String)         // | ~ / - _ ` { } [ ] < > $ & *
}
```

`SessionTerminalSurface.encode(_:)` resolves it through the emulator's own
cursor-key mode. Pasting goes through `paste(_:)` for the same reason:
bracketed paste must be wrapped only when the mode is on.

The unambiguous encodings — `escape` → `0x1B`, `tab` → `0x09`, `control(c)` →
`c & 0x1F`, `backspace` → `0x7F`, `backTab` → `ESC [ Z`, `pageUp` → `ESC [ 5~`
— are unit-tested in the kit. The mode-dependent ones are tested through a fake
surface in both DECCKM states.

### Build it early, against a stub

The record warned that the bar and the attach affordance "should not be
discovered at the end." They should not. Build the bar in parallel with phase 3
against a stub surface — a view that renders incoming bytes as text and echoes
`encode` output — so the encoding questions surface while the renderer is still
being measured, not after it is chosen.

---



## Test plan, by layer


| Layer     | Where                       | What                                                                                                           |
| --------- | --------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Contract  | `crates/latch` tests        | shell lists `connector: null`; claude lists `"claude"`; `connector_kind` is the only matcher                   |
| Contract  | `GatewayV2Tests`            | tri-state decode: absent / null / named                                                                        |
| Routing   | new `SessionRouteTests`     | the full preference × connector × surface × running table                                                      |
| Grant     | `GatewayV2Tests`            | `observe`/`interact` phones get `surface.terminal == false`; manual link gets `true`                           |
| Socket    | new `TerminalSocketTests`   | size on attach, binary input, resize frame shape, `4409`→`.stolen`, `4410`→`.sessionExited`, no auto-reconnect |
| Preview   | `crates/latch` tests        | capture returns content without changing `#{client_flags}`; `observe` accepted where terminal refuses; scrollback ignored while `alternate_on` |
| Preview   | new `SessionPreviewTests`   | `.preview` precedes `.connecting`; attach geometry comes from the preview; `reset()` called exactly once |
| Geometry  | new `TerminalLayoutTests`   | keyboard and rotation emit no resize; a grid change emits exactly one, debounced |
| Keys      | new `TerminalKeyTests`      | fixed encodings; DECCKM-dependent encodings in both modes                                                      |
| Emulator  | fixture harness             | all eleven `fixtures/vt/` cases against `expected.json`                                                        |
| Lifecycle | `RemoteAccessEndToEndTests` | background detaches; foreground does not reattach                                                              |


The emulator harness is the one that gates a dependency decision rather than a
regression; the rest are ordinary.

---



## Sequencing


| #   | Phase                                  | Risk     | Depends on |
| --- | -------------------------------------- | -------- | ---------- |
| 0   | Contract field                         | low      | —          |
| 1   | Terminal socket client                 | low      | 0          |
| 1′  | Preview endpoint + client              | low      | —          |
| 2   | Preference + `SessionSurface.terminal` | low      | —          |
| 3   | Renderer seam + measurement            | **high** | 1          |
| 3′  | Key bar against a stub                 | medium   | 2          |
| 4   | Routing + terminal screen              | medium   | 1, 1′, 2, 3 |
| 5   | Key bar on the real surface            | low      | 3, 3′      |
| 6   | Docs                                   | low      | all        |


0, 1, 1′, and 2 are contract work with tests and no UI risk; they can land
independently and are useful on their own. 3 is the phase that can fail — it is
where a dependency is committed and where the fixtures get a vote. 3′ runs
beside it deliberately.

1′ is the one to consider pulling forward. It is the only phase that produces
something an `observe`-paired phone can use, it is the smallest change on the
Mac, and once the preview paints through the seam it doubles as the first proof
that the renderer receives real pane bytes correctly — before any socket steals
anything. It is also the only phase that regenerates the contract, so landing
it early keeps that churn away from the renderer work.

Phase 6 updates `apps/LatchMobile/README.md` — the "No terminal view" known gap
is deleted and replaced with the setting's behavior, the grant requirement, and
what the preview does and does not show —
and adds a status line to
`[DECISION_MOBILE_TERMINAL_FALLBACK.md](DECISION_MOBILE_TERMINAL_FALLBACK.md)`
recording that the auto-attach recommendation was overridden by product
direction, and why.

---



## Open questions

1. **Does the preview change the auto-attach default?** The direction for this
   objective was that a Terminal-preferred tap steals immediately, and the plan
   implements that. The argument for it was that landing on a screen and
   waiting meant landing on nothing. The preview removes that argument: the
   user can now see the prompt, at the desk's own geometry, without taking
   anything from the Mac. Auto-attach stays until this is answered, because it
   is a product call and it was made deliberately.
2. **~~Does the Terminal default apply to agent sessions on day one?~~
   Built as this plan specified: yes.** Confirming it is still worthwhile, but
   it is now a shipped behavior rather than a pending decision. This plan
   said yes: the setting is *default presentation*, not *fallback behavior*, so
   Terminal-preferred sends a Claude session to a terminal too. The alternative
   — Terminal means "terminal only where chat is impossible" — is a smaller
   change and a weaker setting. Worth confirming before phase 4, because it
   decides whether a user who never opens Settings ever sees `ChatView`.
3. **~~How faithful is `capture-pane -e` against a live agent pane?~~
   Settled in phase 1′ by measurement. No mode preamble is needed.** Every
   `fixtures/vt` case was painted into a real tmux pane at its recorded size,
   captured with `-p -J -e`, replayed into a second fresh pane of the same
   size, and captured again. Three findings:

   - **`capture-pane -e` emits SGR and nothing else.** Across all eleven cases
     the sequence inventory contains no private mode set, no `DECSTBM`, no
     `DECTCEM`, no alternate-screen switch. The one exception is OSC 8
     hyperlinks, which `codex-startup` carries and a renderer must at minimum
     not choke on.
   - **The grid does not need the modes.** A still is cells. Replaying an
     alternate-screen Claude capture into a pane sitting on the *primary*
     screen reproduced it byte-for-byte, colors included. The alternate screen
     is where the content came from, not something the still has to re-enter.
   - **The capture's trailing newline is the one real hazard, and it is not a
     mode.** tmux ends a capture with a newline after the last row; fed into a
     grid exactly `rows` tall that scrolls the whole still up one line and the
     top row is lost. Measured: the round trip differed by exactly that one
     row until the newline was stripped, and was identical after. The gateway
     now strips it, so `content` is rows joined by newlines with none at the
     end.

   What the renderer still owes the still is not a *mode* preamble but a
   *reset*: home the cursor, clear, and reset SGR before feeding, or leftover
   attributes and cursor position from a previous paint bleed into it. That is
   the `reset()` on `SessionTerminalSurface` this plan already requires for the
   attach handoff, used for one more reason. Two smaller notes for phase 3/4:
   the still should be shown with the cursor hidden, since a stray block cursor
   parked wherever the content ended is the only visible artifact of a capture;
   and autowrap must be on in the target, because `-J` deliberately emits
   joined lines longer than `cols` and relies on the emulator re-wrapping them
   exactly as the source pane did (`codex-startup` has one such line, and it
   round-tripped correctly).
4. **~~Scrollback for shells.~~ Built and documented.** The preview carries a
   bounded primary-screen tail — up to 200 lines, forced to zero while the
   alternate screen is active — which `DECISION_EXCLUSIVE_ATTACH.md` already
   reserved as a permitted courtesy. Once attached, the live surface's
   scrollback is still only what has arrived since the steal, and after a
   background/reattach cycle it is empty again. That is a property of exclusive
   attach, not of this plan, and it is now named in
   `apps/LatchMobile/README.md` under *Scrollback, honestly* rather than
   papered over.


---



## Where the build diverged from this plan

Recorded at the close of phase 6. Everything not listed here was built as
written.

**Phase 0 — the shared helper had three callers, not two.** `connector_kind`
was specified so the session list and the Conversation Hub could not disagree.
A third copy of the same match on the same harness marker already existed
inside `JsonlConnector::for_session`, so folding it in removed a duplicate
rather than adding one.

*Not done, and still outstanding:* `packages/client/src/types.ts` has its own
hand-written `SessionSummary` and `InspectReport` that now lag the wire by the
`connector` field. Not a break — the type is structural with no runtime
validation — but worth a follow-up if the TypeScript client is expected to stay
current.

**Phase 1′ — one fixture change was unavoidable.** The fake tmux's
`display-message` ignored `-F` and always printed a session row, so a caller
asking for pane geometry got a session row back. It now expands `#{...}` tokens
generically, which let `session_row` be deleted: list-sessions and
display-message go through the same expander, as they do in real tmux.

`endpoints.preview` is deliberately **not** in the schema's `required` list and
decodes through a hand-written `GatewayEndpoints.init(from:)` defaulting to
false, so an older Mac's discovery document still decodes rather than failing
wholesale over one endpoint the app can do without.

Open question 3 was settled here by measurement; its answer is written into the
open-questions section above. In short: no mode preamble is needed, a `reset()`
before the still is, the capture's trailing newline had to be stripped by the
gateway, and the still is shown with the cursor hidden.

**Phase 3′ — the seam split across the package boundary.** This plan put the
whole seam in `App/LatchMobile/SessionTerminalSurface.swift`. But
`LatchMobileKit` is a plain library that `swift test` exercises with no
simulator, and the app target is a hand-maintained xcodeproj that `swift test`
never compiles — so an `encode(_:)` living only in the app would have made the
DECCKM tests unrunnable. The encoding *table* (`TerminalKeyEncoder`) and the
*protocol* (`TerminalKeyEncoding`) are in the kit; the seam itself stays in the
app where this plan put it. The consequence is the useful one: the stub and the
SwiftTerm surface can disagree about what mode the terminal is in, but not
about what `↑` sends in that mode.

Two encodings this plan did not enumerate: `⌃?` is `0x7F` rather than the
`& 0x1F` mask, and F1–F4 are SS3 while F5 and up are CSI `~` codes with the
historical gaps at 16 and 22. The page keys deliberately do **not** follow the
arrows into DECCKM: they look like arrows on a keyboard and are not arrows on
the wire.

**Phase 3 — SwiftTerm is pinned below 1.19, and the measurement nearly produced
a false negative.** 1.19 added a build-tool plugin whose generator executable
Xcode 27 builds for the run destination rather than the host, so `xcodebuild`
fails looking for an iOS binary in the host products directory. The gate was
re-run against 1.18.0, which passes identically, and the pin is
`.upToNextMinor(from: "1.18.0")` with a comment saying what to check before
lifting it. SwiftTerm also processes `Shaders.metal` as a package resource, so
it does not build without the Metal toolchain — a genuine prerequisite, noted
in `Package.swift`.

The near-miss is worth keeping: `claude-code-trust-prompt` initially reported
both needles missing and the combining-marks row blank. Neither was the
emulator. SwiftTerm stores a never-written cell as code 0 and keeps extended
grapheme clusters in the terminal's own character map, so
`BufferLine.translateToString()` on its own reads a correctly painted screen as
NULs between the words and drops every combining mark. Any Latch code reading
text out of a SwiftTerm grid must use `characterProvider:
terminal.getCharacter(for:)` plus NUL-to-space; the rule is written into the
harness, because the failure mode is a screen that looks right and greps wrong.

The harness stayed in the tree as `Tests/TerminalEmulatorTests`, a separate
target from `LatchMobileKitTests` on purpose: a shared target would put
SwiftTerm on the kit's compile path. What admitted the dependency now governs
upgrading it.

**Phase 4 — panning had to become a mechanism.** "Attach at the desk's grid and
pan when it does not fit" only works if the declared grid is actually laid out.
A renderer sizes its grid to its own bounds, so a surface merely placed in the
viewport would have rendered ~59 columns while the pty had been told 100 — the
exact mismatch the geometry rule exists to prevent. `TerminalGeometry.pixelSize`
frames the surface at the size its grid needs and the viewport scrolls over it.
The width is biased ~5% upward because the cell advance is a ratio rather than
SwiftTerm's own metric: a spare column renders blank, a missing one would clip.

Back-navigation calls `AppModel.discardTerminal(for:)` rather than `detach()`
alone. `TerminalSession.output` is a single `AsyncStream` with one iterator;
detaching without discarding would leave a re-entered screen starting a second
iteration on the same stream, whose delivery is undefined. Backgrounding still
*keeps* the session, which is the case this plan's lifecycle rule was about, so
the closed screen can still offer Reattach.

Attaching installs the key bar but does not raise the keyboard: the first thing
anyone does on arrival is read, and a keyboard seizing half the screen unasked
would cover the prompt this feature exists to show. A tap brings both up.

**Phase 5 — two defects in the phase 4 wiring, and the device work is
unfinished.** Sticky `ctrl` only modified keys pressed on the bar: SwiftTerm has
its own `controlModifier`, which it reads first, and our bar had replaced the
accessory that sets it — so arming `ctrl` and typing `k` on the system keyboard
sent `k`. `TerminalKeyBarState` now holds the modifier outside the view and
`SwiftTermSurface` mirrors it onto SwiftTerm's own, observing the reset
notification synchronously so `locked` re-arms in the same runloop turn. The
SwiftUI `.onTapGesture` layered over the emulator's view was also dropped:
SwiftTerm's own singleTap already becomes first responder and otherwise routes
to selection and mouse reporting, and a competing recognizer over that is a
hazard rather than the mechanism.

Two of the phase's verification items were settled without a device. The key
order was measured: at 375 pt — the iOS 17 floor — the row shows
`esc ctrl tab ← ↓ ↑ →` with 39 pt to spare; `⌃C` is the first key that costs a
swipe, and stops costing one at 390 pt. And from recorded traffic, Claude Code
does not set DECCKM and neither does Codex — they set `?25`, `?1049`, mouse,
`?1004`, `?2004`, `?2026`, `?2031` and no `?1` — so both cursor-key states are
now asserted against real streams in
`Tests/TerminalEmulatorTests/KeyEncodingModeTests.swift` rather than against a
supposition.

*Still unverified, and the one honest gap in this plan's delivery:* everything
that needs a physical phone. The accessory bar's behaviour above a real soft
keyboard, the dismiss-and-return gesture, the feel of sticky `ctrl` and
hold-to-repeat, the real cell advance the geometry rule approximates, paint
throughput on `high-rate-output`, and the acceptance test — answering a Claude
Code directory-trust prompt from the phone. No device was reachable from the
build environment.
