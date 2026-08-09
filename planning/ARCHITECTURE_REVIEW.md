# Latch Architecture and Plan Review

> **Status: resolved.** This review was written against the original architecture and
> plan. Its findings have been adopted and folded into
> [`PROJECT_ARCHITECTURE.md`](./PROJECT_ARCHITECTURE.md) and
> [`IMPLEMENTATION_PLAN.md`](./IMPLEMENTATION_PLAN.md), which are now authoritative.
> It is kept as the record of why those documents look the way they do.
>
> Two things changed after the review was written, once the target customer was pinned
> down as *people who use agents in the terminal but want to chat with them from their
> phone without giving up the terminal*:
>
> - **The resize recommendation in §1.3 is superseded.** "Controller's size wins" is
>   wrong on its own: a phone taking control to answer a prompt would leave the desk
>   session stuck at 40 columns. The adopted policy adds the missing half — the size
>   follows the controller **and reverts when that controller detaches**.
> - **The milestone spine in §6 is superseded.** The adopted sequence borrows
>   infrastructure instead of building it: an iTerm profile for adoption, SSH and
>   Termius for the first phone access, and Overlord's existing agent hooks for
>   notifications. That reaches a phone at milestone two rather than after a cloud
>   control plane. See the implementation plan.
>
> Everything else — particularly §1.1 on the screen model, and the simplifications in
> Part 2 — was adopted as written.

Review of [`PROJECT_ARCHITECTURE.md`](./PROJECT_ARCHITECTURE.md) and
[`IMPLEMENTATION_PLAN.md`](./IMPLEMENTATION_PLAN.md), cross-checked against
[`OVERLORD_INTEGRATION.md`](./OVERLORD_INTEGRATION.md).

## Verdict

The product boundaries are right and unusually well drawn. The four-component split,
"PTY is authoritative," "detaching is not terminating," local-first completeness, and
the refusal to let the cloud own session state are all correct and worth defending.
The Overlord integration document is the strongest of the three; its ownership table
and the process-spawn-vs-viewer-open success boundary are exactly right.

Two categories of problem remain:

1. **One correctness gap that will produce visible failure on the primary target
   workload** (Claude Code / Codex reattach). Everything else in Part 1 is smaller.
2. **A structural over-build in the MVP** — a Node daemon on the interactive data
   path, SQLite as a registry, two languages for three components, and a timer-based
   lease protocol. Each can be removed without losing a stated capability or a module
   boundary. Together they are most of the Phase 1 schedule.

Part 6 sketches the resulting MVP. Part 7 lists what only you can decide.

---

# Part 1 — Issues that will cause failures

## 1.1 A byte-oriented replay buffer cannot restore an alternate-screen application

**This is the most important finding in the review.**

The architecture specifies "a monotonically sequenced output journal or replay
buffer," and Phase 0's exit criterion is "reattachment restores enough output to
understand the current session." Replay is described purely as *bytes since sequence
N*.

That model breaks on the exact programs Latch exists to host. Claude Code, Codex, and
any TUI use the alternate screen and absolute cursor addressing. Replaying the tail of
a byte stream to a fresh terminal:

- can begin mid-escape-sequence, emitting garbage or leaving the terminal in a broken
  mode;
- replays only the tail of a full-screen redraw, so the screen is partially painted;
- does not restore modes that were set before the replay window — alternate screen on,
  bracketed paste on, cursor hidden, scroll region, current SGR attributes;
- gets worse the longer the session runs, which is precisely the persistent-session use
  case.

A user who closes iTerm during a Codex run and reattaches an hour later must see the
current screen, not a byte tail.

**Fix: the worker maintains a headless terminal emulator, not a byte buffer.**

The worker parses PTY output into a screen model (`vt100` or `wezterm-term` in Rust;
both are maintained and used in production terminal software). On attach, the worker
serializes the *current screen state* into a self-contained ANSI sequence that
reconstructs it exactly — modes, scroll region, cursor position, attributes, cell
contents — and sends that as the first payload, then streams live bytes from that
point.

