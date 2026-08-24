# Latch agent jobs vs direct iTerm — findings and recommendation

**Mission:** coo:840 (diagnosis `coo:840.dcvj`)
**Date:** 2026-08-23
**Status:** recommendation; no kernel change in this write-up

This is the document to use for the next objective. It supersedes the
end-of-diagnosis “stop gating EV_READ” one-liner in
[`2026-08-23-coo-840-agent-job-slowness.md`](./2026-08-23-coo-840-agent-job-slowness.md).
That mechanism was slightly wrong; the recommendation below is the corrected
one.

---

## Recommendation (read this first)

**Do not treat this as a Latch architecture rewrite.** Do not resurrect the
in-process terminal worker, do not add a dedicated PTY reader thread in Rust,
and do not replace `latch-tmux` in order to make agents “run independently of
rendering.” Exclusive raw-attach is the right product contract. The remaining
bug is that the patched kernel still **VT-parses the pane on the only tmux
thread, inside the pane-read callback**, so a busy agent TUI can fill the
kernel PTY buffer and stall the child.

**Do this instead:** a bounded `latch-tmux` patch (patch level 2 on the
existing exclusive-raw kernel) that:

1. Forwards raw pane bytes to the exclusive client immediately (already done).
2. **Time-slices or defers `input_parse_pane`** so grid maintenance cannot
   monopolize the event loop.
3. Still parses soon enough that steal/reattach sees a usable current frame
   (alt-screen, cursor, input modes).
4. Bounds the unparsed input buffer; you cannot drain `wp->event->input`
   until the parser has consumed it.

That is configuration-impossible (no tmux.conf or Desktop setting). It is also
not a major architecture change. It is the invariant already written in
[`planning/EXCLUSIVE_ATTACH_IMPLEMENTATION_PLAN.md`](../../planning/EXCLUSIVE_ATTACH_IMPLEMENTATION_PLAN.md)
§4 — “parse a copy, do not stall the child” — which the current patch does not
fully implement.

**Effort:** days with steal/snapshot fixtures, not a project-shaped rewrite.
**Risk:** a stale current-frame if parse lags a burst at the exact moment of
steal (directory-trust / permission prompts). Tests must cover that, not echo
latency.

A follow-up drafted as “dedicated reader thread so the agent runs independently
of rendering” over-scopes this. iTerm already decouples drain from paint; Latch
needs the **kernel** to decouple **drain from grid parse**. A second thread
inside tmux fights libevent’s single-threaded server. A reader thread outside
tmux is the archived worker. Neither is the next step.

---

## What we observed

Overlord Latch launches (`latch create` then `latch open` → iTerm running
`latch attach`) feel — and can actually be — much slower than the same agent
started as a direct iTerm profile command. This is not only a choppy window.

Coding-agent TUIs (Cursor `agent`, Claude Code, Codex) treat stdout as part of
the turn: a blocked `write()`, a stream waiting on `drain`, or a CSI query
waiting on a reply delays the same event loop that streams the model and
starts tools. The LLM is not slower. The gaps between tokens and tools grow.

Two different slownesses must not be collapsed:

| Symptom | What is slow | Latch-specific? |
| --- | --- | --- |
| Choppy typing / delayed paint in the window | iTerm main thread parsing VT (`TokenExecutor`) | Shared with direct iTerm; worse with several agent TUIs (coo:751) |
| The *job* takes longer | Agent blocked on TTY drain or on a terminal-query round trip | Yes: tmux parse on the pane-read path + extra hop + one server for every Latch session |

coo:751 explained the first. This mission is the second. They amplify each
other when the viewer is iTerm, but job slowness holds even if you ignore the
picture.

Live snapshot on this machine during diagnosis (sessions idle ~30 min, so this
is not a mid-turn CPU proof):

| Process | CPU | RSS | Notes |
| --- | --- | --- | --- |
| iTerm2 | ~30% | 265 MB | Still the expensive renderer |
| `latch-tmux` server | ~1% | 3 MB | Two Cursor jobs, not mid-turn |
| two `-R attach-session` clients | ~0% | ~1.3 MB each | `flags=attached,focused,latch-raw` |

Both jobs were ~90×60 with one exclusive client each. Cursor panes had
`history_size` 81 and 202 (primary screen, so `history-limit 50000` applies).
Claude Code alt-screen panes in coo:751 sat at 0.

---

## Why Latch is slower than direct iTerm

### Direct iTerm

```text
agent write() → kernel PTY (~64 KiB)
              → iTerm PTYTask thread (memcpy into a large userspace ring)
              → iTerm main thread parses/paints when it can
```

The child is limited by how fast the reader thread drains the kernel buffer,
not by how fast the display paints.

### Latch today (exclusive raw-attach)

```text
agent write() → pane PTY
              → latch-tmux server (one libevent thread for every session)
                   • copy new bytes to the one raw client     ← already cheap
                   • input_parse_pane() into the live grid    ← on this thread
                   • bufferevent_disable(EV_READ) until the
                     next server_client_loop
              → unix socket → latch-tmux -R client
              → iTerm PTY → iTerm parse/paint
```

Live bytes after the first attach frame are the pane’s own bytes
(`tty_latch_raw_write`), not tmux’s reconstructed CSI. Exclusive attach
already:

- stopped the live CSI rewrite that made iTerm parse a heavier stream
- skipped `tty_block_maybe` for `CLIENT_LATCH_RAW`
- bounded the raw client queue (1 MiB / 2048 chunks) and evicted with
  `slow_client` (exit 76)
- kept reading the PTY when headless, so a job with no window should not
  block on stdout forever

