# Why attaching is unreliable, and what to do instead

> **Decided.** Option A was chosen, exclusive control was dropped, and a
> transcript-fed chat view became the primary interface. Parts 3 and 4 below are
> the record of how that was reached; the live plan is
> [`ENGINE_PLAN.md`](./ENGINE_PLAN.md).

**Question asked:** tmux creates a session and lets other terminals attach to it
with almost no trouble. Latch does the same thing and is intermittent — sessions
end early, clients disconnect, and moving between windows misbehaves. Is there a
simpler and more robust way?

**Short answer:** yes, and the gap is not effort or polish. tmux is robust
because of two invariants Latch does not hold:

1. **The grid is the only thing a client ever sees.** tmux never hands a client
   raw child output. It renders each client's terminal from its own screen
   model, and repaints from that model on every event that could have
   invalidated it.
2. **Attaching cannot fail.** There is no exclusive-write gate on the connect
   path. Any number of clients attach; any of them can type; `attach -d` is how
   you take over.

Latch built the screen model — the hard part, and it is good — and then left it
off the data path. And it made "who may type" a *connection-time* decision, so
the ordinary act of opening a second window returns an error instead of a
screen. Those two choices produce, between them, most of the reported symptoms.

Everything below is evidence for that claim, a survey of how the other
multiplexers do it, and three options with a recommendation.

---

## Part 1 — What the code actually does

### 1.1 The screen model is not on the data path

`Effect::Snapshot` is produced in exactly one place in the entire worker:
`Registry::attach` (`crates/latch/src/worker/registry.rs:344`). After the
handshake, every client receives **the same raw PTY bytes**, broadcast
identically through the fanout (`crates/latch/src/worker/mod.rs:717`).

So `Effect::Resize` (`crates/latch/src/worker/mod.rs:656`) sets the PTY window
size and the screen-model size — and repaints **nobody**:

```rust
Effect::Resize { size } => {
    child.resize(size)?;
    screen.resize(to_term_size(size));
}
```

Every attached client's display is therefore correct only if the child
*voluntarily* redraws after `SIGWINCH`. A full-screen TUI usually does. A shell
sitting at a prompt does not. Nothing in the worker guarantees it.

The size changes on: a controller attaching at a different size, a controller's
window being resized, a steal, a controller detaching (the D4 revert), and
`latch resize`. **Four of those five leave every client other than the new
controller looking at a screen the worker knows is wrong and never corrects.**

tmux does the opposite at each of those points. `server_redraw_client()` is
called on client attach, on detach, on size recalculation, and on session change
(`server-client.c:110, 137, 475, 2653, 2682, 3224`), and the client is repainted
from the grid. Its per-write path, `tty_write()` in `tty.c`, loops over attached
clients and issues drawing commands to *each client's own tty*, translated
through that client's terminfo at that client's geometry. A client that is
mid-redraw is skipped and gets a full repaint instead — never a partial one.

This is also why Latch's watchers are wrong by construction. A 200-column
watcher attached to an 80-column session is sent bytes that were laid out for 80
columns. Note that `ARCHITECTURE_REVIEW.md` §1.3 specified "watchers letterbox or
pan"; nothing implements that.

### 1.2 The suite already reproduces this, intermittently

`crates/latch/tests/worker_resize.rs::the_resize_command_reaches_the_worker_and_pin_survives_a_takeover`
fails about **1 run in 8** when the file runs under normal parallel load, and
passes 5/5 in isolation. Measured on this checkout at `dd577ed`:

```
worker_resize file: 7 passed, 1 failed out of 8 runs
```

It fails by timing out waiting for the child to print its new size:

```
timed out waiting for "43 132" in session output; saw "…50 200\r\n50 200\r\n…"
```

That is not a flaky test to be stabilized with a longer timeout. It is the
architecture's failure mode showing up in CI: **after a resize, the correctness
of the display is conditional on child behaviour and timing.** In the product the
same condition reads as "changing the window I'm viewing from is buggy."

### 1.3 Exclusive control is enforced at connect time, so attaching fails

`Registry::attach` returns `ControlBusy` when a controller exists and `steal` is
not set (`registry.rs:289`). And `latch attach` asks for control, without steal,
by default (`cli/attach.rs:364`; `main.rs:622` hardcodes `steal: false`).

The client then treats that refusal as a dead link — `control_busy` is mapped to
`Interruption::Transport` (`cli/attach.rs:422`) — and with the default
`RetryPolicy::NONE` it `bail!`s (`cli/attach.rs:284`). The terminal window prints
an error and closes.

Three consequences, all matching the report:

- **Opening a session in a second window fails while the first window is open.**
  LatchDesktop runs exactly `[latch, attach, id]` — no `--steal`, no `--retry`
  (`apps/LatchDesktop/Sources/LatchDesktop/LatchClient.swift:112`).
- **Reattaching right after closing a window is a race** against the worker
  reaping the old attachment. Sometimes it wins. That is the "intermittent."