This is what tmux does, and it is the difference between "reattach works" and
"reattach usually works."

**This single change also resolves three other open problems**, which is why it should
be treated as the keystone of the worker design:

| Problem | How the screen model solves it |
| --- | --- |
| Slow-client backpressure (1.2) | Drop a stalled client's queue and re-send a snapshot instead of buffering unboundedly or blocking the PTY. |
| Scrollback bounding (1.4) | Scrollback becomes a bounded line ring in a structured model, not an unbounded byte log. |
| Conversation-view fallback detection (Phase 3, item 5) | "Is the alternate screen active / is the app cursor-addressing?" is a direct property read, not a regex heuristic over raw bytes. |

Recommend replacing `terminal.replay` + `terminal.snapshot` in the message vocabulary
with a single snapshot payload carried on `session.attached`, and rewriting the Phase 0
exit criterion as: *reattaching to a running full-screen application reproduces the
screen byte-for-identically to what a continuously attached client shows.* That is
testable; "enough output to understand" is not.

## 1.2 Backpressure policy is untested and unspecified

The plan lists "output backpressure" as a Rust worker test but no document states the
policy. There are only two candidate behaviors and one of them is wrong:

- Block the PTY read when a client is slow → blocks the **child process**, so a phone on
  a bad cell connection freezes the agent for everyone. Unacceptable.
- Never block the PTY; give each attachment a bounded queue; on overflow, drop the queue
  and mark the client for resync.

The second requires the screen model from 1.1 to resync cheaply. State the policy
explicitly in the architecture: **a client's liveness must never gate the child
process.**

## 1.3 Multi-client resize has no policy, and a Phase 3 exit criterion depends on it

Clients send `terminal.resize`. Nothing defines what happens when a 200-column iTerm
and a 45-column phone are attached to the same session — which is exactly architecture
success criterion 4 and Phase 3's exit criterion "the web client can attach to the same
session concurrently with iTerm."

Undefined here means the last client to connect silently reflows everyone, including
resizing the agent's TUI out from under a user who is typing into it.

**Recommend: the session has one authoritative size. The controller's size sets it.
Watchers letterbox or pan; they do not resize the session.** This beats tmux's
smallest-window-wins because it removes the "someone's phone shrank my terminal"
surprise, and it is less code. Add a `latch resize <session> --cols --rows` escape hatch
and a `--follow-size` opt-in for watchers who do want to drive it.

Whatever you choose, it needs to be written down before Phase 3, because the frontend
and the worker must agree.

## 1.4 Scrollback has no defined source

"Searchable scrollback" is a Phase 3 frontend feature. The worker keeps a *bounded*
replay buffer. A client that attaches three hours into a session therefore has no
history to search, and nothing says whether that is intended.

Decide and record: either (a) scrollback is only what the client accumulated since
attaching — simple, and probably wrong for the agent use case where the point is to
check what happened while you were away; or (b) the worker keeps a capped on-disk
journal and clients request history ranges.

Recommend (b) with an explicit default cap (e.g. 10 MB or 50k lines per session,
configurable), because the whole product promise is "see what your agent did while you
were gone." This needs to be reconciled with the "bounded local replay file" language
in the security section, and session deletion must delete the journal.

## 1.5 Terminal escape injection has a concrete path from Overlord

The test strategy mentions "terminal escape injection in metadata," which is the right
instinct, but the concrete path deserves naming because it crosses a product boundary:

```
Overlord mission title (user-controlled)
  -> OVERLORD_INTEGRATION manifest display.title
  -> Latch session metadata
  -> Phase 2 item 7: "display session name and persistence state in terminal titles"
```

An escape sequence in a mission title reaches a terminal title-setting sequence. The
same applies to `latch list` output rendering names into a user's terminal.

**Rule to state in the architecture: all externally supplied display metadata is
sanitized to printable characters at ingest, not at render.** Sanitizing at render means
every future call site is a new chance to forget.

