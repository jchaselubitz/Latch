# Exclusive attach implementation plan

**Status:** implementation-ready plan; no product code has been changed.

**Source decision:** [`../docs/DECISION_EXCLUSIVE_ATTACH.md`](../docs/DECISION_EXCLUSIVE_ATTACH.md)

**Compatibility policy:** this is a coordinated cutover. There is no old attach
path, protocol negotiation, session migration, or simultaneous support for old
clients. The CLI, Desktop, gateway, mobile contract, and bundled session kernel
ship together.

## Result

Keep packaging tmux, but package a small, explicitly versioned Latch patch on
top of the pinned tmux source. The patch adds one kernel primitive: an
**exclusive hybrid attach**. It atomically:

1. disconnects the previous human surface;
2. adopts the new terminal size;
3. paints tmux's current pane grid and terminal modes once;
4. switches that client to byte-transparent pane output and input; and
5. evicts the client if its output queue becomes slow enough to threaten the
   session.

This keeps the part tmux is good at—PTY ownership, process lifetime, the live
grid, headless draining, and exited panes—without using tmux's CSI renderer on
the live hot path. After the one attach paint, bytes written by Claude, Codex,
or the shell are the bytes written to iTerm or Termius.

Do **not** implement this by composing stock `capture-pane` with a control-mode
`%output` relay. That composition cannot meet the attach contract reliably.

## Product boundary: Latch is not a terminal application

The new attach design does **not** reverse the earlier product decision. Latch
remains a persistent session host and attachment broker. iTerm, Terminal,
Termius, and a future native terminal view remain control surfaces that parse
and paint terminal bytes.

Overlord should keep the two choices orthogonal:

| Choice | Examples | Responsibility |
| --- | --- | --- |
| Execution/session provider | `direct`, `latch` | Who owns the PTY and whether the process survives a viewer |
| Viewer/control surface | iTerm, Terminal, none initially; later a remote terminal | Where the one active human interaction is painted |

The launch sequence remains:

```text
Overlord resolves command + cwd + environment + mission context
  -> latch create --manifest-file -
  -> Latch returns a durable provider session id
  -> optionally latch open --with <preferred viewer>
  -> that viewer runs latch attach <session>
  -> a later viewer may run latch attach and steal the same session
```

This is already the boundary in `planning/OVERLORD_INTEGRATION.md` and in
Overlord's `ExecutionProviderKind` versus `TerminalViewerKind` model. The
manifest contract does not change for exclusive attach.

Collapsing Latch into the terminal/viewer list would lose important states:

- a Latch session can be created headless with no viewer;
- viewer launch can fail while session creation succeeds;
- the preferred desk viewer and the later remote surface are different choices;
- changing or stealing the surface must not relaunch the agent;
- Latch is useful from any terminal and independently of Overlord.

The UI may offer a convenient composed preset such as **Persistent with Latch,
open in iTerm**, but the stored model and launch pipeline remain two-dimensional:
**Run in** `Direct | Latch (persistent)` and **Open with**
`iTerm | Terminal | None`. Latch Desktop may manage sessions, but it does not
become the terminal emulator merely because Latch delegates the active surface.

## Problems the decision did not yet resolve

These are blockers to a naive implementation, not reasons to reverse the
exclusive-attach decision.

### 1. Stock tmux has no atomic "frame, then raw" attach

Control mode exposes the pane's original output bytes, but `capture-pane` and
`%output` are separate observations. Output can arrive between them. There is
no public sequence number that says whether a chunk is already represented in
the capture, so a relay must either drop a chunk or paint it twice. A repeated
chunk is not harmless: it may scroll, ring a bell, change a title, or mutate a
terminal mode.

The handoff therefore has to live inside the session kernel's event loop. The
kernel must serialize the snapshot boundary and the first raw byte.

### 2. `capture-pane` is text, not a complete attach frame

Even with `-e`, `capture-pane` returns cells and attributes. It is not a complete
terminal reconstruction: alternate-screen selection, cursor state, input
modes, mouse/focus modes, saved cursor state, and other xterm state are not all
part of that output. A blocked trust prompt may look right but accept the wrong
key encoding.

The first paint must use tmux's own full client redraw machinery (plus any
mode-coverage fixes proven necessary by fixtures), not a Rust string assembled
from `capture-pane`.

