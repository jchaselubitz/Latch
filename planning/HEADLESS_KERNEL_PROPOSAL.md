# Proposal: a Latch-native headless session kernel

> Final state (2026-08-31): the migration is complete and latchd is the sole
> shipped kernel. See [LATCHD_FINAL_MIGRATION.md](LATCHD_FINAL_MIGRATION.md) for
> the retired surface, verification gate, and rollback boundary. Earlier parts
> below are retained as staged decision history.

**Status:** proposal, coo:847. Decision at the end.
**Question:** if we replaced the private patched tmux (`latch-tmux`) with a
kernel built for Latch, what would it look like — and should we build it?

Requirements, as given:

1. A headless terminal: it contains the session and makes it available to
   clients that attach. No windows, tabs, or panes — presentation is the
   client's job.
2. Absolutely minimal overhead: a connected client feels directly connected to
   the underlying process.
3. Any TUI the session runs paints the client directly — no transcoding
   screen model on the live path.
4. Persistence and detach/reattach are the features copied from tmux.
5. First-class primitives for the Conversation Hub / chat system to drive a
   session.
6. Multiple concurrent sessions per host.

Requirements 2 and 3 are not aspirations; they are the shipped contract of
[`DECISION_EXCLUSIVE_ATTACH.md`](../docs/DECISION_EXCLUSIVE_ATTACH.md): one
human surface, exclusive steal, one current-frame snapshot, then the pane's
own bytes. This proposal does not revisit that contract. It asks what the
smallest process that can honor it looks like.

---

## Part 1 — What we actually use from tmux

The honest starting point is an inventory. `crates/latch/src/engine.rs` is the
only module that talks to the kernel, and its entire demand on tmux is:

| Engine call | tmux mechanism | Notes |
| --- | --- | --- |
| `create` | `new-session -d`, `remain-on-exit on` | plus a FIFO launch shim |
| `attach_exclusive` | **our patch** (`-R attach-session`) | steal, snapshot, raw splice |
| `list` / `inspect` | `list-sessions` + format strings | polled |
| `capture_pane` | `capture-pane -p -J` | subprocess per call |
| `paste_message` | `load-buffer` + `paste-buffer` + `send-keys Enter` | three subprocesses |
| `send_keys` | `send-keys` | subprocess per call |
| `resize` | `resize-window` | |
| `stop` / `kill_session` | signal pane, `kill-session` | |
| exit records | `remain-on-exit` + pane format variables | |
| first-viewer gate | `wait-for` channel | |
| kernel verification | probe binary flag + `#{latch_raw_kernel}` | needed only because the kernel is replaceable-by-accident |

That is the whole surface. Everything tmux is famous for — per-client grid
rendering, terminfo translation per attached terminal, windows, panes,
layouts, copy mode, the status line, space-multiplexing — is either configured
off (`~/.latch/tmux.conf` strips the status bar, prefix, and copy-mode keys)
or **patched out of the data path** by
[`patches/tmux/0001-latch-exclusive-raw-attach.patch`](../patches/tmux/0001-latch-exclusive-raw-attach.patch).

And the load-bearing behavior — the thing requirement 2 and 3 rest on — is
not tmux at all. It is ~860 lines of our own C, applied to pinned tmux 3.7b
internals:

- **Patch 1, exclusive raw attach:** a client that identifies with `-R` steals
  the session, receives one snapshot of the pane grid, and is then spliced to
  raw pane bytes. tmux's renderer never touches the live stream.
- **Patch 2, deferred parse:** because tmux's architecture assumes the grid is
  parsed synchronously on the pane read callback, keeping its grid current
  *while also* forwarding raw bytes required rescheduling `input_parse_pane`
  onto a 1 ms timer in 4 KiB slices, with a 256 KiB backlog cap, a bounded
  catch-up slice so steal and `MSG_EXIT` are not starved, and a full
  parse barrier before every snapshot
  ([`ai/history/2026-08-23-coo-840-kernel-patch-2.md`](../ai/history/2026-08-23-coo-840-kernel-patch-2.md)).

Patch 2 exists because we are holding tmux's event loop in a shape it was
never designed for. It works — 18 e2e tests, ~70k CSI frames/s without
stalling the child — but it is us maintaining a concurrency model *inside*
someone else's single-threaded C server, against internals with no stable
API, on a pinned release, with our own build pipeline
(`scripts/build-tmux.sh`, utf8proc vendoring, patch manifests with sha256s,
and a third Developer-ID-signed binary in every payload). The last two days of
`ai/history/` include a malformed-hunk incident in that patch pipeline.

So the question is not "can we write something as good as tmux." It is: **the
part of tmux we depend on is already custom code we wrote; should it live as
patches inside a 60k-line C server we use 5% of, or as a small program we
own end to end?**

## Part 2 — Why this is not latch-term v1 again

This repository has already built and deleted a custom terminal server.
[`ATTACHMENT_ARCHITECTURE_REVIEW.md`](./ATTACHMENT_ARCHITECTURE_REVIEW.md)
diagnosed why v1 failed, and the diagnosis was specific — two invariants tmux
held and v1 did not:

1. *The grid is the only thing a client ever sees* — tmux re-renders every
   client from its screen model at that client's geometry; v1 broadcast raw
   bytes to multiple clients at mismatched sizes and repainted nobody on
   resize.
2. *Attaching cannot fail* — v1 made "who may type" a connect-time gate and
   returned errors where tmux returned a screen.

Both failure causes are structurally absent from what we would build now,
because [`DECISION_EXCLUSIVE_ATTACH.md`](../docs/DECISION_EXCLUSIVE_ATTACH.md)
changed the product contract underneath them:

- There is **at most one surface**, so there is no fan-out of raw bytes to
  clients at the wrong geometry — the one surface *is* the geometry. The
  entire per-client rendering problem, the hardest thing tmux does and the
  thing v1 got wrong, no longer exists in the requirements.
- Attach **steals by design**, so there is no connect-time refusal path. The
  v1 `ControlBusy` error is not a bug to avoid; it is a state that cannot be
  expressed.

v1 failed because it broadcast raw bytes while pretending to multiplex over
space. The current contract multiplexes over time only. A kernel built for
that contract is a different, much smaller program — and we know it is
buildable, because we already built it twice: once as latch-term v1's worker
(wrong contract, right mechanics) and once as the tmux patches (right
contract, wrong host).

`docs/ARCHITECTURE_RULES.md` currently forbids recreating "worker, framing,
attachment registry, screen-model, or resize-authority modules in the active
workspace." That rule encodes ENGINE_PLAN's decision. Adopting this proposal
supersedes that line for the new kernel crate specifically; the rule's intent
— never put a screen model on the live path, never gate attach — stays and is
restated below as invariants.

## Part 3 — Architecture: `latchd`

One new crate, `crates/latchd`, producing one binary that replaces
`latch-tmux` in the payload. `engine.rs` keeps its exact public surface and
becomes a client of `latchd` instead of a driver of tmux subprocesses.

### 3.1 Process model: one daemon per session

`latch create` forks a `latchd` instance per session: double-fork, `setsid`,
PTY master via `openpty`, child spawned in the PTY slave as session leader.
There is **no central server**.

```text
~/.latch/sessions/<id>/
  ctl.sock        # unix socket, 0600, dir 0700; control + attach
  manifest.json   # existing launch manifest (unchanged)
  daemon.pid
  exit.json       # written on child exit; survives until prune
```

- `latch list` scans the directory and liveness-checks each daemon (connect
  or `kill(pid, 0)`), instead of round-tripping `list-sessions` format
  strings through a server.
- **No shared fate.** A crash, upgrade, or kill of one session's daemon
  touches one session. The coo:751 field report found the tmux server 53
  minutes old under a 25-hour desktop uptime — every session predating that
  restart silently became `lost`. With per-session daemons that failure class
  disappears, and `latch update` can replace the kernel binary without
  ending anything already running.
- Sessions are enumerable and inspectable even when a daemon is dead: the
  directory with `exit.json`, or without a live pid, *is* the `exited` /
  `lost` state. Today those states are derived by diffing metadata against a
  tmux server that may have restarted.

The cost is N processes instead of one. A daemon at rest is one PTY fd, one
listening socket, a grid, and a bounded scrollback ring — the measured
`latch-tmux` server was 3.3 MB RSS for four sessions; expect a few MB per
daemon. For the tens of concurrent agent sessions Latch targets, this is
noise, and it buys the isolation.

### 3.2 The hot path: a splice, nothing else

The daemon's event loop owns three things: the PTY master, the control
listener, and at most one **live surface** connection.

```text
child PTY ──read──► daemon ──write──► surface socket ──► latch attach ──► tty
        ◄──write── daemon ◄──read───                 ◄── keystrokes ◄──
```

Output is forwarded byte-for-byte, unmodified. Input is written to the PTY
byte-for-byte, unmodified. No parsing, no framing, no per-message headers on
the live path — after the attach handshake the socket *is* the byte stream in
both directions. That is one `read` and one `write` per direction through a
unix socket, the same hop count as the patched tmux path today, implemented
in a loop small enough to read in one sitting.

Backpressure policy is copied from what patch 2 proved out: the child is
never stalled to protect a surface. If the surface's socket buffer fills past
a bound, the surface is **evicted with a reason** (the slow-client eviction
the e2e suite already asserts), the session keeps running headless, and the
next attach gets a current frame.

### 3.3 The grid: off the hot path, on a thread

The daemon keeps a current-frame model so that attach and the Hub always have
"the pane now" — the same reason the deferred-parse patch exists. But in a
process we own, the entire patch collapses into ordinary threading:

- Bytes read from the PTY are forwarded to the surface first, then appended
  to a queue consumed by a **parser thread** owning the screen model.
- The parser can never starve the event loop, because it is not in it. The
  1 ms slice timer, the 4 KiB slices, the 256 KiB backlog cap, the bounded
  catch-up slice — patch 2's whole apparatus — become "a thread."
- Snapshot requests carry a sequence barrier: the daemon notes the byte
  offset at request time and the parser answers when it has parsed past it.
  Same guarantee as patch 2's full-parse-before-snapshot, without a timer
  dance. A Rust VT parser processes input orders of magnitude faster than any
  child produces it; the barrier is microseconds in practice.

The screen model is `vt100` plus the adapter work from `archive/latch-term-v1`
— the mode tracker, the whole-sequence/whole-character chunk filter, the
wide-character resize repair.
[`DECISION_EMULATOR.md`](../docs/DECISION_EMULATOR.md) measured that stack at
11/11 snapshot-fidelity on the recorded agent fixtures, with
`state_formatted` producing 71–1431 bytes per screen, and its accepted gaps
(blink/conceal/strikethrough, IRM/DECAWM enforcement) verified absent from
every recorded Claude/Codex stream. The fixture suite in `fixtures/vt/` is
the acceptance gate, re-run, not re-argued.

Note what the grid is *not* for: it never renders to the live surface
(invariant, carried over from the exclusive-attach decision). It exists for
exactly three consumers: the attach snapshot, the Hub's structured snapshot,
and scrollback capture.

### 3.4 Attach: steal, one frame, then bytes

The wire realization of the existing contract:

1. Client connects to `ctl.sock`, sends `Attach { cols, rows, reason }`.
2. Preflight: session must exist and the daemon be live. A live session with
   another surface is valid — there is no busy error, only steal.
3. Atomically: the previous surface (if any) is sent
   `Detached { reason: stolen }` and closed; its `latch attach` restores the
   tty and exits with the same reasoned release codes `SurfaceRelease`
   classifies today (stolen / session-ended / attach-failed).
4. The PTY is resized to the new winsize; the child gets `SIGWINCH`.
5. Parser barrier, then the daemon writes the **current frame** — cursor,
   modes, alternate screen, title — as standard xterm sequences
   (`state_formatted`), framed as the last control payload.
6. A `Live` marker, after which the socket is raw bytes both ways and the
   daemon is a splice.

`latch attach` keeps its interface; `engine::attach_exclusive` stops spawning
a tmux client and speaks this handshake directly. The gateway's
`/v2/sessions/{id}/terminal` WebSocket and the Desktop viewer follow the
identical sequence, as they are required to today. The
first-viewer gate (`wait-for` today) becomes daemon state: `create
--hold-until-viewer` defers the child's launch until the first successful
attach or an explicit `release` verb.

### 3.5 The control plane: what the chat system actually gets

This is the part that is a genuine capability gain rather than a port.
Today every Hub interaction with a live session is a subprocess: composing a
message is `load-buffer` + `paste-buffer` + `send-keys` (three tmux client
processes, three server round-trips), and observation is polling
`capture-pane`. There are no events; "did the agent stop and ask something"
is inferred by re-capturing and diffing.

`latchd` exposes the control plane on the same socket as typed frames
(length-prefixed JSON; a connection that never sends `Attach` is a control
connection, any number may be open concurrently):

**Verbs**

| Verb | Replaces | Notes |
| --- | --- | --- |
| `write { bytes }` | `send-keys --` | raw injection |
| `key { name }` | `send-keys` named keys | small table, mode-aware (DECCKM, keypad) |
| `paste { text }` | buffer dance | wrapped in bracketed paste iff the grid says DECSET 2004 is on — the daemon *knows*, today we guess |
| `submit { text }` | `paste_message` | paste + Enter as one atomic verb, so the "pasted but not submitted" recovery note in `engine.rs` disappears |
| `snapshot { format }` | `capture-pane` | `text`, `escape-stream`, or structured JSON (cells + attrs + cursor + modes) for the Hub |
| `history { max }` | — | bounded primary-screen scrollback ring (below) |
| `stat` | `list-sessions` formats | pid, cwd, winsize, state, surface holder, child exit |
| `resize`, `signal`, `kill`, `release` | resize-window / kill-session | |

**Events** — a control connection can `subscribe` and receive pushes:

- `child-exited { status }`
- `surface-attached` / `surface-detached { reason }`
- `title-changed`, `bell`
- `alt-screen { entered/left }`, `cursor-visibility`, `mode-changed`
- `output-quiet { ms }` — quiescence notification, the primitive behind
  "the agent has stopped painting and is probably waiting on input"

Events are what tmux structurally cannot give us without yet another patch:
its client protocol has no push channel we could subscribe from Rust without
holding a control-mode client and parsing its notification text. For the
Conversation Hub — whose channel-2 observation today leans on transcript
files precisely because the terminal side is opaque — `output-quiet`,
`alt-screen`, and `child-exited` as pushes replace polling loops.

Latency also improves category, not degree: a control verb is a write on an
already-open socket (microseconds) instead of `fork`/`exec` of a tmux client
plus a server round-trip (milliseconds) per call.

### 3.6 Persistence, exit, scrollback, security

- **Exit retention:** on child exit the daemon writes `exit.json`, keeps the
  final grid, and lingers so `latch attach` on an exited session still shows
  the last frame (the `remain-on-exit` behavior). The 24-hour retention and
  `latch prune` policy from
  [`DECISION_SCROLLBACK.md`](../docs/DECISION_SCROLLBACK.md) is unchanged;
  prune ends the daemon and removes the directory.
- **Scrollback:** a bounded primary-screen ring mirrored out of the parser
  (the latch-term mechanism), alternate-screen output excluded, oldest
  dropped first, dropped count kept. It is served **only** through the
  `history` control verb for `latch inspect` and the Hub. It is not an attach
  payload — attach stays "last frame, not replay."
- **Security:** session dir 0700, socket 0600, peer-uid check
  (`LOCAL_PEERCRED`) on every connection. Same posture as the private tmux
  socket, minus the risk of a user's own tmux ever being confused for the
  kernel — the entire binary-probe and `#{latch_raw_kernel}` verification
  machinery in `engine.rs` is deleted, because there is no upstream binary
  the kernel can be accidentally swapped for.
- **Environment:** the child env handling (`TMUX` removal becomes moot,
  `LATCH_SESSION_ID` stays the nesting marker) and the FIFO launch shim carry
  over unchanged.

### 3.7 Invariants (carried forward, enforceable in review)

1. The screen model is never on the live path. After `Live`, the daemon
   forwards bytes it does not interpret.
2. Attach never fails because someone else is attached. Steal is the only
   contention semantic.
3. The child is never stalled to protect a surface or the parser. Slow
   consumers are evicted or lag; the session is primary.
4. One human surface at a time. Observation without control goes through
   `snapshot` / `history` / events, which cannot back-pressure the pane.

## Part 4 — What we give up, honestly

- **tmux's field-testing.** Twenty years of tty arcana: signal edge cases,
  exotic terminals, forkpty quirks across platforms. Mitigation is scope: the
  daemon needs no terminfo at all (the client terminal renders; snapshots are
  emitted as plain xterm sequences against the already-pinned
  `default-terminal`), supports one OS family today (macOS, Linux next), and
  the tty-handling surface is `openpty` + `SIGWINCH` + raw mode — the same
  narrow slice v1's worker and every Rust PTY crate already exercise.