Related and worth stating plainly rather than implying: the daemon "authenticating local
clients" cannot defend against a same-uid process. Any process running as the user can
attach and type into an agent session. That is inherent to the design (tmux has the same
property) and should be an explicit accepted limitation in the threat model, not
something the word "authentication" appears to solve.

---

# Part 2 — Simplifications with no loss of functionality or modularity

Each item states what disappears and what it costs.

## 2.1 Take the daemon off the data path — then off the MVP

The architecture assigns `latchd` "routing client attachments to session workers." Read
literally, every keystroke and every output byte crosses `client -> daemon -> worker`,
with a Node.js process in the middle of an interactive path where keystroke latency is
the single most noticeable quality metric, and where GC pauses are directly visible. It
also means a daemon crash drops every live terminal.

**Change: the daemon resolves, it does not relay.** A client asks for a session, gets a
worker socket path, and connects to the worker directly. The daemon leaves the hot path,
a daemon crash no longer touches attached terminals, and the worker enforces its own
access rules — which is where they belong, since the worker is the thing being
protected.

Once the daemon is not relaying, ask what it still does in Phase 1:

| Daemon responsibility | Without a daemon |
| --- | --- |
| create / list / lookup / stop | CLI spawns the worker directly (`setsid`); lists by scanning the session directory; stops by sending a message to the live worker |
| SQLite registry | Per-session directory (2.2) |
| Routing attachments | Direct connect to worker socket |
| Naming and metadata | `meta.json` written by the worker |
| Launching preferred viewer | CLI does it — it is a fire-and-forget `open`/AppleScript call |
| Authenticating local clients | Directory mode `0700` + peer-credential check in the worker |
| Restart reconciliation | Nothing to reconcile — workers are self-describing |
| Cloud registration / rendezvous | **Genuinely needs a resident process. Phase 5.** |
| Browser WebSocket endpoint | **Genuinely needs a resident process. Phase 3.** |

The daemon earns its existence at Phase 3 (one loopback WebSocket gateway, so the
browser has a single endpoint instead of a port per worker) and again at Phase 5 (cloud
presence). It does not earn it in Phase 1.

**What this removes from the MVP:** the launchd user agent and its install/start/stop/
diagnose surface, daemon-restart reconciliation, the SQLite-versus-live-worker
divergence problem, the `lost` state as a *stored* state, daemon-side client
authentication, and an entire process to supervise.

**What it costs:** a worker that hard-crashes (SIGKILL) leaves a stale directory. That is
what `latch prune` is for, and a socket that refuses connection is definitive proof the
session is gone — a stronger signal than a database row.

Worth noting: Phase 0's exit criteria already require a worker that survives its client
exiting. Phase 0 therefore already builds the hard part, and Phase 1 currently wraps a
daemon around something that already works.

## 2.2 Replace SQLite with a per-session directory

The architecture already concedes the point: "The worker, not SQLite, keeps a session
alive. SQLite is a recovery index." A recovery index that must be reconciled against
live workers on every start is pure cost when the live workers are themselves
discoverable. WAL mode buys nothing with a single writer, and `session_attachments`
with `connected_at`/`disconnected_at` writes ephemeral presence data to durable
storage — that belongs in worker memory.

```text
~/.latch/sessions/<id>/
  meta.json      # format_version, name, title, cwd, command_label, created_at,
                 # source, external_metadata  (written once at spawn)
  control.sock   # worker socket; connectable == alive
  journal        # bounded output journal
  exit.json      # code + timestamp, written by the worker at exit
```

State is *derived*, which removes a whole class of lying-registry bugs:

```text
control.sock connects            -> running (ask the worker for detail)
exit.json present, socket gone   -> exited
neither                          -> lost
```

**Removes:** schema and migrations, WAL configuration, reconciliation, the
daemon-owns-the-DB constraint, and the stored/live divergence entirely.