### 3. Terminal-query ownership would otherwise produce duplicate replies

tmux control mode reports queries such as `CSI c` in `%output`, while tmux also
answers them on behalf of the pane. This was reproduced against the bundled
tmux 3.7b: the query appeared in `%output` and tmux injected `CSI ?1;2c` back
into the pane. Forwarding that same query to a real terminal would invite a
second response.

The kernel needs an explicit rule:

- with no raw surface, tmux remains the virtual terminal and answers queries;
- with a raw surface, the real terminal owns query responses and tmux suppresses
  its duplicate reply for output forwarded to that surface;
- the transition is serialized with attach and detach.

Query/reply fixtures must prove that the pane receives exactly one response in
both states.

### 4. A screen model is still required

"Latch is not in the VT path" cannot mean "nothing parses pane output." A
current frame cannot exist unless the session kernel continues updating a
screen model while attached and headless. The implementable invariant is:

> The kernel may parse a copy to retain state, but it must not synthesize,
> transcode, diff, or replay live output after the attach frame.

That is the performance boundary the implementation and benchmarks enforce.

### 5. Exclusive does not make backpressure safe by itself

A backgrounded phone or napped terminal can still stop reading. Letting its
queue grow blocks the pane or consumes memory without bound. Because there is
only one human surface, recovery is simpler than multi-client resync: cap the
raw client's queue, detach it on overflow, continue draining the pane into the
kernel grid, and let the next attach repaint the current frame.

Use two bounds, initially 1 MiB and 2,048 chunks, matching the previously
measured queue envelope. A later measurement may lower them. Overflow never
triggers an automatic snapshot into the same stalled connection.

### 6. Steal ordering and failure behavior need to be contractual

The new surface must not receive live bytes until the old surface can no longer
receive or send them. Conversely, a failed new attach must not evict a healthy
old one. The order is:

1. authenticate/resolve the session and open the new tty;
2. validate the kernel capability and terminal size;
3. commit the steal inside tmux;
4. detach A and stop accepting A's input;
5. resize, enqueue B's full redraw, and establish the raw-byte boundary;
6. acknowledge B as attached.

If steps 1–2 fail, A remains live. After step 3, B owns the surface; a failure
detaches B and leaves the session headless rather than reviving A behind the
user's back.

### 7. Live read-only attach conflicts with the single-surface contract

A read-only live client either becomes a second paint surface, evicts a user
who can answer the prompt, or gives a lower-privileged remote grant the power
to deny control. None is a clean default.

For this coordinated cutover, remove live `--read-only` / `mode=read-only`
terminal attachment. Conversation Hub and `latch inspect` remain the observation
surfaces. Recently exited sessions remain inspectable as a final frame. A later
snapshot-only endpoint can be designed without becoming a live client.

### 8. Terminal dialect is part of the surface contract

The child continues to receive `TERM=xterm-256color`. Every supported human
surface must consume that dialect. The attach repaint must restore all
functional modes exercised by the Claude and Codex fixtures, including kitty
keyboard flags if a current fixture uses them. Cosmetic gaps such as an old OSC
8 span may be accepted only when a fixture proves input remains correct.

## Target architecture

```text
                         non-viewer commands
                  capture / paste / send / inspect
                                 |
                                 v
agent child PTY <----> patched latch-tmux session kernel
                         |  parses a copy into its grid
                         |  owns query replies while headless
                         |
                         +---- one initial grid/mode redraw ----+
                         +---- then original pane bytes --------+--> active tty
                         <---- then original tty bytes ----------+

active tty = local iTerm OR SSH/Termius OR gateway PTY, never two
```

`latch attach`, `latch open`, Desktop, SSH, and the gateway all converge on the
same kernel primitive. Conversation capture and message injection are control
operations, not attached paint surfaces, so they do not participate in steal.

## Kernel contract

Add a Latch-specific attach capability to the bundled tmux binary. The exact
internal flag name is private, but the observable contract is stable:

- `exclusive`: commit detaches the prior Latch raw client;
- `initial-frame`: reset/reinitialize the receiving tty, render the current
  visible pane including alternate-screen state, restore cursor and functional
  modes, then place a strict boundary in the output queue;
