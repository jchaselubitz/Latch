# Latch Implementation Plan

## Purpose

This plan delivers Latch in milestones that are each personally useful the day they
land. It is written to be implementable: the on-disk layout, wire protocol, and
milestone exit criteria are concrete enough to start from.

The target customer is specific, and every sequencing decision below follows from it:

> People who use agents in the terminal, but want to chat with them from their phone
> (or embedded in a web app) without giving up the first-class terminal surface.

Two consequences shape the whole plan. First, the terminal experience these users
already have must not degrade — that is the price of entry, not a feature. Second,
the differentiating capability is remote interaction, so the plan reaches a phone as
early as physically possible.

The strategy for getting there early is to **borrow infrastructure rather than build
it**, and replace each borrowed piece only once its value is proven:

| Need | Borrowed first | Built later |
| --- | --- | --- |
| Zero-friction adoption | iTerm profile runs `latch` | — (it keeps working) |
| Remote reachability | SSH + Termius over Tailscale | Latch cloud control plane (M4) |
| "Your agent needs you" alerts | Overlord's existing agent hooks | Latch push notifications (M4) |
| Structured agent interaction | Overlord connector events | Latch extension SDK (M5) |

Architectural boundaries come from [`PROJECT_ARCHITECTURE.md`](./PROJECT_ARCHITECTURE.md).
The Overlord contract comes from [`OVERLORD_INTEGRATION.md`](./OVERLORD_INTEGRATION.md).
[`ARCHITECTURE_REVIEW.md`](./ARCHITECTURE_REVIEW.md) records the analysis that produced
this sequence.

---

## Decisions encoded in this plan

These were open questions. All are now settled, so implementation can start without
further design work.

### D1 — One Rust binary for the local plane

**Decided.** `latch` is a single Rust executable containing both the CLI and the
session worker (worker mode via an internal subcommand). There is no Node.js in the
local plane and no separate worker binary to install.

The deciding factor is the terminal-profile adoption path in M1: **every terminal
window you open pays CLI startup cost.** Rust starts in single-digit milliseconds; Node
is roughly 50–100 ms plus ~40 MB RSS. For a customer whose stated requirement is that
their terminal experience is preserved, a perceptible hitch on every new window is
close to disqualifying. Attach latency over a phone SSH connection compounds it.

TypeScript remains the language of the web and cloud plane (M3 onward): the embeddable
client, the Overlord integration, and the cloud control plane. The split is
**Rust owns the process plane; TypeScript owns the presentation and network plane.**

Two consequences worth planning around. Distribution is one static binary, which makes
signing and notarization in M6 straightforward. And the protocol boundary still keeps
the CLI independently replaceable, so this decision does not propagate into the worker
or into any client.

### D2 — No daemon, no SQLite, no launchd in the MVP

Sessions are discovered through the filesystem. Each worker owns a directory
containing its socket and metadata; liveness is "does the socket accept a connection."
There is no resident registry process, no database, and no service manager until
something genuinely requires one.

A resident process first earns its place in M4 (cloud presence and rendezvous), and
possibly in M3 if the Overlord Desktop host cannot open a Unix socket (see M3).

*Revisit if:* sessions need to survive user logout, or auto-start on login becomes a
daily friction. That is a small, separable addition — a launchd agent that supervises
nothing but restarts on boot.

### D3 — The worker maintains a headless terminal emulator