**Costs:** write `meta.json` via temp-file-plus-rename to avoid torn writes. Name lookup
is a directory scan, which is instant at any plausible session count. Keep the migration
instinct as a `format_version` field.

Add SQLite later if a real query need appears. There is not one for `list` over tens of
sessions.

## 2.3 Consider one language for the local plane

Current split: Node daemon + Rust worker + Node CLI. The Rust worker is well justified —
signals, process groups, idle memory, no runtime to package. But the **CLI is also on the
interactive hot path**, and in Node it carries ~40 MB RSS and process startup on every
`latch attach`, for a program whose entire promise is "this feels like a native terminal
program."

The plan already anticipates the outcome: *"If packaging Node.js later becomes
undesirable, retain the protocol and replace only the registry daemon and CLI."* That
sentence predicts a rewrite of two of the three components.

If 2.1 moves the daemon to Phase 3, the split becomes principled rather than
incidental:

```text
Rust        local process plane   — worker + CLI, one binary, one codec
TypeScript  web / cloud plane     — Phase 3 gateway, browser SDK, Phase 5 control plane
```

One shipped artifact for the MVP: a single static `latch` binary that dispatches to
worker mode internally. Trivially notarizable, no Node to package, no runtime version
skew on a user's machine.

The stated reason for TypeScript is sharing protocol types with the future web SDK. That
benefit is weaker than it appears: the browser client speaks WebSocket, not a Unix
socket, so it is a different transport regardless; and the cross-language fixture suite
you already plan to maintain (for Rust, TS, and later Swift) is the actual mechanism that
keeps codecs honest. Codec implementations stay at two either way — a Rust CLI reuses the
worker's.

This is the one recommendation with a real tradeoff: TypeScript iterates faster, and if
the team is TS-heavy that matters more than the packaging cost. Flagged as a decision in
Part 7 rather than a defect.

## 2.4 Replace timer-based leases with connection-scoped exclusive write

The current model has lease renewal, expiry, stale-controller eviction, visible transfer,
and "user policy" governing whether the previous controller is notified before or at
transfer. That is a distributed-systems answer to a local problem.

Locally, the OS already tells you when a client is gone: the socket closes. A timer can
only be *wrong* relative to that signal — either evicting a live controller or leaving a
dead one in place.

**Simpler model with identical safety:**

- an attachment declares `watch` or `control` at connect time;
- the worker permits at most one `control` attachment;
- a second request for control returns `control_busy`; `--steal` demotes the current
  controller, which is notified;
- socket close releases control. No timers.

**Removes:** renewal messages, lease timers, timer-versus-disconnect races, and expiry
edge cases from the MVP.

Timer-based leases genuinely earn their place for *remote* clients, where a TCP
connection can hang without closing. Introduce expiry in Phase 5 alongside the cloud,
where the problem actually exists.

## 2.5 Shrink the MVP protocol from sixteen messages to about eight

Fewer messages is not cosmetic — every message multiplies across the Rust, TypeScript,
and eventual Swift fixture matrix you have committed to maintaining.

| Current | Proposed |
| --- | --- |
| `client.hello`, `session.attach`, `session.attached` | `attach` / `attached` — on a per-session socket there is no session to negotiate, and version negotiation rides on `attach` |
| `terminal.replay`, `terminal.snapshot` | folded into `attached` (see 1.1); removes a round trip on every reconnect |
| `control.request`, `control.granted`, `control.released` | `control.request` + a broadcast `control.state` |
| `presence.changed`, `session.state_changed` | one `session.update` carrying whatever changed |
| `terminal.output`, `terminal.input`, `terminal.resize`, `session.exited`, `protocol.error` | unchanged |

Also missing and needed: **the Unix-socket transport has no framing.** WebSocket gives
the browser client framing for free; the local transport must define its own (e.g.
`u8 type + u32be length + payload`) and the codec must be transport-agnostic. Discovering
this after the fixture suite exists means rewriting the fixture suite.

