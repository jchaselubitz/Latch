# Decision: the mobile terminal fallback, and what renders it

**Status:** implemented, with two of its answers overtaken. The build order
that followed from it is
[`PLAN_MOBILE_TERMINAL_VIEW.md`](PLAN_MOBILE_TERMINAL_VIEW.md), and it shipped.
What was built, and where this record no longer describes it:

- **The product shape held.** A session with no connector opens its terminal on
  the phone, behind a Latch-owned seam, requiring the `control` grant, with a
  key accessory bar. All of that is as recommended here.
- **Recommendation 1 was overridden on product direction.** This record argued
  for landing on the terminal screen and making the attach a separate,
  deliberate action. The direction taken instead was that a tap on a
  Terminal-preferred session steals immediately, with the user-chosen
  *Session view* setting standing as the consent. The plan records the
  mitigations that came with it — an arrival banner naming the steal, and no
  auto-attach for a session that is not running.
  A later finding softened the disagreement rather than settling it: the
  no-steal `GET /v2/sessions/{id}/preview` route means a phone that has not
  attached is no longer looking at nothing, which was the argument that made
  auto-attach necessary. The default stayed as directed; the question is
  recorded as open question 1 in the plan.
- **The emulator was chosen by measurement, not by the shape argument.**
  SwiftTerm is what ships, but it was admitted by replaying all eleven
  `fixtures/vt` cases through it — every `contains` string, `alternate_screen`,
  cursor visibility, scroll region, row text, cursor position, mode flag and
  title, including wide/combining/emoji rows and the mid-stream resizes at
  their recorded byte offsets — before the dependency was committed anywhere.
  That was the `DECISION_EMULATOR.md` precedent this record invoked, and it was
  the gate that decided, not the reasoning in *The candidates* below. The
  harness stayed in the tree as `Tests/TerminalEmulatorTests`, so what admitted
  the dependency now governs upgrading it. SwiftTerm is pinned below 1.19 for a
  build-tooling reason recorded in `apps/LatchMobile/Package.swift`.
- **One thing this record did not anticipate:** an observing device gets
  something after all. `GET /v2/sessions/{id}/preview` is a one-shot pane
  capture at the `observe` grant — a query, not an attach — so a phone that may
  never take the surface can still see it.

**Question:** when a session has no agent connector, should tapping it on the
phone open that session in a terminal instead of Latch's conversation
interface — and what should draw that terminal?
**Answer:** yes to the product shape, reached deliberately rather than by
silently swapping one screen for another. The emulator is an open choice, and
the candidate that prompted the question — libghostty — is the one candidate
that cannot take the job today, for a structural reason rather than a matter
of effort.
**Scope:** `apps/LatchMobile`, and one additive field on
`schemas/remote-access/v2` that the routing needs.

This records coo:848. It does not reopen
[`DECISION_EXCLUSIVE_ATTACH.md`](DECISION_EXCLUSIVE_ATTACH.md) — it is the
first design that consumes it from the phone, and most of what follows is
that contract's consequences arriving on a touchscreen.

---

## What the phone does today

Tapping a session pushes `ChatView`, and `ChatView` has two ways to end in a
screen that tells the user to go and use a different machine:

| Condition | Where | What the user sees |
| --- | --- | --- |
| Discovery did not advertise the `conversation` endpoint | `ChatView.body`, via `AppModel.surface.chat` | "Conversation unavailable … Use terminal attach on the Mac for this session." |
| The socket opened and the state's `connector` is `null` | `ChatView.conversation(_:)` | "Conversation unsupported … Terminal attach remains available for recovery." |

Both are `ContentUnavailableView`s with a terminal glyph, and neither has a
terminal behind it. The second one is the case this objective is about: the
session is live, reachable, and authenticated, and the phone's answer is that
the user should walk to their desk.

That is not a rare corner. `connector_for_session` in
`crates/latch/src/conversation/connectors/jsonl.rs` builds a real connector
only when the session's persisted `harness` marker reads `claude` or `codex`.
Everything else — every plain `latch` shell, every `latch run -- <anything
else>` — gets a `PendingConnector`, whose state reports `connector: null`
permanently. A shell session is not a degraded agent session; it is a session
whose entire interface is a terminal, and the phone currently offers no way to
reach it.

---

## The gateway half already exists

`WS /v2/sessions/{id}/terminal` is implemented in
`crates/latch/src/cli/serve/terminal.rs`, advertised in discovery
(`endpoints.terminal: true`), and generated into the phone's contract
(`GatewayEndpointsName.terminal`, `TerminalCloseReason`). It relays raw bytes
both ways against one spawned `latch attach`, and the phone never opens it.
`apps/LatchMobile/README.md` says so under Known gaps: "No terminal view.
Discovery reports the endpoint; the app does not use it yet."