Those changes do **not** make pane reads as cheap as iTerm’s memcpy thread,
and they do not give each session its own drain loop.

### What actually gates the child (correction)

tmux 3.7b `window_pane_read_callback` (patched `window.c`):

1. Forwards new data to pipe / control clients / the Latch raw client.
2. Runs `input_parse_pane` **to completion** on that callback.
3. Calls `bufferevent_disable(EV_READ)`.
4. Later, `server_client_check_pane_buffer` turns `EV_READ` back on for any
   non-control client, which includes latch-raw.

The disable is a one-loop debounce plus flow control for tmux control-mode
clients. For Latch it is already re-enabled every loop. **Deleting
`bufferevent_disable` would not fix job slowness.** The stall is step 2: the
only tmux thread sits in the VT parser while the kernel PTY buffer fills and
the agent blocks.

You also cannot drain `wp->event->input` until the parser’s
`wp->offset` has consumed those bytes. A deferred parse must retain the
buffer and bound it.

Terminal queries are a second stall: with a raw surface attached, tmux does
not answer DA / cursor / mode queries (patch guards in `input.c`). The query
goes to iTerm’s main thread and back. Direct iTerm still waits on iTerm; Latch
adds the tmux hop.

### What is not causing it

- Latch Desktop RSS / `latch list` polling (coo:758).
- Claude observer hooks: `SessionStart` and `PermissionRequest` only; Cursor
  is not injected (`harness_kind` is `claude` / `codex` only).
- `TERM=xterm-256color` and unsetting `TMUX`.
- First-viewer wait (coo:772): launch only.
- Overlord `$SHELL -ilc` wrapper: extra shell at start, not per-token.
- The exclusive-attach echo test (p95 ≤ 2 ms vs a bare PTY): wrong workload.
  That is a tiny `printf`, not a 90×60 or 272×59 agent TUI.

Amplifiers, not root cause: all Latch sessions share one tmux server;
Conversation Hub `capture-pane` ~every 1.5 s **only while a conversation
socket is subscribed**; wide Claude alt-screen geometry.

---

## Scope of the recommended change

### Not configuration

Nothing in generated `tmux.conf`, Latch Desktop, or Overlord launch settings
can move `input_parse_pane` off the read callback.

### Not a product architecture change

Keep:

- private patched `latch-tmux` as session kernel
- exclusive steal, one human surface
- first frame from the grid, then raw pane bytes
- slow-client eviction
- Conversation Hub as the observe path (not a second live TTY)

Do not:

- rebuild `latch-term` / an in-process emulator on the live path
- add a dedicated PTY reader thread in the CLI or a sidecar process
- try to make tmux itself multi-threaded (libevent server is one thread)
- wait for Cursor/Claude to stop blocking their turn loop on TTY I/O
  (true, out of our control, and insufficient by itself)

### What to implement

A second Latch patch on pinned tmux 3.7b, same packaging path as
`patches/tmux/0001-latch-exclusive-raw-attach.patch`.

**Target behavior**

- Raw forward stays first and is not delayed on parse.
- Grid parse runs in bounded slices or on an idle/deferred turn of the same
  event loop, never as an unbounded `input_parse_pane` of the entire burst
  before the next pane `read()`.
- Unparsed bytes in `wp->event->input` are capped; overflow should drop parse
  lag by catching the parser up (or evicting a slow *viewer*, which already
  exists) — never stall the child to protect an unbounded grid backlog.
- Before painting the exclusive-attach snapshot, the parser must be caught up
  for that pane so steal still shows the trust/permission prompt that is on
  screen *now*.
- Existing reasoned exits (`stolen` / `slow_client` / `session_exited`) stay.

**Tests that matter** (the 2 ms echo test is not sufficient):

- High-rate pane writer keeps advancing on disk while a raw client is
  attached (already have a shape for this in `exclusive_attach_e2e.rs`;
  extend so parse load is realistic CSI, not `tr '\\0' 'y'`).
- Steal during a redraw burst still restores alt-screen, cursor, and modes.
- Output amplification after the attach boundary stays 1.00× (raw bytes).
- Quiet pane still produces zero live paint after the initial frame.
- Slow-client eviction still leaves the pane progressing and the next attach
  getting a current frame.

**Where the code lives**

- `window_pane_read_callback` / `input_parse_pane` in patched `window.c` /
  `input.c`
- `server_client_check_pane_buffer` must account for a parser that can lag
  the raw-client offset
- Snapshot boundary in `server-client.c` (`LATCH_RAW_SNAPSHOT` →
  `LATCH_RAW_LIVE`) must flush parse before the first frame

---

## Suggested sequence

1. **Kernel patch (this recommendation).** Unblocks the child under TUI
   redraw volume. Highest leverage inside Latch.
2. **Throughput fixture** at desk geometry (and 272×59) so we do not regress
   into “echo is 2 ms therefore we are fine.”
3. **Operational:** cap concurrent live agent TUIs; avoid huge widths for
   Claude alt-screen. Already true from coo:751; still true after (1).
4. **Hub `capture-pane`:** keep it off the default path; if chat is open
   during a turn, a cheaper pending-prompt probe would help the shared loop.
   Not the first patch.
5. **Agent-side TUI drain** (Cursor/Claude) is out of repo. Even a perfect
   splice still meets a TUI that may wait on queries answered by iTerm.

---

## Decision for the next objective

If the next objective is framed as “dedicated reader thread / independent of
rendering,” narrow it to the kernel patch above. The diagnosis does **not**
require a new Latch architecture. It requires finishing the performance
boundary exclusive attach already claimed.