The worker parses PTY output into a live screen model and can serialize that screen
into a self-contained ANSI sequence that reconstructs it exactly. This is the
substrate, not an optimization — see [Screen model](#the-screen-model).

### D4 — Session size follows the controller, and reverts

The session has one authoritative size, set by whichever attachment currently holds
input control. When a controller detaches, the size reverts to what was in effect
before it took control. See [Resize authority](#resize-authority).

### D5 — Exclusive write, not timed leases

At most one attachment holds input control. Control is released by socket close, not
by timer expiry. Timed leases are deferred to M4, where remote connections can hang
without closing.

---

## The screen model

This is the single most important implementation decision in the kernel, so it is
specified before the milestones.

**A byte-replay buffer cannot restore an alternate-screen application.** Replaying the
tail of an output stream to a fresh terminal can begin mid-escape-sequence, paints only
part of a full-screen redraw, and never restores modes set before the replay window —
alternate screen, bracketed paste, cursor visibility, scroll region, current attributes.
Claude Code and Codex use all of these. Since every iOS app-backgrounding is a detach
and reattach, a session on a phone reattaches constantly; if reattach is approximate,
the product does not work on the platform it exists for.

**Implementation:** the worker feeds PTY output through a VT parser maintaining a full
screen model, and can emit a byte sequence that reconstructs the current screen from a
reset terminal. `vt100` was chosen over `wezterm-term` in M1 by evaluation against
`fixtures/vt/` — see [`../docs/DECISION_EMULATOR.md`](../docs/DECISION_EMULATOR.md). The
choice sits behind an internal trait so it can be swapped.

The screen model is load-bearing for four separate requirements:

| Requirement | How the screen model serves it |
| --- | --- |
| Correct reattach | Serialize current screen; no byte-tail guessing. |
| Slow-client backpressure | Drop a stalled client's queue and re-send a snapshot instead of buffering unboundedly. **The PTY read must never block on a client** — that would freeze the child process for everyone. |
| Scrollback bounding | A bounded line ring in a structured model, not an unbounded byte log. |
| Conversation-view fallback (M3) | "Is the alternate screen active?" is a property read, not a heuristic over raw bytes. |

---

## On-disk layout

```text
~/.latch/
  config.toml                  # non-secret preferences
  sessions/
    ses_01J.../
      meta.json                # written once at spawn (temp file + rename)
      control.sock             # worker socket; connectable == alive
      journal                  # bounded output journal
      exit.json                # written by the worker at exit
```

`~/.latch` and every session directory are mode `0700`. The socket is `0600`.

```jsonc
// meta.json
{
  "format_version": 1,
  "id": "ses_01J...",
  "name": "latch-api",             // auto-derived unless --name given
  "title": "Authentication refactor",
  "cwd": "/Users/jake/Development/...",
  "command_label": "codex",        // redacted; never full argv
  "created_at": "2026-08-05T15:00:00Z",
  "initial_size": { "cols": 200, "rows": 50 },
  "source": { "kind": "cli", "external_run_id": null }
}
```

```jsonc
// exit.json
{ "code": 0, "signal": null, "exited_at": "2026-08-05T17:22:31Z" }
```

**Session state is derived, never stored.** This removes the entire class of
lying-registry bugs:

```text
socket accepts a connection      -> ask the worker (creating | running | stopping)
exit.json present, socket gone   -> exited
neither                          -> lost
```

Secrets, full environment blocks, and raw argv never touch `meta.json`. Launch material
arrives over stdin or the socket and lives only in worker memory.

---

## Wire protocol

One duplex byte stream. Unix socket locally; the same frame vocabulary rides a
WebSocket in M3/M4, so the codec must be transport-agnostic.

### Framing

```text
u8   type
u32  length (big-endian)
[u8] payload
```

| Type | Name | Payload |
| --- | --- | --- |
| `0x01` | `terminal.output` | raw bytes (hot path, no structured decode) |
| `0x02` | `terminal.input` | raw bytes (hot path) |
| `0x10` | `control` | MessagePack object with a `t` discriminator |

Keeping output and input as bare binary types keeps the hot path allocation-free.

### Control messages

Eight, deliberately. Every message multiplies across the Rust and TypeScript fixture
suites (and Swift later, if it happens).

```text
attach          { protocol, mode: watch|control, steal, client: {kind,name}, size }
attached        { protocol, session, controller, attachments }
resize          { cols, rows, pin? }                    // pin is management-only
control.request { steal }
control.state   { controller_id, controller_label }        // broadcast
session.update  { state?, attachments?, title?, force? }    // presence + state, merged
session.exited  { code, signal, at }
error           { code, message }
```

**The snapshot is not a message.** After `attached`, the worker sends the serialized
screen as ordinary `terminal.output` frames, then continues with live output. A
reconnecting client therefore needs no special replay round trip and no separate code
path — reconnect is just attach.

Version negotiation rides `attach`; an unsupported `protocol` gets `error` and a close,
never a guess. Because clients connect to a per-session socket, there is no session
to negotiate and no separate hello.

---

## Resize authority

Termius on a phone is roughly 40 columns; iTerm is roughly 200. This is the default
configuration for this customer, not an edge case, and it arrives at M2.

**Policy:** the session's size is the current controller's size. When a controller
detaches, the size reverts to what was in effect before that controller took over.
Watchers never resize the session.

In practice: the phone takes control, the session reflows to 40 columns and Claude Code
is usable on it; the phone disconnects, the session snaps back to 200 and the desk
session is intact. `latch resize <session> --cols N --rows M` overrides manually, and
`--pin` freezes the size against controller changes.

---

## Repository structure

```text
Latch/
  crates/
    latch/                   # the single binary: CLI + worker modes
    latch-protocol/          # framing, control messages, codec
    latch-term/              # screen model + snapshot serialization
  packages/                  # M3 onward
    protocol/                # TypeScript codec (fixture-verified against Rust)
    session-client/          # TypeScript attachment client
    terminal-react/          # xterm.js behind a Latch renderer API
  fixtures/                  # language-neutral protocol + VT fixtures
  planning/
  docs/
```

Dependency direction: `latch-protocol` and `latch-term` are leaves. Nothing in
`crates/` may import Overlord types. `packages/protocol` and `crates/latch-protocol` are
independent implementations kept honest by `fixtures/`, not by code sharing.

Cloud and relay services (M4) are separate deployables added at that milestone. Swift
packages remain deliberately absent.

---

# M1 — Kernel, with the iTerm experience preserved

**Goal:** every terminal window is already a persistent session, and nothing about
using iTerm feels different.

### Work

**Worker**

1. Spawn detached (`setsid`), owning the PTY and the child's process group.
2. Screen model: `vt100` behind the `Screen` trait, with snapshot serialization and a
   bounded scrollback ring. Done in M1a.
3. Bounded on-disk journal with a configurable cap (default 10 MB per session).
4. Control socket: framing codec, peer-credential check, `0700`/`0600` modes.
5. Attachment registry: watch/control modes, exclusive write, `steal`, broadcast
   `control.state`.
6. Per-client bounded queue; on overflow drop the queue and resync via snapshot. **The
   PTY read never blocks on a client.**
7. Resize authority and revert-on-detach (D4).
8. Exit detection, `exit.json`, graceful then forced stop of its own child process
   group. No stored PID is ever consulted for a kill.

**CLI**

9. `latch` / `latch shell` / `latch run -- <cmd>` — create and attach.
10. `latch attach` — raw mode, input and resize forwarding, terminal restored on exit,
    including on panic and signal.
11. `latch list` / `inspect` / `stop` / `rename` / `prune` / `doctor` / `config` /
    `resize` / `capabilities`, all with `--json`.
12. Session directory creation, id generation, `meta.json` via temp-file-plus-rename.
13. Launch manifest over stdin (`latch create --manifest-file -`). Built now because
    M3's Overlord provider uses the same path, and because it keeps secrets out of argv
    from day one.

**Adoption and hygiene**

14. Export `LATCH_SESSION_ID`; refuse to nest. **Mandatory at M1** — with an iTerm
    profile running `latch`, nesting is a daily occurrence, not an edge case.
15. Auto-name sessions from cwd and command; `latch list` sorts by last activity and
    shows idle time. With every window a session, the list is the primary navigation
    surface rather than an occasional command.
16. Sanitize all externally supplied display metadata to printable characters **at
    ingest**, not at render. Sanitizing at render means every future call site is a new
    chance to forget.
17. Document the iTerm profile setup: point the profile's command at `latch`. This is
    configuration, not code, and it is what makes adoption free.

### Exit criteria

- Every new iTerm window is a persistent session, and opening one feels instant.
- Closing a window leaves the process running; `exit` ends it.
- Reattaching to a running Claude Code session reproduces the screen **identically** to
  what a continuously attached client shows — including alternate screen, colors,
  cursor position, and scroll region.
- A second terminal attaches to the same session; exactly one holds input control, and
  transfers are visible to both.
- `latch stop` terminates only the selected verified session's process group.
- A hung or killed client never affects the child process.
- `latch list` is useful with 30 sessions open.
- No daemon, no database, no service manager, no Node.js.

### Tests

Protocol fixture suite (encoded frames + expected decodes). VT fidelity against recorded
byte streams from Claude Code and Codex startup, alternate screen, resize, Unicode and
wide characters, bracketed paste, cursor rewriting, sustained high output. Snapshot
round-trip: feed a stream, snapshot, replay into a fresh emulator, assert identical
screens. Abrupt client disconnect. Concurrent attachment and control steal. Backpressure
under a deliberately stalled client. Signal and process-group behavior. Filesystem
permissions. Malformed and oversized frame fuzzing.

---

# M2 — The phone, over SSH

**Goal:** interact with a running agent session from a phone, today, with no Latch
networking code.

SSH into the Mac (Tailscale or equivalent) and run `latch attach` in Termius. This
borrows SSH's entire reachability, authentication, and encryption stack, and validates
demand — do you actually reach for your agent from your phone? — before anything is
built to serve it.

It is also the harshest realistic test of the kernel: cell networks, app backgrounding,
connections dropping on network change, and a 40-column screen.

### Work

1. Attach resilience when the transport dies mid-stream: detect, and make reattach a
   single command (or automatic on `latch attach --retry`).
2. Verify the resize revert loop (D4) end-to-end from a real phone.
3. Scrollback on attach: decide and implement how much history accompanies the
   snapshot, and add paging if the snapshot alone proves insufficient. Decided against
   measurement — 200 lines under a 32 KiB ceiling, served from the screen model's ring
   rather than the journal, none on a resync, and no paging. See
   [`../docs/DECISION_SCROLLBACK.md`](../docs/DECISION_SCROLLBACK.md).
4. Verify backpressure behavior on a genuinely slow link.
5. `latch attach --watch` for peeking without taking control from your desk session.
6. Document the SSH/Tailscale setup. See
   [`../docs/SSH_SETUP.md`](../docs/SSH_SETUP.md).

### Exit criteria

Each is verified twice, and both halves are needed. The simulated half runs the real
binary against a cuttable socket proxy standing in for the SSH tunnel
(`crates/latch/tests/remote_attach.rs`); the field half is a person with a phone, on
cell, away from their network. The simulation cannot produce cell latency, iOS
backgrounding, or the answer to whether anyone reaches for this — so a green suite is
necessary and is not sufficient. Field results and the verdict go in
[`../docs/M2_FIELD_REPORT.md`](../docs/M2_FIELD_REPORT.md).

- You can answer a Claude Code permission prompt from your phone, away from your
  network, and the agent continues.
- Backgrounding and reopening Termius restores the screen correctly, every time. A
  single wrong restore is an M1 snapshot defect: it is fixed in `latch-term` with a new
  fixture, never worked around in the client.
- The desk session's geometry is intact after the phone disconnects.
- A dropped connection loses no session state.

**Explicitly not shippable.** This path needs the Mac SSH-reachable, so it is a dogfood
vehicle, not a product. Its job is to prove the demand and harden the kernel before M4
builds the real transport.

---

# M3 — Overlord Desktop chat

**Goal:** the first test of the actual hypothesis — that agent interaction reads better
as a conversation than as a terminal.

Overlord Desktop is the embedding customer; there is no separate web demo app.

### Work

**Client SDK**

1. TypeScript protocol codec, verified against the Rust implementation by `fixtures/`.
2. TypeScript attachment client with reconnect (which, per the protocol, is just
   attach). The client takes a transport rather than opening a connection itself: a
   Unix socket in Electron's main process now, a WebSocket in M4. Frames cross to the
   renderer over Electron IPC, so the renderer never needs socket access.
3. xterm.js behind a stable Latch renderer API — the dependency must not be the public
   surface.
4. Terminal view: exact VT rendering, connection state, watch/control indication,
   selection, scrollback search.
5. Conversation view: message composer, grouping of submitted input with subsequent
   output, collapsible output, one-action switch to terminal view.
6. Automatic fallback to terminal presentation when the screen model reports alternate
   screen or heavy cursor addressing.

**Overlord integration** (`OVERLORD_INTEGRATION.md` Stage 2)

7. `latch` execution provider: `capabilities`, `create` via protected manifest,
   `inspect`, `stop`.
8. Provider session mapping on the execution request; the Latch session ID is an
   identifier, never authorization.
9. Settings: execution provider separate from preferred viewer, per user and target.
10. Desktop UI: embedded chat view, open-in-iTerm, detach, end session — with terminal
    state displayed separately from agent state.
11. **Notifications ride Overlord's existing agent hooks.** Overlord already knows when
    an agent needs input; no Latch notification infrastructure is built here.

### Transport — resolved

Overlord Desktop is an Electron application
([cooperativ-labs/Overlord](https://github.com/cooperativ-labs/Overlord)), so it can
open a Unix socket from its main process. **It attaches to the worker socket directly,
and no gateway or daemon is needed.** The renderer reaches the session over Electron
IPC to the main process, which holds the socket.

This means M3 introduces no resident process, and the product still has none until M4.
A loopback WebSocket gateway is required only for a genuine browser context — a hosted
web client with no Node process available — which is not on the roadmap before the
cloud control plane makes it moot.

### Exit criteria

- Overlord Desktop attaches to a session concurrently with iTerm.
- Control transfers are visible; two uncontrolled writers are impossible.
- Switching presentation does not reconnect or restart the process.
- Arbitrary terminal programs remain usable in terminal view.
- Overlord's normal connector attach/update/deliver lifecycle is unchanged.
- Completing an objective does not end the Latch session.
- Latch still ships no resident process. M3 adds a client, not a service.

### The hypothesis under test

Because the terminal stays first-class, conversation view does not need to project
arbitrary terminal output faithfully. It needs to cover what you would plausibly do from
a phone: read status, approve, answer, send a follow-up. Nobody navigates a TUI on a
train.

**Kill criterion:** if, after sustained use, you consistently switch to terminal view to
get things done, the honest conclusion is that Latch is a persistence-and-remote-access
product. That is still a good product, and M4 still matters. Decide this before
investing in M5.

---

# M4 — Reach

**Goal:** the phone experience without SSH, and the agent able to reach you.

Push notification and remote connectivity ship together because from the user's side
they are one feature. Without notification, remote chat is a polling chore nobody does
twice.

### Work

1. Separately deployable cloud control plane with its own API, migrations, credentials,
   and health checks.
2. Account and device registration with device-bound keys; revocation and rotation.
3. Bounded session-directory heartbeats — no terminal content, ever.
4. Short-lived, session-scoped watch/control grants.
5. Direct-connection rendezvous; WebSocket relay fallback as a separate deployable.
6. End-to-end encryption between client and device before frames enter the relay.
7. Push notification registration and delivery, replacing the borrowed Overlord hooks.
8. Timed control leases (D5) — remote connections can hang without closing, which is the
   case that justifies them.
9. Overlord mobile chat view, using scoped Latch grants.
10. Audit events, retention, and deletion policy for directory metadata.

### Boundary that must hold

Overlord's backend never proxies terminal bytes, and PTY bytes never travel through
`ovld agent-session`. Overlord requests a scoped grant and hands it to the embedded
Latch client, which connects directly or through the Latch relay. This is why Overlord
mobile chat is here rather than in M3.

### Exit criteria

- A phone discovers and attaches to a session without knowing the host address, and
  without SSH.
- A permission prompt produces a notification, and answering it from the phone unblocks
  the agent.
- Relay operators cannot read terminal frames.
- Revoking a device closes active remote attachments and prevents new ones.
- Local attachment continues working through a full cloud outage.

---

# M5 — Widgets and the extension SDK

**Deliberately last among feature work.** Widgets are held until the functionality
beneath them is proven: M2 proves remote access matters, M3 proves whether chat is the
right surface, and only then is it clear which widgets are worth building.

The prototype path is Overlord connector events, which already exist and are already
fixture-proven. The Latch-native extension SDK is generalized from what that teaches,
rather than designed speculatively.

### Work

1. Extension manifests, capability declarations, versioning, runtime discovery.
2. Structured sideband event envelope, ordered against terminal frames.
3. Action request IDs, revisions, expiry, first-writer-wins resolution.
4. Extension slots in the TypeScript client.
5. First adapter, on the harness with the strongest structured surface.
6. Widgets: permission request, structured question, tool activity, turn completion.
7. Terminal fallback shown beside every interactive widget.
8. Capability negotiation so no client renders an unsupported control.

### Exit criteria

- Removing the adapter leaves a fully functioning terminal session.
- A stale widget cannot answer a later request.
- Answering in iTerm resolves the corresponding widget everywhere.
- Extension code gains no privilege beyond its declared session.

---

# M6 — Hardening and expansion

Signed and notarized distribution. Auto-update with rollback. Crash reporting that
excludes terminal content and environment data. Protocol compatibility matrix and
deprecation policy. Resource and replay limits. Soak testing for days-long sessions.
Network fault injection and relay failover. Linux support. Optional launchd agent for
logout survival and login auto-start, if daily use demands it. Terminal-specific launch
integrations (iTerm/Apple Terminal AppleScript), if the profile approach ever proves
insufficient. Team sharing. Swift packages, only if a native embedding customer appears.

---

## Cross-cutting test strategy

**Protocol conformance.** Language-neutral fixtures of encoded frames and expected
decoded values in `fixtures/`. Rust and TypeScript run the same set. A protocol version
is unsupported until every shipping client passes it.

**Terminal fidelity.** Recorded byte streams and interactive fixture programs, asserted
against normalized screen models rather than screenshots. The snapshot round-trip test
is the single most important test in the suite: stream → snapshot → replay into a fresh
emulator → assert identical screens.

**Lifecycle and concurrency.** Races among process exit, detach, stop, control transfer,
and reconnect. Duplicate frames and requests must be idempotent.

**Security.** Filesystem modes, socket peer identity, malformed and oversized frames,
path traversal, terminal escape injection in display metadata, redaction of launch
material. From M4: expired grants, device revocation, replay access.

**Privacy.** Automated inspection of cloud requests and diagnostics to prove terminal
bytes, environment values, and command secrets never enter metadata payloads.

---

## Release sequence

| Milestone | Ships | Proves |
| --- | --- | --- |
| M1 | Kernel + iTerm profile | Persistence costs nothing and breaks nothing |
| M2 | SSH/Termius phone attach | Remote agent interaction is worth wanting |
| M3 | Overlord Desktop chat | Whether chat beats terminal for agent work |
| M4 | Cloud reach + push + Overlord mobile | The product, for people who are not you |
| M5 | Widgets | How much better structured interaction is |
| M6 | Hardening, Linux, distribution | Readiness for people who are not you |

Each milestone is useful without the next. M1 and M2 are useful within days and require
no service, account, or second product.

---

## Open items

None outstanding.

Resolved: the local plane is Rust (D1), and Overlord Desktop is Electron and therefore
attaches directly to the worker socket, so M3 introduces no gateway process. The screen
model library is `vt100`, decided in M1 by running both candidates against
`fixtures/vt/`; the reasoning, the measurements, and what would justify revisiting it
are in [`../docs/DECISION_EMULATOR.md`](../docs/DECISION_EMULATOR.md).

History on attach and exited-session retention were decided in M2 against measurement
of the recorded streams in `fixtures/vt/`: an attaching client is sent the newest 200
scrollback lines under a 32 KiB ceiling and a resync is sent none, and an exited session
stays readable for 24 hours before `prune` reclaims it. The journal cap stays at 10 MB
per session — it is the raw record, and history on attach is served from the screen
model's structured ring rather than from it, which is what keeps a byte-tail replay out
of the reattach path. The measurements, including the finding that agent sessions
produce no scrollback at all, are in
[`../docs/DECISION_SCROLLBACK.md`](../docs/DECISION_SCROLLBACK.md).