Nothing about the transport needs work either. WebSockets already run over
both routes — a manual `latch serve` link directly, and a paired link through
the loopback listener `NoiseTunnelGatewayTransport` bridges into the Noise
tunnel, which is how `ConversationSocket` reaches the Hub today.
`LatchGateway.openConversation` is the exact template an `openTerminal` would
follow: build the path, carry the token, hand back a `URLSessionWebSocketTask`.

So the missing piece is genuinely and only the renderer, and the requirement
on it is narrow: consume raw xterm bytes, emit raw bytes back, resize, and
stay smooth while an agent TUI repaints a full screen. Nothing in the wire
contract prefers one emulator over another. That framing is what makes "add
libghostty" the obvious-sounding move, and also what makes it replaceable.

---

## libghostty: two different products, and only one of them ships to iOS

libghostty is the candidate that prompted this record, so it is examined
first — not because the feature requires it. It is also the candidate that
turns out to be unavailable, which is worth establishing before comparing the
ones that are.

Verified against `include/ghostty.h` and the upstream API documentation on
24 August 2026, and corroborated by an independent iOS proof-of-concept that
hit the same wall.

### libghostty (the embedding API, shipped as `GhosttyKit.xcframework`)

This is the one that is a terminal: Metal rendering, CoreText fonts, input
handling, selection, the whole surface. It builds `ios-arm64` and
`ios-arm64-simulator` slices, and `ghostty_surface_config_s` has a
`GHOSTTY_PLATFORM_IOS` tag that takes a `UIView`. On the face of it, exactly
what this feature wants.

It cannot be used here. The surface owns its own I/O, and `termio.Backend` has
exactly one variant — `exec` — which spawns a child process behind a PTY. The
config struct's only I/O-shaped fields are `command`, `working_directory`,
`env_vars`, and `initial_input`: all of them describe a process to start
locally. There is no entry point that hands a surface bytes from somewhere
else, which is precisely what a remote Latch session is. iOS does not permit
spawning that child in the first place, so the one backend the library has is
the one the platform forbids.

Feeding a libghostty surface from a WebSocket therefore needs an upstream
change — a second termio backend — not integration work on our side. Upstream
also states the embedding API is used only by the macOS app, is not stabilized
for general-purpose embedding, and may change significantly between releases.

### libghostty-vt (shipped as `ghostty-vt.xcframework`)

This is the piece being extracted for general use: VT parsing, terminal state,
and render-state updates, with a documented C API (`ghostty_terminal_new`,
`ghostty_terminal_vt_write`, `ghostty_terminal_resize`, the formatter calls).
It builds the same three Apple slices and is released on upstream's tip
channel. Feeding it our stream is the easy part — `ghostty_terminal_vt_write`
takes exactly the bytes the terminal socket delivers.

**It contains no renderer.** It hands out render state and expects the
embedder to draw. A third-party iOS proof-of-concept confirmed the core
behaves correctly on-device by asserting a cell buffer against a canned PTY
stream; a cell buffer is what you get, and a terminal is what you still have
to write.

### What choosing libghostty-vt would actually mean owning

| Piece | libghostty-vt gives us | We would write |
| --- | --- | --- |
| Escape-sequence parsing, grid, modes, wide characters | yes | — |
| Drawing cells to the screen (Metal or CoreText) | render state only | all of it |
| Font selection, metrics, ligatures, fallback | — | all of it |
| Scrollback presentation and touch scrolling | grid access | all of it |
| Selection, copy, the iOS edit menu | — | all of it |
| Soft-keyboard input, IME, key encoding | — | all of it |
| VoiceOver and Dynamic Type | — | all of it |

Also inherited: a static library needing `-force_load` inside a dynamic
framework, a `-lc++` dependency (utfcpp), and a pre-1.0 API on a tip channel.

There is a community Swift package (`libghostty-spm`) that wraps the *full*
embedding library with iOS fixes and its own sandboxed-shell layer, which
would sidestep the renderer work. It is a third-party fork of an API upstream
calls unstable, and this repository already carries a Noise handshake, a
pinned key, and a written threat model
([`REMOTE_ACCESS_THREAT_MODEL.md`](REMOTE_ACCESS_THREAT_MODEL.md)); a
downstream binary rebuild of a terminal core is not a dependency to take on
someone else's release cadence. Worth re-examining if it stabilizes, not worth
adopting to save the renderer.

