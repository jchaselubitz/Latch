# coo:840.dcvj — Why Latch agent jobs run slower than a direct iTerm session

**Superseded recommendation:** the “stop gating EV_READ on `input_parse_pane`”
one-liner at the end is slightly wrong. Use
[`2026-08-23-coo-840-findings-and-recommendation.md`](./2026-08-23-coo-840-findings-and-recommendation.md)
for the corrected recommendation (defer/time-slice parse in `latch-tmux`; not
configuration; not an architecture rewrite).

Diagnosis only; no code changes. Question: agent jobs in Latch feel — and
actually are — much slower than the same agent started directly in iTerm. This
is not only a choppy display.

## Verdict

Yes, it makes sense. The LLM is not slower. The agent process is coupled to a
different stdout path, and coding-agent TUIs (Cursor `agent`, Claude Code,
Codex) treat that path as part of the turn loop: a blocked `write()` delays
the next token, tool, or HTTP chunk.

Direct iTerm drains the child's PTY on a dedicated reader thread and parses VT
later. Latch owns the PTY inside a single-threaded `latch-tmux` server that
must parse every pane byte into its grid before it reads more. That extra
parse-and-forward sits on the agent's blocking TTY, so the session itself
waits.

Exclusive raw-attach already stopped tmux from *rewriting* live CSI onto
iTerm. It did not take Latch off the pane-read hot path. A screen model is
still required for steal/reattach, and that copy is parsed on the same event
loop that drains the child.

## Two different "slow"s (do not collapse them)

| What you see | What is actually slow | Latch-specific? |
| --- | --- | --- |
| Choppy / delayed typing in the window | iTerm main thread parsing VT (`TokenExecutor`) | Shared with direct iTerm; worse when several agent TUIs paint at once (coo:751) |
| The *job* takes longer: gaps between tokens, tools start late, turns crawl | Agent blocked on stdout (or on a terminal query that has to round-trip through tmux → iTerm → tmux) | Yes: tmux parse + extra hop + one server for every Latch session |

The second row is this ticket. coo:751 described the first. They amplify each
other when the viewer is iTerm, but the job-slowness mechanism holds even if
you ignore the window.

## Path comparison

**Direct iTerm**

```text
agent write() → kernel PTY (~64 KiB)
              → iTerm PTYTask thread (memcpy into a large userspace ring)
              → iTerm main thread parses/paints when it can
```

The child is limited by how fast the reader thread drains the kernel buffer,
not by how fast the display paints. Node/Ink-style TUIs get `write()` returned
quickly; the JS event loop keeps streaming the model.

**Latch (current exclusive raw-attach)**

```text
agent write() → pane PTY
              → latch-tmux server (one event loop for every session)
                   • copy new bytes toward the one raw client
                   • input_parse_pane() into the live grid   ← still required
                   • bufferevent_disable(EV_READ) until that work finishes
              → unix socket → latch-tmux -R client
              → iTerm PTY → iTerm parse/paint
```

Live bytes after the first attach frame are the pane's own bytes (`tty_latch_raw_write`),
not tmux's reconstructed CSI. That was the coo:751 follow-up. The remaining
cost is: **parse a copy on the read path, then an extra userspace hop.**

Measured input-echo overhead in `exclusive_attach_e2e.rs` is ≤ 2 ms vs a bare
PTY. That test is a tiny `printf`. It does not exercise a 90×60 (or 270-column)
agent TUI rewriting the grid. Throughput under redraw volume is the issue, not
keystroke latency.

## Why that stalls the agent, not just the picture

Cursor's CLI is Node (`~/.local/share/cursor-agent/.../index.js`). Claude Code
is the same class of program. When stdout is a TTY, a burst of CSI either:

- blocks in `write()`, or
- fills the stream until the TUI awaits `drain`.

That work shares the agent's event loop with `fetch()` for the next model
chunk and with tool execution that is kicked off only after the UI says
"running X". Slow drain → later tools → longer wall-clock turns. That is a
real performance discrepancy, not a rendering illusion.

Terminal queries make it worse. With a raw surface attached, tmux does **not**
answer DA / cursor / mode queries; the real terminal does (see the patch's
`server_client_latch_raw_active` guards in `input.c`). So:

```text
agent CSI query → tmux → iTerm main thread → reply → tmux → agent
```

iTerm on this machine was sampled at **~30% CPU** (TokenExecutor path from
coo:751) while two idle Latch Cursor sessions sat in the background. A TUI
that waits on those replies waits on iTerm's main thread *plus* the extra hop.
Direct iTerm still waits on iTerm, but not on tmux.

## What exclusive attach already fixed (so this is not 2026-08-22)