## 2.6 Stop storing PIDs as anything but display data

The `sessions` table holds `primary_pid` and `worker_pid` alongside the rule "must never
target a process using an unvalidated PID from stale storage." Storing them invites
precisely the mistake being warned against.

With 2.1 and 2.2, the rule becomes structural instead of a discipline: **`latch stop`
sends a message to the live worker over its socket, and the worker signals its own
child's process group.** No stored PID is ever consulted for a kill. If PIDs are kept for
display in `latch inspect`, label the field as display-only.

---

# Part 3 — Sequencing

## 3.1 Phase 2 is polish placed ahead of the phase that validates the architecture

Phase 2 delivers iTerm and Apple Terminal AppleScript launchers, window-versus-tab
preferences, terminal profile integration, and title display — conveniences layered over
a capability the user already has, since `latch attach x` works in any terminal after
Phase 1.

Phase 3 delivers the browser client, which is where the differentiated product value
lives (the entire conversational-agent thesis needs web and mobile) **and where the
protocol gets validated against a second, radically different client.** That validation
is the real architectural risk in the plan, and it is currently scheduled after a phase
of AppleScript.

**Recommend:** keep only the generic command-template launcher and the
`LATCH_SESSION_ID` nesting guard — roughly a day or two of work — and move the
terminal-specific AppleScript launchers to Phase 6 polish. Promote the web frontend.

This also composes with 2.1: Phase 3 is where the daemon first earns its existence, so
deferring the daemon and promoting the web client are the same move.

## 3.2 Move the nesting guard to Phase 1

`LATCH_SESSION_ID` detection is Phase 2 item 6. A user will run `latch` inside a Latch
session on day one, and `OVERLORD_INTEGRATION.md` already depends on the variable for
its "launch an objective from inside Latch" flow. It is a few lines. Move it to Phase 1.

## 3.3 Add `capabilities` to the Phase 1 command list

`OVERLORD_INTEGRATION.md` requires `latch capabilities --json` for discovery, and its
Stage 2 ("first-class local provider") is the minimum seamless integration. Phase 1's
command list is `create list inspect attach detach stop rename prune doctor` — no
`capabilities`. Overlord integration Stage 2 cannot be built against Phase 1 as written.

---

# Part 4 — Internal inconsistencies

Small, precise, worth fixing regardless of the structural decisions above.

1. **`ARCHITECTURE.md` "Creating a session manually," steps 5 and 7.** Step 5: "The CLI
   attaches to the session and requests input control." Step 7: "The CLI remains only a
   viewer." These contradict on their face. Intended meaning is presumably *the CLI is
   not the session's parent process*; say that.

2. **`detach` in the Phase 1 daemon command list.** Detaching is a client-side action —
   close the socket. A daemon-side `detach` implies force-kicking *another* client, which
   is a real feature described nowhere. Either specify force-detach or drop it.

3. **`unreachable` state.** `OVERLORD_INTEGRATION.md` lists `running / exited / stopping /
   lost / unreachable` as inspect results, but the architecture's state machine is
   `creating -> running -> stopping -> exited \-> lost`. `unreachable` is a
   *client-side connectivity projection* in the cloud case, not a session state. Label it
   as such, or the two documents disagree about what a session can be.

4. **Exited sessions: attachable or not?** Never stated. For agent workflows this matters
   a lot — the main reason to come back is to read what the agent did after it finished.
   Recommend exited sessions stay attachable read-only until pruned. Related: `prune`
   exists as a command but no retention policy is defined in any document.

5. **`web-demo` hosting is undecided.** The repo structure has it as a top-level app;
   Phase 3 item 8 says "served by `latchd` or a development server." Pick one — it
   determines whether the daemon needs an HTTP surface.

6. **Success criterion 7 has no supporting command.** "Configure and diagnose Latch
   entirely from the CLI" — Phase 1 ships `doctor` but no `config` command. Add
   `latch config get/set/path`.