- `raw-output`: every pane byte accepted after that boundary is queued unchanged
  to the active tty exactly once;
- `raw-input`: terminal bytes are sent unchanged to the pane; tmux key tables,
  prefix handling, and key-name translation are bypassed;
- `resize-owner`: only the active raw client changes window size; its initial
  size is installed before the frame is rendered and later `SIGWINCH` events
  update the pane;
- `headless`: with no raw client, tmux keeps draining the pane and maintaining
  the grid;
- `bounded`: queued output is bounded by bytes and chunks; overflow detaches the
  client and records `slow_client`;
- `query-owner`: tmux answers terminal queries headless, the attached terminal
  answers them live, never both;
- `reasoned-detach`: normal close, stolen, slow client, session exit, and kernel
  failure are distinguishable to `latch attach`.

The attach transition runs on tmux's server event loop. Pane output processed
before the boundary is represented by the initial redraw; pane output processed
after it is forwarded raw. No separate CLI capture is involved.

## Phase 0 — prove the kernel primitive

Build the patch as a disposable spike before changing Rust call sites.

1. Add an upstream-source manifest with the tmux version, source SHA-256, Latch
   patch level, and patch-file SHA-256.
2. Add a minimal patch that advertises `latch-raw-attach-v1` through a
   machine-readable capability query. Do not rely on `tmux -V` alone.
3. Implement exclusive attach, full initial redraw, the raw boundary, raw
   input, resize ownership, and reasoned detach in the tmux server/client.
4. Implement query ownership and the bounded slow-client eviction before using
   the primitive from Latch.
5. Exercise the patch directly with PTY integration tests.

Phase 0 exit tests:

- a pane blocked after painting a trust prompt writes no more bytes; attach B
  still sees the prompt, cursor, and usable input mode;
- after an explicit test marker, B receives a byte-for-byte copy of randomized
  binary pane output, including split escape sequences and invalid UTF-8;
- randomized terminal input reaches the pane byte-for-byte;
- B stealing from A prevents A input and output before B is acknowledged;
- a failed B preflight leaves A attached;
- resize-before-frame and resize-during-handoff end at B's geometry with no
  duplicate redraw bytes after the boundary;
- DA, DSR, DECRQM, focus, mouse, bracketed-paste, and kitty-keyboard fixtures
  give the pane exactly one response/encoding in headless and live states;
- a client that stops reading is detached at the queue bound while pane output
  and `capture-pane` continue advancing;
- clean close, steal, overflow, and pane exit restore the attach process tty and
  report distinct reasons.

If this phase cannot satisfy all of those properties inside the tmux event
loop, stop. The fallback is a new Latch PTY host that owns a screen model and
passes local tty descriptors with `SCM_RIGHTS`; it is **not** a stock tmux
control-mode relay. Do not land a best-effort capture/`%output` bridge.

## Phase 1 — make the patched kernel reproducible

Files:

- `vendor/tmux/manifest.json`
- `vendor/tmux/patches/*.patch`
- `scripts/build-tmux.sh` (new shared build entry point)
- `scripts/release-cli.sh`
- `.github/workflows/release-cli.yml`
- `scripts/install-cli.sh`

Work:

1. Move source download, checksum verification, extraction, patch application,
   static dependency wiring, and capability verification into one build script
   used by local release builds and CI.
2. Fail the build if a patch applies with fuzz, the source checksum differs, or
   the resulting binary does not advertise the exact Latch capability and patch
   level.
3. Keep the binary name `latch-tmux`; continue signing and notarizing it in the
   same archive as `latch` and `latch-remote`.
4. Update installer and updater completeness checks to verify capability, not
   only the filename and upstream version.
5. Add a documented tmux-update procedure: change the upstream pin, rebase the
   patch without fuzz, run kernel conformance and soak tests, then update the
   manifest hash.

Exit criterion: a clean checkout can reproducibly build a binary that passes
the Phase 0 suite, and an unpatched upstream tmux is rejected by Latch before a
session is created or attached.

## Phase 2 — switch every local attach to exclusive hybrid mode

Primary files:

- `crates/latch/src/engine.rs`
- `crates/latch/src/cli/attach.rs`
- `crates/latch/src/main.rs`
- `crates/latch/src/cli/open.rs`
- `crates/latch/src/cli/manage.rs`
- `fixtures/testing/fake-tmux.py`
- `crates/latch/tests/tmux_kernel.rs`