---

## The candidates, on this repository's own criteria

Ghostty is not a requirement. The bar is a clean, performant terminal surface,
and four things could clear it.

[`DECISION_EMULATOR.md`](DECISION_EMULATOR.md) set the standard for this kind
of choice: measure fidelity, maintenance, and weight rather than compare
documentation. That measurement has not been run — no fixture has been fed to
any candidate on a device — so what follows is a shape recommendation, and the
measurement is named below as the thing that settles it.

| | libghostty (apprt) | libghostty-vt | SwiftTerm | xterm.js in a web view |
| --- | --- | --- | --- | --- |
| Renders on iOS | yes | no, state only | yes, UIKit `TerminalView` | yes, in `WKWebView` |
| Accepts an external byte stream | **no**, exec-only backend | yes | yes, `feed` + delegate | yes |
| API stability | upstream calls it unstable | pre-1.0, tip channel | tagged releases, SemVer | stable, `@xterm/xterm` 5.5 |
| Input, selection, scrolling, VoiceOver | included | none | included | web behaviors, not iOS ones |
| Code we would already own | none | none | none | `packages/terminal-react` |

**SwiftTerm** is a maintained Swift VT100/xterm emulator whose iOS
`TerminalView` is documented as being for exactly this case — the platform has
no local processes, so the expected use is wiring the view to a remote host
through a delegate. It has active 2026 releases, a GPU backend, iOS VoiceOver
and selection work, and kitty-protocol input.

**xterm.js in a `WKWebView`** is the option worth naming because it looks
almost free: `packages/terminal-react` already wraps `@xterm/xterm` against
this exact wire contract, so the emulator, the reconnect logic, and the close
codes are code that exists and is tested. What it costs is the thing the
requirement asks for. Every byte crosses a JavaScript bridge before it reaches
a DOM renderer, on the surface whose whole job is a full-screen agent TUI
repainting at speed; and the touch, selection, scroll, and keyboard behaviors
would be the web's rather than the platform's, on a screen where those are
most of the experience. Reasonable as a spike to prove the routing end to end
before any emulator is chosen. Not the shipped answer.

`planning/PROJECT_ARCHITECTURE.md` anticipates this choice in the abstract:
the Swift frontend "may build on a maintained native terminal-emulation core",
and those dependencies "should be hidden behind Latch's own session-view API."

That last clause matters more than which candidate wins. The emulator is an
implementation detail behind a Latch-owned view, and picking one should not be
a one-way door.

---

## What holds regardless of which emulator renders it

These are the parts that decide whether the feature is good, and none of them
are affected by the choice above. Three of the four are consequences of
exclusive attach, and they are why "the user taps on the session, and it
simply opens" needs one qualification.

### Opening a terminal steals the desk, and steal is destructive

A terminal connection is the session's single exclusive surface. Connecting
detaches whoever holds it — with a reason, `4409 stolen` — resizes the pane to
the phone's geometry, and `SIGWINCH`es the child. If iTerm is attached at the
desk, the phone takes it away and reflows the agent's TUI to a phone-sized
grid.

That is the right behavior when the user meant it. It is a bad surprise when
the tap was exploratory, and worse because the session list gives no hint that
a desk surface is live. A tap that lands on a chat screen is free; a tap that
lands on a terminal is not.

So: route to the terminal on tap, but land on the surface, not in the steal.
The screen opens showing what the session is and offers to attach; the socket
is opened by that action. This preserves the "one tap gets you the terminal"
feeling while keeping the destructive step something the user aimed at. The
gateway agrees with this shape already — its handshake deadline exists so that
an unfinished connection "must expire rather than sit holding a steal in
reserve."

### It requires the `control` grant, which a paired phone may not have

The terminal socket has no observe mode by design: "observing without
controlling is Conversation Hub's job." The route requires
`x-latch-device-grant: control`. A phone paired at `observe` or `interact`
cannot open one at all.

The phone already models this — `DevicePermission`, and
`SessionSurface.restricted(to:)` — but only for the composer. The terminal
entry point needs the same treatment, and the failure has to be stated on the
screen ("this phone is paired to observe; raise it to control on the Mac")
rather than surfacing as a socket that closes. The current dead-end screens
are the thing being removed; replacing them with a subtler dead end is not
progress.

### Backgrounding, on a surface that is exclusive

`AppModel` already suspends the paired transport and the conversation sockets
before suspension and rediscovers on foreground, because iOS reclaims
connections without delivering a close. A terminal socket needs the same
lifecycle, with a sharper consequence: while the phone holds the surface, the
desk does not have it. A phone that is suspended with the socket open is
holding the session's only surface hostage from a locked pocket.