- **Grid emulation maturity.** tmux's `input.c` handles sequences vt100 does
  not. The measured answer from DECISION_EMULATOR stands: across every
  recorded agent stream, the vt100+adapter stack was byte-perfect on
  snapshot round-trips and the gaps were unused. The risk is a *future* TUI
  using something outside that envelope; the fixture corpus and its
  chunk-size sweeps are the regression net, and recording new fixtures is
  the standing procedure when an agent misbehaves.
- **A second kernel during migration.** For one release window the payload
  carries both kernels behind a flag. Bounded by the cutover plan below.
- **Opportunity cost.** Realistically 4–6 engineer-weeks to the parity gate
  (Part 5), against a kernel that works today. This is the strongest argument
  for "stay," and the reason the recommendation is staged rather than a
  rewrite-in-place.

Prior art worth naming: `shpool` (Rust, session-per-daemon, attach/detach,
explicitly "dtach with a plan") validates the shape at production quality,
though it restores by replaying a raw ring rather than a grid snapshot, which
is exactly the piece our exclusive-attach contract requires and our vt100
work supplies. `dtach`/`abduco` are the same shape minus any screen model —
they demonstrate how little a headless kernel needs, but cannot produce a
last frame. Zellij is a full multiplexer with the same
renderer-on-the-path property we just patched out of tmux.

## Part 5 — Migration: behind the seam we already have

`engine.rs` is the single choke point, and the test suites are
kernel-relative already (`LATCH_E2E_TMUX_BIN` selects the binary under test).

1. **Phase A — parity.** Build `latchd` to the surface in Part 3.1–3.4 only
   (create, list/inspect, attach/steal/snapshot/splice, resize, stop, exit
   records, first-viewer gate). Gate: the existing `exclusive_attach_e2e`
   (18 tests) and phase-0 kernel suites pass against `latchd`; the
   `fixtures/vt/` corpus passes through the parser at every chunk size; the
   patch-2 performance assertions hold (child unstalled under a ~70k
   frames/s writer, 1.0000x post-boundary amplification, slow-client
   eviction).
2. **Phase B — cutover.** Payload ships `latchd`; `latch-tmux` remains in
   the payload one release behind a `LATCH_KERNEL=tmux` escape hatch. New
   sessions use the new kernel; existing tmux-hosted sessions run to
   completion on the old one (no live migration — sessions are days, not
   months). Soak on our own machines first, as with every kernel change so
   far.
3. **Phase C — the control plane.** `snapshot`/`submit`/`key`/events land,
   the Hub's injection path (`conversation/connectors/jsonl.rs`) moves off
   subprocess `send-keys`, and `capture-pane` polling is retired. This phase
   is where the build pays rent beyond parity.
4. **Retire** the patch pipeline: `patches/tmux/`, `scripts/build-tmux.sh`,
   the manifest sha256 machinery, the kernel-verification preflights, and
   one of three signed binaries in the payload.

## Recommendation

**Build it — staged as above, keeping `latch-tmux` shipping until the parity
gate is green and soaked.** Not because tmux is failing us today (patch level
2 passes its suite and holds its numbers), but because of where the
trajectory points:

1. **We already own the hard part.** The behavior Latch depends on —
   exclusive raw attach, snapshot-then-splice, deferred parsing — is our
   code, maintained as patches against the internals of a C server that
   assumes the opposite architecture. Each patch fights tmux's design
   (patch 2 is a hand-rolled scheduler inside its event loop); each upstream
   release is a rebase of that fight; the patch pipeline has already produced
   incidents. The custom kernel is not *more* custom code than we have — it
   is the same custom behavior moved to a host that agrees with it.
2. **The next requirement lands worse in tmux.** The chat system needs a
   control plane with events and structured snapshots. In tmux that is patch
   3 and patch 4 — more private C against a moving upstream, or subprocess
   polling forever. In `latchd` it is the natural API of a process we own.
   Requirement 5 is the tiebreaker: staying with tmux means the Hub's
   terminal-side primitives stay subprocess-and-poll.
3. **The v1 failure does not forecast this build.** v1 failed on two
   specific invariants — per-client rendering and connect-time exclusivity —
   both of which the exclusive-attach contract has since removed from the
   requirements. The risky component that remains (screen-model fidelity)
   is the one piece v1 demonstrably got right, is already measured at
   11/11 on the fixture corpus, and sits off the hot path.
4. **Shared fate is a real, observed defect.** The coo:751 report shows a
   tmux server restart orphaning every session's metadata at once.
   Per-session daemons remove that class, and make kernel updates
   non-disruptive.

**Stay with tmux instead if** any of these holds: the team cannot spend the
4–6 weeks before the chat control plane is needed; a product direction toward
space-multiplexing (two live mirrored surfaces) re-emerges, which would
resurrect the per-client rendering problem tmux solves and this design
deliberately does not; or Phase A misses its parity gate on fixture fidelity,
which would mean the emulation envelope was measured wrong and tmux's
`input.c` is load-bearing after all. In that world, the fallback is explicit:
keep the patched kernel, accept subprocess-and-poll as the Hub's terminal
interface, and budget a rebase per upstream tmux release.

---

## Part 6 — Phase A, built (coo:847.r29g)

The parity build in Part 5 exists. It is behind the seam, off by default, and
green on the acceptance gates named above.

### What shipped