Work:

1. Replace `engine::attach` / `attach_read_only` with one
   `attach_exclusive` entry point. It invokes the patched attach capability and
   always steals at commit. There is no ordinary `attach-session` fallback.
2. Remove the live `--read-only` flag and its retry branches. `--retry` may
   retry transport startup, but never turns a committed steal into two live
   clients.
3. Preserve terminal raw-mode restoration on normal exit, SIGHUP/SIGTERM,
   steal, slow-client eviction, and pane exit.
4. Replace the current "attached client count became nonzero" first-viewer
   heuristic with the patch's attached acknowledgement. Signal the Overlord
   first-viewer gate only after the initial frame has been accepted by the
   kernel for a real surface.
5. Keep `window-size latest` only as an internal default; assert that at most one
   raw client can influence it. `latch resize --pin` may remain an explicit
   administrative override, but attach must report a pinned-size mismatch
   instead of silently claiming the new tty owns geometry.
6. Change inspect/list wording from generic tmux clients to the product fact:
   `surfaceAttached` and, where useful, `surfaceKind` / detach reason. Do not
   expose internal non-viewer tmux command clients as surfaces.
7. Bump the terminal/session protocol capability. Old kernels and clients fail
   closed with "update the complete Latch payload"; no compatibility path is
   retained.

Local integration tests:

- first local attach, local steal, steal back, and rapid competing attaches;
- closing A at the same instant B steals;
- an away-from-desk trust prompt shown and answered after phone-sized attach;
- pane exit during each attach phase;
- pinned and unpinned geometry;
- bare `latch`, `latch open`, Desktop's launch command, and nested-session guard;
- first-viewer timeout when no attach commits, and release when one does.

Exit criterion: all local human paths use the exclusive primitive, and code
search finds no user-facing ordinary `tmux attach-session` invocation.

## Phase 3 — move the gateway onto the same contract

Primary files:

- `crates/latch/src/cli/serve/terminal.rs`
- `crates/latch/src/cli/serve/pty.rs`
- `crates/latch/src/cli/serve/routes.rs`
- `crates/latch/src/cli/serve/contract.rs`
- `schemas/remote-access/v2/terminal-connection.schema.json`
- generated TypeScript and Swift contracts
- gateway, SDK, and mobile tests

Work:

1. Keep the gateway's PTY wrapper, but make the spawned `latch attach` the same
   exclusive hybrid client as a local terminal. A WebSocket attach therefore
   steals iTerm, and an iTerm attach steals the WebSocket.
2. Remove `mode=read-only`, `readOnlyTerminal`, and the read-only terminal grant
   from the coordinated schema/client update. A terminal connection requires
   control permission.
3. Put a deadline around every WebSocket write. If the peer cannot drain output,
   close it, reap the attach process, and let the kernel remain headless. Do not
   await a socket send indefinitely inside the PTY-read branch.
4. Observe attach-process exit concurrently with PTY reads and socket I/O.
   Translate `stolen`, `slow_client`, `session_exited`, and `kernel_error` into
   stable WebSocket close codes/reasons.
5. Bound pre-attach input and require a valid initial size before commit. No
   unauthenticated or half-initialized socket may evict the desk surface.
6. Ensure a disconnected gateway task always kills and waits for its PTY child;
   no orphan attach remains counted as the active surface.

Gateway integration tests:

- WebSocket steals local; local steals WebSocket;
- an unauthorized or malformed WebSocket cannot steal;
- backgrounded/non-reading WebSocket is closed within the configured write
  deadline and does not stop pane progress;
- output after the first frame is byte-identical and not JSON/base64 expanded;
- resize frames affect the pane only while that socket owns the surface;
- old-owner input racing with steal is rejected;
- every close reason reaps the child and leaves zero or one surface.

Exit criterion: local and remote attach are one behavior, and a stalled phone
cannot block the session or leave a ghost attachment.

## Phase 4 — update product surfaces and documentation together

Primary files:

- `docs/ARCHITECTURE_RULES.md`
- `docs/ITERM_SETUP.md`
- `docs/SSH_SETUP.md`
- `docs/CLI_RELEASES.md`
- remote-access and SDK docs that describe terminal mode
- Desktop/mobile generated models and tests
- `planning/OVERLORD_INTEGRATION.md`
- Overlord provider/viewer settings and launch tests in the sibling resource

Work:

1. Replace multi-client and `window-size latest` product language with the
   exclusive-steal invariant.
2. Document the visible result of a steal: the previous attach exits with a
   reason; its terminal is restored; rerunning `latch attach` steals back.
3. Document that the first paint is a current frame, not scrollback or a PTY
   replay, and that subsequent output is the agent's own stream.
4. Remove read-only live-terminal UI and capability text. Direct users who only
   want to observe to Conversation Hub / inspect.
5. State the supported terminal dialect and Termius/iTerm settings.
6. Answer the packaging question explicitly: `latch-tmux` remains required and
   is now a Latch-patched kernel, not an optional system dependency.
7. Keep Overlord's Latch execution provider separate from its preferred viewer.
   Add a regression test for `provider=latch, viewer=none` and for changing the
   viewer without recreating the provider session. UI copy may present a
   composed preset, but persisted settings must not collapse the two axes.

Exit criterion: no current document or generated client advertises mirrored
attaches, watch mode, or an unpatched tmux kernel.

## Phase 5 — performance, soak, and release gate

Automated performance assertions:

- after the attach boundary, output amplification is exactly `1.00x`: the tty
  receives the same byte sequence the pane wrote, with no kernel repaint unless
  the pane itself writes one;
- a quiet pane produces zero live paint bytes after the initial frame;
- local echo p95 is no more than 2 ms above a direct PTY baseline on the same
  machine under the redraw fixture;
- the active output queue never exceeds 1 MiB or 2,048 chunks;
- after eviction of a stalled client, pane output and current-frame capture keep
  advancing.

Soak matrix:

- 8 hours each of Claude Code and Codex at desk geometry;
- repeated full-screen redraw fixture at 272x59;
- display sleep/wake with iTerm active;
- Termius connection background/foreground and forced network loss;
- 1,000 alternating desk/phone steals;
- agent waiting on directory trust and permission prompts during long idle
  periods.

Record:

- iTerm CPU and main-thread samples versus direct agent execution;
- kernel, CLI, gateway, and agent RSS before/after warmup;
- bytes written by pane versus bytes delivered after the boundary;
- attach frame size and attach-to-interactive latency;
- steals, slow-client evictions, queue high-water marks, and query-owner
  transitions.

Release gates:

- iTerm live-render CPU is within 5% of the direct-agent baseline for the same
  fixture and geometry;
- no monotonic kernel/gateway RSS growth after warmup;
- no lost session, blocked pane, duplicate query response, ghost surface, or
  corrupted terminal in the soak matrix;
- the trust/permission prompt is visible and actionable on every steal;
- all Rust, Swift, TypeScript, schema-freshness, release-build, signing, and
  patched-kernel conformance suites pass.

## Coordinated cutover

1. Stop existing sessions before installing the release. The release does not
   migrate a running upstream-tmux server into raw mode.
2. Install the signed `latch`, `latch-remote`, and patched `latch-tmux` payload
   together.
3. Update Desktop and mobile/client contracts in the same cutover.
4. On first command, reject an old server or unpatched sibling with a precise
   restart/update instruction; never fall back to ordinary attach.
5. Dogfood local iTerm first, then SSH/Termius, then enable the gateway terminal
   endpoint.

Rollback is the previous complete signed payload plus a tmux-server restart.
There is no mixed-version rollback and no promise to preserve sessions across
that rollback.

## Definition of done

- Latch owns a session that continues headless.
- Exactly one human surface is live.
- A new attach always steals after successful preflight.
- The new surface receives the current actionable frame even when the child is
  silent.
- From the boundary onward, pane output and terminal input are byte-transparent.
- A slow or sleeping surface is evicted before it can block the pane or grow
  memory without bound.
- Query responses, resize authority, and steal ordering each have one owner.
- Local terminal, SSH/Termius, and gateway behavior are the same.
- The shipped kernel is reproducible, signed, capability-checked, and still
  packaged as `latch-tmux`.
- Current docs and generated contracts describe only the new behavior.