The gateway's slow-client eviction (`4408`, a five-second write deadline)
bounds the damage, but the app should release the surface on suspension
deliberately rather than rely on being evicted, and reattaching on foreground
is another steal — which the user should see happen rather than have it
happen silently.

### The soft keyboard cannot answer a TUI

An iPhone keyboard has no Escape, no Control, no arrows, no Tab. The prompts
this feature exists to reach — directory trust, a permission modal, a stopped
composer — are answered with exactly those keys. A key accessory bar is not
polish here; without it the feature does not do the thing it was added for.
This is the largest piece of work in the whole feature that has nothing to do
with terminal emulation.

---

## Routing the tap needs one additive contract field

To send a tap to the right screen, the list has to know whether a session has
a connector, and today it does not: `SessionSummary` carries `id`, `name`,
`title`, `state`, `cwd`, `command_label`, and timings. The connector answer
arrives only in conversation state, over a socket that must be opened first.

Probing to decide is not an option: the two sockets are not interchangeable,
and opening the terminal one to find out would steal the desk before the user
has chosen anything.

Add the answer to `GET /v2/sessions` instead. It is cheap on the Mac —
`connector_for_session` reads the persisted harness marker and nothing else,
which is a local metadata read the list already performs. The field is purely
additive, so an older phone ignores it and an older Mac omits it, which the
client already handles as "unknown, keep the current behavior."

`command_label` looks like it could serve as a heuristic and should not be
used as one. It describes what was launched, not whether a connector bound to
it.

Note the distinction the field has to preserve: `connector: null` is permanent
for a shell and transient for an agent session whose transcript has not bound
yet. Routing a starting Claude session permanently to a terminal because it
was tapped two seconds early would be a worse bug than the dead end it
replaces.

---

## Recommendation

1. **Take the product idea.** A session with no connector should open in a
   terminal. The current screens tell a user with a live, reachable session to
   go to another machine, and the endpoint that would serve them has been
   implemented and advertised for some time.
2. **Put a Latch-owned `SessionTerminalView` in front of the emulator**, as
   `PROJECT_ARCHITECTURE.md` already requires. Its surface is small — feed
   bytes, report size, emit input, report close reason — because the wire
   contract is raw bytes in both directions.
3. **Do not adopt the libghostty embedding API for this**, and do not treat
   that as a loss. It is not integration work; it is an upstream feature
   request for a non-exec termio backend, on an API upstream calls unstable,
   blocking a phone feature. Nothing in the product needs Ghostty
   specifically — the requirement is a clean, performant terminal.
4. **Build the first implementation on SwiftTerm**, whose iOS view is designed
   for a remote byte source and which brings input, selection, scrolling, and
   VoiceOver with it. Behind the seam in (2) it is replaceable.
5. **Keep libghostty-vt as the named revisit**, on the terms in the next
   section. It is the better emulation core and upstream is extracting it for
   exactly this purpose; what it does not have is the half of the work that is
   not emulation.
6. **Sequence the work so the risky part is last.** The tap-routing field and
   the terminal socket client are contract work with tests. The renderer is
   the middle. The key accessory bar and the attach/steal affordance are what
   make it usable, and they should not be discovered at the end.

Before step 4 is committed, run the measurement `DECISION_EMULATOR.md` set the
precedent for: feed every stream in [`fixtures/vt/`](../fixtures/vt) through
both candidates on a device and compare against the recorded expectations.
The fixtures are recorded Claude Code and Codex sessions, which is exactly the
traffic this surface will carry. A shape argument should not outrank that
measurement if it disagrees.

---

## What would change this

- **Upstream libghostty gains a termio backend that accepts external bytes.**
  This is the single change that makes the embedding API the obvious answer,
  because it would arrive with the renderer, the input pipeline, and the
  selection model already built. Watch for it.
- **libghostty-vt stabilizes and a rendering layer for it becomes something we
  can consume rather than write.** The core is not the reason to hesitate; the
  seven rows of the ownership table are.
- **The `fixtures/vt/` measurement contradicts the shape argument.** Then the
  measurement wins, exactly as it did in `DECISION_EMULATOR.md`.
- **A product decision to give the phone an observing terminal.** Everything
  above assumes the terminal stays a control surface. A read-only live
  terminal is explicitly refused today by
  [`ARCHITECTURE_RULES.md`](ARCHITECTURE_RULES.md), and reversing that changes
  the steal problem, the grant problem, and the backgrounding problem all at
  once.