- **`crates/latch-term`** — the v1 screen model, restored from the
  `archive/latch-term-v1` tag unchanged. It is a leaf crate (`vt100` +
  `thiserror`), and its suite passes as recorded in `DECISION_EMULATOR.md`:
  the `fixtures/vt/` corpus round-trips at every chunk size, and the
  wide-character and resize cases hold. The daemon's parser thread *is* a
  `latch_term::Terminal`.
- **`crates/latchd`** — one binary, one session:
  - `pty.rs` — `openpty` + `fork`/`exec`, child as session leader with the
    slave as controlling terminal, in its own process group (so `-pid`
    signalling reaches the job, as tmux's `pane_pid` did).
  - `daemon.rs` — the reader/parser/surface-writer/connection threads of
    Part 3.2–3.4. The hot path is a byte splice; the parser owns the grid off
    the event loop; a snapshot is an item *in* the parser queue, so it is
    answered at its exact stream position (the deferred-parse barrier, as
    ordinary threading — patch 2 collapses). Slow surfaces are evicted at a
    4 MiB bound; the child is never stalled.
  - `protocol.rs` / `client.rs` — length-prefixed JSON control frames, then a
    raw surface after the attach handshake. `attach_tty` is the human surface:
    raw mode, paint one frame, splice, restore the tty on every exit path.
  - `keys.rs`, `render.rs`, `paths.rs` — the `send-keys` name table
    (mode-aware), the snapshot renderings (`text`/`styled`/`escape`/`json`),
    and the short per-user socket directory with a `kernel.json` pointer in
    each session dir.
- **The seam** — `engine.rs` gained a `Kernel` selector (`LATCH_KERNEL`,
  default `tmux`). Every verb the inventory in Part 1 listed dispatches to
  `engine/latchd_kernel.rs` when `latchd` is selected; the returned shapes
  (`SessionInfo`, `SurfaceRelease`) are unchanged, so nothing above the seam
  moved. `doctor` checks the daemon instead of tmux on that kernel.

### Gate results

- `cargo test --workspace` (default `tmux` selector): unchanged — the tmux
  suites, including `tmux_kernel` (23) and the library (121), still pass. The
  daemon is inert until selected.
- `crates/latchd` unit + integration: 6 + 15, against the real `latchd`
  binary — attach-snapshot-then-splice, steal with reasons, control verbs
  driving a headless child, bracketed-paste-iff-DECSET, history, exit
  retention and last-frame-on-reattach, slow-client eviction, `output-quiet`
  and the other events, `await_surface`, `kill`, plus a byte-exact throughput
  gate.
- `crates/latch --test latchd_kernel_e2e` (10, mandatory and internally
  serialized): the real `latch` binary with `LATCH_KERNEL=latchd` through real
  PTYs — the same shapes `exclusive_attach_e2e` asserts on patched tmux, plus
  an engine-level control-plane test that drives a pane through
  `engine::paste_message` / `send_keys` / `capture_pane`.

### How to run it

```text
cargo build -p latchd                       # daemon lands next to `latch`
LATCH_KERNEL=latchd latch create ...        # a session on the daemon kernel
LATCH_KERNEL=latchd latch attach <id>       # exclusive raw surface
cargo build -p latchd && \
  cargo test -p latch --test latchd_kernel_e2e -- --test-threads=1
```

### What Phase A does *not* yet do (Part 5, Phases B–C)

- No payload/packaging change: `latch update` still ships only `latch-tmux`
  and `latch-remote`; the daemon is not signed into the release payload, and
  the gateway PTY host (`cli/serve/pty.rs`) still spawns `latch attach`, which
  now speaks either kernel by selector but has not been pointed at `latchd` in
  production.
- The Hub still calls `capture_pane` / `paste_message`; the event-driven
  observation path (subscribe to `output-quiet` / `child-exited` instead of
  polling) is Phase C and is not wired into `conversation/`.
- Default stays `tmux`. Flipping the default is the Phase B soak decision,
  not this objective.

---

## Part 7 — Phase A hardening gate (coo:847.krze)

Phase A is now a reproducible gate rather than an opt-in suite that could
report success without finding the daemon it was meant to test.

### What changed in the gate

- The engine-level `latchd_kernel_e2e` harness treats a missing or invalid
  `latchd` path as a hard failure. Its cases serialize inside the test binary,
  so the process-wide kernel selector and real PTYs cannot race even when a
  caller forgets `--test-threads=1`.
- Rust CI explicitly builds `latchd`, then runs the real-kernel parity suite
  on both `macos-latest` and `ubuntu-latest` before the workspace tests. The
  workspace `Cargo.lock` records the current versions of the two new crates,
  so the locked build is reproducible.
- The daemon authenticates socket peers on both supported CI families:
  `getpeereid` on Darwin/BSD and `SO_PEERCRED` on Linux. The real-daemon suite
  asserts that the socket and `kernel.json` are mode `0600` and that the
  socket belongs to the current uid. Linux target check and clippy cover the
  Linux credential path and its platform-specific `openpty` declaration.
- The integration sockets live in each harness's private temp directory
  rather than the global `/tmp` namespace. Production continues to use its
  short per-user `0700` directory; the test arrangement makes isolation and
  cleanup deterministic.

### Reproducible acceptance commands

```text
cargo test --locked --package latch-term --all-targets
cargo test --locked --package latchd --all-targets -- --test-threads=1
cargo build --locked --package latchd
cargo test --locked --package latch --test latchd_kernel_e2e -- --test-threads=1
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo check --locked --target x86_64-unknown-linux-gnu --package latch --test latchd_kernel_e2e
cargo clippy --locked --target x86_64-unknown-linux-gnu --package latchd --all-targets -- -D warnings
```

The screen-model gate is 55 tests, including recorded fixture fidelity,
arbitrary chunk boundaries, mid-stream resize, wide characters, alternate
screen, and bounded scrollback. The daemon gate is 6 unit plus 15
real-binary integration tests. The engine gate is 10 real-PTY tests through
the authored `latch` binary.

The throughput case sends 200,000 fixed ten-byte CSI frames through a child
PTY after the snapshot boundary, reads exactly 2,000,000 live bytes, and
asserts byte identity, `1.0000x` amplification, a running child afterward,
and at least 70,000 frames/s. A representative debug run on the arm64 macOS
development host measured 4,471,493 frames/s, `1.0000x` amplification, and
44 ms elapsed. This is an acceptance floor, not a claim that CI or release
hardware will reproduce that particular peak number. The existing 16 MiB
non-reading-surface case still proves that the 4 MiB surface queue evicts the
client while the child reaches its completion marker and remains running.

### Accepted gaps carried into soak

- This objective cross-compiles and clippy-checks the Linux paths locally;
  the real Ubuntu PTY run occurs in the CI job when the branch is integrated.
- A cross-uid rejection cannot be manufactured by an unprivileged unit test.
  Both platform credential APIs fail closed, and every successful integration
  connection exercises the same-uid path.
- The live surface queue (4 MiB), control frames (16 MiB), and scrollback
  (50,000 lines) are hard-bounded. The off-path parser queue is deliberately
  lossless rather than byte-capped: dropping it would make later snapshots
  false, while blocking it would violate the child-first invariant. Phase A
  proves catch-up across the throughput and 16 MiB backpressure cases; the
  dogfood objective records parser backlog and RSS under longer workloads
  before the default changes.

---

## Part 8 — Phase A integrated into `main` (coo:847.s08z)

Phase A now lives on `main`. Nothing about a default Latch install changes:
the shipped payload is still `latch`, `latch-tmux`, and `latch-remote`, and
the kernel selector still resolves to `tmux` for every process that does not
ask for otherwise.

### The merged architecture

`main` gains two crates and one seam, and no existing call site moved.

- `crates/latch-term` — the standalone screen model (grid, modes, scrollback,
  snapshot round-trip) with its own fixture-fidelity and chunk-boundary
  suites. It is a library only; nothing links it except the daemon.
- `crates/latchd` — one daemon per session: `pty.rs` forks the child as a
  session leader in its own process group, `daemon.rs` runs the reader,
  parser, surface-writer, and connection threads with the raw splice as the
  hot path and the grid strictly off it, `protocol.rs`/`client.rs` carry
  length-prefixed JSON control frames and the post-handshake raw surface, and
  `keys.rs`/`render.rs`/`paths.rs` supply the `send-keys` name table, the
  snapshot renderings, and the short per-user `0700` socket directory with a
  `kernel.json` pointer per session.
- `crates/latch/src/engine.rs` — the `Kernel` selector and its dispatch. Every
  verb inventoried in Part 1 checks `kernel()` first and hands off to
  `engine/latchd_kernel.rs` when the daemon is selected. `SessionInfo` and
  `SurfaceRelease` are unchanged, so `cli/`, `conversation/`, `serve/`, and
  the clients above them are untouched by the merge.

The branch had already absorbed `main` before this objective, so integration
is a merge with no textual conflicts and no divergence to unwind. It is
recorded as a single no-fast-forward merge commit precisely so the rollback
below is one revert rather than an archaeology exercise.

### Proof that the daemon is inert unless selected

- `kernel()` maps only the exact string `latchd` to the daemon; every other
  value, and an unset variable, is `Kernel::Tmux`.
- Every daemon entry point in `engine.rs` sits behind `kernel().is_latchd()`,
  and `doctor` branches on the same selector, so a default process never
  resolves, executes, or links against a daemon path at runtime.
- `cargo test --workspace --all-targets` runs under the default selector and
  the tmux suites are unchanged. The daemon suites reach `latchd` only
  because they set the selector themselves.
- A default-kernel `latch doctor` run on a clean `LATCH_HOME` spawns no
  daemon process and creates no socket; it reports tmux exactly as before.
- `LATCH_LATCHD_BIN` is honoured only under `debug_assertions`. On a release
  build an operator who sets `LATCH_KERNEL=latchd` today gets
  `bundled kernel .../latchd is missing; run latch update to repair the
  complete payload` — the daemon fails closed instead of half-working from an
  arbitrary path. Shipping it is Phase B (coo:847.c51g).

### Operator commands

```text
latch <verb> ...                            # unchanged: the tmux kernel
LATCH_KERNEL=tmux latch <verb> ...          # the same thing, said explicitly

cargo build -p latchd                       # daemon lands beside `latch`
LATCH_KERNEL=latchd latch create ...        # a session on the daemon kernel
LATCH_KERNEL=latchd latch attach <id>       # exclusive raw surface
LATCH_KERNEL=latchd latch doctor            # checks the daemon, not tmux
LATCH_KERNEL=latchd latch inspect <id>      # same shapes as the tmux kernel
```

The selector is per process and applies to session creation. A session is owned
by whichever kernel created it; subsequent list, inspect, attach, drive, and
lifecycle calls route from its protected `kernel.json` record (record absence
means the legacy tmux kernel). There is no live migration and none is planned
(coo:847.qaw3 preserves existing tmux sessions until they exit).

Reproducing the gate locally:

```text
cargo build --locked --package latchd
cargo test --locked --package latch --test latchd_kernel_e2e -- --test-threads=1
cargo test --workspace --all-targets
```

### Verified at integration

Run on the arm64 macOS development host against this merge:

| Check | Result |
| --- | --- |
| `scripts/check-boundaries.sh` | ok |
| mobile contract drift | current |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo build --locked -p latchd` | ok |
| `latchd_kernel_e2e` (serial, real daemon) | 10/10 |
| `cargo test --workspace --all-targets` | all suites pass; `latchd` 6 unit + 15 integration |
| `cargo test --workspace --doc` | clean |
| default-kernel `latch doctor` | no daemon spawned, no socket created |

### Remaining rollout risks

- **Orphaned daemons have no reaper.** A daemon observed from the previous
  objective's test run was still alive a day later, holding its socket, after
  its session directory had been deleted underneath it: lifetime is bound to
  the child, and the child was blocked forever on a launch FIFO that never got
  written. On the tmux kernel a comparable abort leaves work on one shared
  server that `latch` already knows how to clean. Before the default flips,
  the daemon needs a bounded startup handshake and an exit when its session
  record disappears, and `latch doctor` needs to see and clear strays. This is
  soak-blocking (coo:847.wff1), not merge-blocking, because nothing reaches
  this path without the selector.
- **Linux has been checked, not exercised.** The Linux credential path
  (`SO_PEERCRED`) and the platform-specific `openpty` declaration are covered
  by cross-target clippy and `cargo check`; the first real Ubuntu PTY run of
  the parity suite happens in the CI job this merge enables.
- **The throughput floor is a floor.** 70,000 frames/s is the acceptance
  gate; the 4.47M frames/s debug measurement is one host on one run and is not
  a promise about CI or release hardware. A CI failure here should be read as
  a hardware or contention signal first.
- **The parser queue is deliberately unbounded.** Live surface (4 MiB),
  control frames (16 MiB), and scrollback (50,000 lines) are hard-bounded; the
  off-path parser queue is not, because dropping it makes later snapshots lie
  and blocking it violates the child-first invariant. Backlog and RSS under
  long workloads are soak measurements.
- **Nothing above the seam is daemon-aware yet.** The gateway PTY host still
  spawns `latch attach` and the Hub still polls `capture_pane` /
  `paste_message`. Both work through the selector, but the event-driven
  control-plane path is Phase C (coo:847.5wdg).
- **Pre-existing and unrelated:** `scripts/generate-remote-access-types.py
  --check` was already failing on `main` before this merge. The generated
  Rust and TypeScript differ only in the canonical schema-set SHA-256 comment
  — a schema edit landed without regeneration. It is fixed in its own commit
  beside this merge so the boundaries job is green, and it touches no
  behaviour.

### Rollback path

There are three levels, cheapest first.

1. **Nothing to roll back at runtime.** The default is `tmux`. An operator
   who hits daemon trouble stops passing `LATCH_KERNEL=latchd`; the next
   process is on tmux. No release, no update, no restart of existing
   sessions.
2. **Revert the integration.** The merge is a single no-fast-forward commit:

   ```text
   git revert -m 1 784eada   # "Integrate the latchd headless kernel (Phase A)"
   ```

   That removes both crates, the selector, and the CI steps in one commit and
   returns `main` to a tmux-only build. Nothing else on `main` depends on
   either crate, so the revert is self-contained.
3. **Re-merge rather than re-implement.** The branch ref
   `design-latch-headless-terminal-alternative-847` still points at the
   integrated tip (`99a4803`), and a revert does not remove the commits from
   history, so undoing step 2 is another merge — not a rebuild.

The rollback boundary tightens at Phase B, when the daemon enters the signed
payload, and again at the default cutover; each of those objectives owns its
own rollback and neither is unlocked by this one. This objective does not
flip the default kernel and does not remove tmux.

---

## Part 9 — Phase B four-binary packaging (coo:847.c51g)

Phase B changes distribution, not kernel selection. Both supported CLI targets
(`aarch64-apple-darwin` and `x86_64-apple-darwin`) now build and publish one
coordinated payload containing `latch`, `latch-remote`, `latch-tmux`, and
`latchd`. The default remains tmux, and `LATCH_KERNEL=tmux` remains an explicit
escape hatch.

### Release contract

- `scripts/release-cli.sh` builds `latchd` with the other Rust binaries, signs
  and verifies all four executables, and submits the archive containing all
  four to the existing notarization flow.
- `latch-payload.json` is included in every archive. It binds format version,
  product version, target triple, and the ordered four-binary member list. The
  release script checks each Rust binary's reported version before creating it.
- The archive checksum remains the outer integrity boundary. The installer and
  updater verify that checksum, the manifest, every required member, the
  Developer ID team (when the current install is signed), tmux's private raw
  attach capability, latchd's product/protocol version, and the remote helper's
  product version before changing any installed path.

### Update, repair, and rollback

The updater stages beside the installed CLI so each replacement is a
same-filesystem rename. All four members pass completeness, signature, and
version checks before the first rename. Siblings are backed up during the
transaction; a failure at any later member restores every earlier member, and
`latch` is replaced last. Missing tmux, remote, or latchd members and invalid
tmux/latchd/remote versions turn an otherwise-current update into a repair of
the complete payload.

Replacing a kernel's executable path does not signal, restart, or reconnect a
running kernel. A test starts long-lived processes from both installed kernel
paths, performs the four-binary replacement, and proves both old processes
finish normally while new invocations execute the replacement binaries. This
is the intended update boundary: live tmux servers and per-session latchd
daemons continue from their already-mapped images; only future process starts
use the new files.

### Diagnostics and rollback boundary

`latch doctor` reports the selected kernel plus separate `tmuxVersion` and
`latchdVersion` fields and validates both shipped kernels regardless of which
one is selected. This makes a broken fallback visible while dogfood is using
latchd, and a broken opt-in daemon visible while the default is still tmux.

Rollback is an update to an earlier complete four-binary archive. It does not
terminate live sessions for the same reason a forward update does not. The
operational escape hatch is cheaper: stop setting `LATCH_KERNEL=latchd`, or set
`LATCH_KERNEL=tmux` explicitly; no default changed in Phase B and no live
session migration is attempted.

---

## Part 10 — bounded latchd dogfood soak (coo:847.wff1)

The bounded soak decision is **GO for default cutover**, retaining tmux as the
escape hatch and preserving every existing session on its original kernel.
The full matrix, measurements, defects, fixes, and residual risks are recorded
in [`LATCHD_SOAK_REPORT.md`](LATCHD_SOAK_REPORT.md).

Two soak blockers were fixed before reaching that decision. Existing sessions
now route from durable kernel identity instead of the caller's environment,
eliminating the Desktop/Overlord false-lost split while supporting a genuinely
mixed home. Launch handoff is bounded: an abandoned FIFO or deleted session
directory reaps the per-session daemon and its child instead of leaking them.

The daemon `stat` response now exposes byte-flow, parser/surface backlog,
attach/steal, eviction, and control-failure counters. The measured debug run
delivered 2,000,000 post-boundary bytes exactly once (1.0000x) at 3.40 million
ten-byte frames/s; its parser backlog peaked at 1,733,760 bytes and drained.
Two hundred steals averaged 307 us (676 us max), five hundred snapshots
averaged 737 us (1.364 ms max), and healthy control failures were zero. Three
live Codex sessions remained visible from a tmux-selected caller with zero
lost sessions, 0.0–0.1% sampled CPU, and 2.2–3.0 MiB RSS per daemon.

---

## Part 11 — latchd default cutover (coo:847.qaw3)

Latchd is now the default for **new** sessions: an unset `LATCH_KERNEL` (and
any value other than the explicit fallback `tmux`) selects the per-session
daemon. `LATCH_KERNEL=tmux` remains documented for at least one release window.
The selector is deliberately creation-only. Every existing session keeps the
kernel recorded at creation—legacy records without `kernel.json` remain tmux—so
this cutover neither migrates nor terminates live tmux sessions.

Inspect now reports the owning `kernel` for the individual session, while
doctor continues to report the caller's selected creation kernel and validates
both bundled kernels. Desktop renders the inspect value and the gateway's
existing inspect response carries it through to API clients.

The real PTY cutover test creates a session with no selector, verifies the
persisted latchd identity, then invokes inspect, doctor, and a raw terminal
attach with `LATCH_KERNEL=tmux`. The running latchd session remains visible and
attachable, proving the operational rollback changes only future creation and
does not interrupt a live session. The existing four-binary updater test
continues to prove that replacing either kernel on disk also leaves running
tmux and latchd sessions intact.

---

## Part 12 — event-driven Conversation Hub (coo:847.5wdg)

Phase C moves latchd-owned conversations off the tmux-shaped subprocess path.
Each JSONL connector worker now owns a persistent kernel control connection;
the observation worker also owns a persistent latchd event subscription. A
session has one observation task regardless of subscriber count, and replacing
the WebSocket that happened to start it no longer aborts observation for the
remaining clients.

The event stream is intentionally a wake-up channel, not an authoritative
journal. `output-quiet`, `child-exited`, alternate-screen, title, and surface
events trigger source catch-up and a structured snapshot. Establishing or
re-establishing the subscription is itself a resynchronization event, so the
Hub takes a fresh parser-barrier snapshot rather than guessing which events it
missed. A five-second source-only safety catch-up covers transcript and hook
writes with no corresponding terminal output; it does not reintroduce periodic
screen capture. Primary-screen history is read through latchd's bounded history
verb and combined with the structured current frame for input-safety checks.

`send_message` uses latchd's atomic `submit` request over the persistent
connection. Interactive choices use a structured snapshot followed by one
`key` request. Query failures may reconnect and retry because they are
side-effect-free; submit, paste, and key requests are never retried after
dispatch. The existing Hub operation ledger returns a prior result for a
duplicate operation id and marks an interrupted in-flight operation ambiguous,
preventing duplicate submissions across WebSocket retries and process restart.

Tmux-owned sessions keep the existing 250 ms connector poll plus bounded
`capture-pane`, paste-buffer, and `send-keys` implementation for the fallback
release window. Kernel identity comes from the durable session record, so one
Hub can concurrently serve tmux and latchd conversations without depending on
the gateway process's creation selector.

Event fanout inside latchd is bounded. A subscriber that leaves 1,024 wake-up
events unread is evicted and counted; reconnect plus snapshot is its recovery.
This prevents a stalled chat observer from adding backpressure to the parser or
PTY reader. The parser backlog is independently capped and signals the reader
when it drains, while the raw surface retains its existing slow-client eviction
contract.

The Phase C gate adds or retains coverage for:

- real latchd structured snapshot/history barriers, atomic submit and key,
  output-quiet/title/alternate-screen/child-exited events, and reconnect resync;
- bounded event-subscriber eviction and successful resubscription;
- WebSocket resume after event loss, retained-mutation overflow recovery, and
  observation continuity when the first socket disconnects;
- duplicate operation-id suppression and ambiguous restart recovery;
- real mixed-kernel routing plus the tmux conversation fallback suite.

Reproduce the focused gate with:

```text
cargo build -p latchd
cargo test -p latchd --all-targets -- --test-threads=1
cargo test -p latch --lib conversation:: -- --test-threads=1
cargo test -p latch --test latchd_kernel_e2e -- --test-threads=1
cargo test -p latch --test tmux_kernel -- --test-threads=1
```

The tmux polling path remains deliberately present until the fallback window is
closed by coo:847.adya. Removing it earlier would make existing tmux sessions
observable through terminal attach only and would violate the no-live-migration
cutover contract.