7. **PID storage versus the stale-PID rule** — see 2.6.

8. **Platform seam.** The MVP is macOS-only, but the tech table justifies `portable-pty`
   by "macOS/Linux portability" and Phase 6 adds Linux. Nothing in the repo structure
   names where platform-specific code lives (launchd vs systemd, AppleScript launchers,
   socket paths). Name the seam now; it is free at this stage.

---

# Part 5 — What is right and should not be traded away

Stated explicitly so the recommendations above are not read as broader disagreement:

- **Separating process creation from viewer presentation.** This is the core insight and
  the thing tmux-alternatives get wrong. The Overlord success-boundary reversal
  (process spawned ≠ terminal opened) follows from it and is correct.
- **The PTY stays authoritative; rich UI is a projection.** This is what keeps
  conversation view from becoming a parallel truth that drifts.
- **Widgets bound to request ID + revision, first-writer-wins, resolved-or-stale.**
  The right answer, and the hardest part of Phase 4 to retrofit if omitted.
- **The Overlord ownership table** and the rule that a Latch session ID is not
  authorization.
- **Cross-language protocol fixtures as the conformance mechanism.** Keep this even if
  the CLI moves to Rust — arguably especially then.
- **Refusing to claim process persistence across reboot.** Honest, and it prevents a
  feature that would require checkpoint/restore.
- **"Each milestone must be useful without the next."**

---

# Part 6 — What the MVP looks like with these applied

```text
One Rust binary: latch
  latch run/shell    -> spawn a detached worker (setsid), write ~/.latch/sessions/<id>/,
                        connect to its socket, request control
  latch attach       -> resolve name -> dir, connect to control.sock, raw mode, stream
  latch list         -> scan session dirs; liveness = socket connects
  latch stop         -> message the live worker; the worker signals its own child pgroup
  latch prune        -> remove dirs with dead sockets past retention
  latch doctor       -> permissions, stale dirs, version
  latch config       -> get/set/path
  latch capabilities -> --json, for Overlord discovery

Worker (same binary, internal subcommand)
  owns PTY, child process group, headless VT screen model, bounded journal,
  attachments, exclusive-write arbitration, exit record

No daemon. No launchd. No SQLite. No Node.
```

Phase 3 introduces `latch-gateway` in TypeScript: one loopback WebSocket endpoint that
forwards to worker sockets, serving the browser client. That is the first point at which
a resident process is genuinely required. Phase 5 extends the same process with cloud
presence and rendezvous.

Every architectural boundary in `PROJECT_ARCHITECTURE.md` survives: the worker still
owns the PTY, clients still speak one versioned protocol, the protocol is still
transport-agnostic, harness enhancements are still optional, and the Overlord contract in
`OVERLORD_INTEGRATION.md` is unchanged — `latch create --manifest-file -` behaves
identically whether a daemon or the CLI spawns the worker.

What is gone is a supervised process, a database, a service manager, a language runtime,
and a lease protocol — none of which appear in any success criterion.

---

# Part 7 — Decisions that need you

1. **CLI language (2.3).** Rust gives one binary and better attach latency; TypeScript
   gives faster iteration and matches the team's likely center of gravity. This is a real
   tradeoff, unlike the others.
2. **Daemon in Phase 1 or Phase 3 (2.1).** If there is a near-term reason for a resident
   process I have not seen — auto-start on login, a menu-bar surface, background cloud
   registration sooner than Phase 5 — that changes the answer.
3. **Scrollback model (1.4).** Client-accumulated versus a capped on-disk journal, and the
   default cap.
4. **Resize policy (1.3).** Controller-wins is my recommendation; smallest-wins is the
   tmux-compatible alternative.
5. **Exited-session retention (Part 4, item 4).** How long, and read-only-attachable or
   not.

Items 1.1, 1.2, 1.5, 2.5, 2.6, 3.2, 3.3, and all of Part 4 are corrections rather than
choices, and can be applied to the two planning documents on request.