- No second live tmux CSI renderer onto iTerm after the first frame.
- `tty_block_maybe` is skipped for `CLIENT_LATCH_RAW`, so a slow viewer is not
  allowed to pause the whole server the way stock tmux would.
- Output to the raw client is bounded (1 MiB / 2048 chunks); overflow detaches
  with `slow_client` (exit 76) and the pane keeps being drained into the grid.
- Headless sessions still read the PTY, so a job with no window should not
  block on stdout forever.

Those changes stop *unbounded* backpressure and stop the reconstructed-CSI
tax. They do not make pane reads as cheap as iTerm's memcpy thread, and they
do not give each session its own drain loop.

## Amplifiers

1. **One tmux server for every Latch session.** Live check during this
   investigation: two Cursor jobs (`coo:826`, `coo:837`), both
   `latch-raw` attached, sharing pid 26823. Busy TUIs serialize on that
   process. Direct iTerm: each window's PTY is drained independently.

2. **iTerm is still the viewer.** Overlord Latch launches still `latch open`
   into iTerm running `latch attach`. You pay tmux *and* iTerm. Direct pays
   only iTerm, with drain decoupled from paint.

3. **Conversation Hub** (`crates/latch/src/conversation/connectors/jsonl.rs`):
   while a conversation WebSocket is subscribed, the connector
   `capture-pane`s about every 1.5 s and the hub poll is 250 ms. `capture-pane`
   runs on the same tmux loop. Desk-only use (no Hub subscriber) does not pay
   this. Remote chat open during a heavy turn does.

4. **Cursor panes are not alt-screen-only.** Sampled `history_size` was 81 and
   202 on the two live Cursor sessions. Claude Code's alt-screen panes sit at
   0 (coo:751). Cursor therefore uses the configured `history-limit 50000`.
   That is not the current 200-line problem, but a long primary-screen session
   makes every later parse/`capture-pane` heavier.

5. **Geometry.** These two sessions were ~90×60, not the 272×59 Claude windows
   from coo:751. Wider Claude TUIs multiply per-frame parse cost.

## What is not causing job slowness

- Latch Desktop RSS / `latch list` polling (already gated in coo:758).
- Claude observer hooks: `SessionStart` and `PermissionRequest` only, 1 MB
  bound (`observer.rs`). Not per-token. Cursor is not injected with that
  plugin (`harness_kind` is only `claude` / `codex`).
- Forcing `TERM=xterm-256color` and unsetting `TMUX` (the child should not
  take a "I'm inside tmux" compatibility path).
- First-viewer wait (3 s unannounced, 30 s once `latch open` stamps a
  marker): launch only, coo:772.
- Overlord wrapping the command as `$SHELL -ilc S`: extra shell at start,
  not per-token.
- The 2 ms echo-latency budget: that test is the wrong workload.

## Live snapshot (2026-08-23, this machine)

| Process | CPU | RSS | Notes |
| --- | --- | --- | --- |
| iTerm2 | ~30% | 265 MB | 1h40m up; still the expensive renderer |
| `latch-tmux` server | ~1% | 3 MB | Idle-ish; two Cursor sessions not mid-turn |
| two `-R attach-session` clients | ~0% | 1.3 MB each | `flags=attached,focused,latch-raw` |
| `latch serve` / `latch-remote` | ~0% | small | Remote stack up, not on the local paint path |

`latch list` showed two running Cursor jobs, ~90×60, one client each. That
matches exclusive attach. It does not contradict the throughput story: the
server was not busy at sample time because those jobs were idle (~30 min).

## If we change something later (not done here)

Highest leverage, in order:

1. **Decouple pane drain from VT parse.** Keep the grid for steal/headless,
   but do not let `input_parse_pane` gate `EV_READ`. A dedicated read that
   always copies into the raw-client queue (and a side parse) is the actual
   "parse a copy, don't stall the child" invariant from
   `planning/EXCLUSIVE_ATTACH_IMPLEMENTATION_PLAN.md` §4.
2. **Don't await TUI I/O on the agent's turn loop** — that's inside Cursor /
   Claude, not Latch. Worth knowing: even a perfect splice still meets a TUI
   that may wait on queries answered by iTerm's main thread.
3. **Cap concurrent live agent TUIs / avoid huge widths** — operational, from
   coo:751, still valid for Claude alt-screen redraws.
4. **Hub `capture-pane`** should stay off the default path; if chat is open
   during a turn, consider a cheaper pending-prompt probe than a full pane
   dump every 1.5 s.

A naive "tmux is fine, iTerm is slow" reading of coo:751 under-explains this
ticket. iTerm slowness is real and visual. Latch still sits on the child's
TTY in a way a direct iTerm profile does not.