- **`latch` inside a Latch session can never work.** The nesting guard resolves
  to `AttachToEnclosing` and calls `attach_created_session` (`main.rs:599`),
  which attaches without steal — to a session whose controller is, by
  definition, the enclosing window.

tmux has no equivalent concept. Every attached client can type. Geometry is
resolved by the `window-size` option (`latest` by default; also `largest`,
`smallest`, `manual` — `options-table.c:1805`), which always produces an answer
and never refuses a connection.

The exclusion policy itself may well be right for an agent session. Enforcing it
by **failing the connect** is what is wrong. It converts a policy question ("who
types?") into a transport failure, at the one moment the user is watching.

### 1.4 The local data path has six hops and a resync protocol

Latch, per byte of child output:

```
PTY → Terminal::advance → journal → bounded per-client ring → frame encoder
    → Unix socket → client FrameDecoder → client stdout
```

The bounded ring needs an overflow path, which needs `discard_pending_writes`,
which needs the rule that an in-flight frame must never be truncated or the peer
desynchronizes permanently (see the comment at
`crates/latch/src/worker/socket.rs`), which needs `resync_stalled_clients` to
push a replacement snapshot once per pass. All of that is careful, correct, and
load-bearing — and it exists only because the client process is a byte relay.

tmux, per byte: the client passes its **stdin and stdout file descriptors to the
server over SCM_RIGHTS** (`MSG_IDENTIFY_STDIN` / `MSG_IDENTIFY_STDOUT`,
`client.c:472-475`; received at `server-client.c:2898-2905`), and the server's
`tty_init` writes escape sequences **directly into the client's terminal**. There
is no framing, no per-client queue, no backpressure protocol, and no resync path,
because the client process is not on the data path at all. It exists to hold the
terminal, forward `SIGWINCH`, and be killed.

Latch's framed codec is needed for a WebSocket transport that does not exist yet
(M3+). It is currently paying its full complexity cost on the one transport that
does not need it.

### 1.5 A worker that fails leaves no record and no log

`spawn_detached` sets `.stderr(Stdio::null())` (`worker/spawn.rs:151`).

`run()` returns `Err` for a whole class of failures — a PTY ioctl error inside
`Effect::Resize`, a non-`EIO` read error, a journal open failure, a `write_exit`
failure. On every one of those paths **`finish()` never runs**: no `exit.json` is
written and the socket file is left behind. `derive_state` then burns
`LIVENESS_PATIENCE` (500 ms, `worker/mod.rs:156`) probing a socket nobody is
behind, and reports `Lost`.

From the outside that is exactly "the session ended early," with no diagnostic
anywhere. tmux writes `tmux-server-*.log` under `-v`. Latch has no equivalent,
which is a large part of why the problem reads as "very intermittent and buggy"
rather than as a specific bug.

### 1.6 The journal is dead weight in the hot path

Every PTY chunk is appended to an on-disk journal (`worker/mod.rs:716`).
`Journal::read_all` and `read_tail` have **no callers outside `journal.rs`**. The
only use of the file elsewhere is its mtime, for ordering `latch list`
(`cli/manage.rs:565`).

So it is write amplification on the hot path, plus a fatal failure mode
(`Journal::open` aborts the worker), for a feature the screen model replaced.

---

## Part 2 — How the others do it

| System | Screen state | Client on data path? | Multi-client | Attach can fail? |
| --- | --- | --- | --- | --- |
| **tmux** | full grid per pane | no — server writes the client's tty fd | yes, all can type | no |
| **GNU screen** | full grid | no | yes | no |
| **dtach / abduco** | none — byte tail | yes (thin) | yes | no |
| **zellij** | full grid | yes (relay, like Latch) | yes | no |
| **mosh** | full grid, diffed | yes (state sync) | single client | n/a |
| **Latch today** | full grid, unused after attach | yes (relay) | one writer | **yes** |

**tmux** is the reference implementation of the thing Latch is trying to be, and
the two invariants at the top of this document are the whole of why it feels
solid. Worth knowing: **control mode** (`tmux -CC`) is a documented line protocol
that emits structured `%`-prefixed events, and iTerm2 already consumes it to
render tmux windows as native tabs with native scrollback and search. That is
substantially Latch's Component 3 pitch, shipping today.

**dtach / abduco** (~1200 lines total) keep a byte buffer and, on attach, either
replay it, send `^L`, or send nothing (`dtach/master.c:405-420, 576-578`).
Latch's own `PROJECT_ARCHITECTURE.md` rejects this correctly: byte replay cannot
restore an alternate-screen application, and Claude Code lives on the alternate
screen. Named here only so it is on the record as considered.

**zellij** is the closest Rust reference, but its client is also a relay over an
IPC socket — same shape as Latch. It is not evidence that the relay design is
robust.

**mosh** is the interesting one for later. It owns a screen model and sends the
client whatever diff reaches the target state, over UDP, with predictive local
echo. That is the right answer for M4's cell-network transport, and it is
*exactly the design Latch already built and then declined to use* — the snapshot
machinery in `latch-term` is most of a mosh server.

---

## Part 3 — Three options

### Option A — Adopt tmux as the kernel

`latch` becomes a front end over a private tmux server:

```bash
tmux -S ~/.latch/server -f ~/.latch/tmux.conf new-session -A -s <id> -- <cmd>
tmux -S ~/.latch/server attach -t <id>          # attach
tmux -S ~/.latch/server attach -d -t <id>       # take over
```

A private socket plus an empty config (`status off`, no prefix binding, no
copy-mode keys) means the user never sees tmux; there is no new interface to
adopt, which was the product's founding objection to it.

Latch keeps everything that is actually its product: the CLI contract, the JSON
reports, the `meta.json` sidecar, the Overlord integration, the desktop app, and
the roadmap for components 3 and 4. Web and mobile clients use control mode
rather than a bespoke protocol.

- **Buys:** correct per-client redraw, multi-client attach, a resize policy that
  cannot fail, scrollback, detach semantics, and a structured event protocol —
  all proven, today. M1 and M2 stop being engineering work.
- **Costs:** a bundled dependency (ISC-licensed, ~1 MB, pinnable inside
  `Latch.app`); `$TMUX` in the child environment needs handling for users who run
  their own tmux; and per-byte agent hooks are constrained to what `pipe-pane`
  and control mode's `%output` expose. That last one is the real question and is
  what a spike should answer.

### Option B — Keep the worker, adopt tmux's two invariants

This is **strictly smaller than what exists now** — it deletes more than it adds.

1. **Repaint on every invalidation.** Wherever the registry emits
   `Effect::Resize`, also emit `Effect::Snapshot` to *every* attached client.
   ~20 lines, and it closes the largest hole in Part 1.1. Phase two, if
   differently-sized watchers need to be genuinely correct, is per-client
   rendering out of `latch-term` — a real project, and the only way to match
   tmux's fidelity.
2. **Attaching never fails.** Remove `control_busy` from the connect path. Every
   client attaches; taking control is always satisfiable (last attach wins, like
   `tmux attach -d`), or is a separate request after a successful attach. Keep
   the exclusivity policy if it is wanted — just stop expressing it as a refused
   connection.
3. **Pass the tty fd for local attach.** SCM_RIGHTS the client's stdin/stdout to
   the worker and write the terminal directly. This deletes `queue.rs`, the
   overflow/resync path, and most of the client relay for the local transport.
   Keep the framed protocol strictly for remote and embedded clients, where it
   earns its keep.
4. **Delete the journal. Log worker stderr to a file. Always write `exit.json`.**

### Option C — dtach model

Byte tail plus a redraw method. Rejected, for the reason already written down in
`PROJECT_ARCHITECTURE.md`: it cannot restore the alternate screen, and that is
where the agents live. Listed for completeness.

---

## Part 4 — Recommendation

**Do the small fixes now, then spike Option A for two days, then decide.**

The small fixes are needed under every option and are worth doing this week
regardless:

- [ ] Worker stderr → `~/.latch/sessions/<id>/worker.log` instead of `/dev/null`.
- [ ] Write `exit.json` on the error path too, so a crashed worker reads as
      `exited`-with-cause rather than `lost`.
- [ ] Emit `Effect::Snapshot` to all attached clients alongside every
      `Effect::Resize`.
- [ ] Stop failing an attach on `control_busy` — at minimum, have LatchDesktop
      and `attach_created_session` pass steal, so opening a window always works.
- [ ] Remove the journal from the hot path.

My expectation is that those five convert "very intermittent and buggy" into
"works," which then makes the architecture decision a considered one rather than
a rescue.

Then spike A, because the honest read of the situation is this: **Latch is
reimplementing the tmux server, and the differentiator was never the server.** It
is components 3 and 4 — the conversation view and the agent widgets. M1 and M2
have consumed the reliability budget, and per `docs/M2_FIELD_REPORT.md` the M2
verdict was never even recorded. If tmux can sit behind the existing CLI contract
— and the contract is already the product boundary, which is what makes this
cheap to test — then the entire remaining budget goes to the part nobody else has
built.

Option B is the fallback if the spike shows tmux constrains the agent-extension
story too much. It is a good fallback: it is a subtraction from the current
design, not an addition to it.

---

## Appendix — What is right and should not be traded away

Worth stating plainly, because the above is a critique and the surrounding work
is not bad:

- **The screen model is the correct call and the evaluation behind it
  (`DECISION_EMULATOR.md`) is genuinely good.** The problem is that it is not
  used after attach, not that it exists.
- **No daemon, no database, derived state** is a sound simplification and none of
  the symptoms trace to it.
- **Not storing PIDs, signalling the group from the live worker,
  manifest-over-stdin, sanitizing at ingest** are all right and cost nothing.
- **The effect-returning registry** is the right shape for testing ordering, and
  it is why the two M2 kernel defects were findable at all.

The recommendation is not that this was built badly. It is that the two
invariants tmux holds are load-bearing, Latch holds neither, and holding them is
either a subtraction from this codebase or a reason to stop maintaining a kernel
that already exists.
