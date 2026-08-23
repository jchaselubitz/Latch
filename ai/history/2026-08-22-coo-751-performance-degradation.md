# coo:751.4pfr — Latch performance degradation investigation

Deep investigation of terminals that lock up or type-lag after 10 minutes to a few hours, and of sessions that flip to `lost` in Latch Desktop. Measured on this M1 Pro (32 GB) while Latch Desktop, remote access, iTerm2, and several live Claude/Cursor sessions were running.

## Verdict

This is not a Latch process memory leak. Latch Desktop, `latch-tmux`, `latch-remote`, and `latch serve` are small and stable after a day of uptime. The freeze is a **rendering / backpressure** problem: iTerm2 spends its main thread parsing huge Claude/Codex alternate-screen redraws, so keystrokes wait behind VT work. System memory pressure from long-running agents (and Overlord/Cursor/Arc) makes that worse. Inactivity contributes because the display sleeps while the machine stays awake, and a slow or backgrounded remote client can stall tmux for the local window too.

`lost` in the desktop app is a **separate derived state**: metadata on disk, session absent from the private tmux server. It is not "the UI got slow." A tmux server restart, a killed session, or a listing that drops half-expanded rows will show it.

The truncated note on the objective ("it seems to be a rendering issue because") is correct.

## What was measured on this machine (2026-08-22 ~11:35)

| Process | Uptime | RSS | Notes |
| --- | --- | --- | --- |
| Latch Desktop | 25 h | 89 MB | Idle CPU. FD count 110. No leak signature. |
| `latch-remote` | 25 h | 7.5 MB | Listening `*:54791`. No live TCP clients. |
| `latch serve` | 25 h | 4 MB | Loopback gateway only. |
| `latch-tmux` server | 53 min | 3.3 MB | ~2% CPU. History 0/50000 on Claude panes. |
| iTerm2 3.6.11 | 13 h | 137 MB | **18–28% CPU**, main thread in `TokenExecutor` / `VT100Screen`. |
| Claude (3 live) | 40–56 min | 160–321 MB each | Alt-screen TUI, `history_size=0`. |
| Cursor agent (this session) | 30 min | 251 MB | 26% CPU while investigating. |
| Overlord Helper (Renderer) | 17 h | **1.4 GB** | Largest single process. |

System: 32 GB RAM, ~125 MB free, **~14 GB compressed**, hundreds of thousands of swapouts. Display sleep is 5 minutes; system sleep is held off by `caffeinate`, Arc WebRTC, ChatGPT, and powerd. TCP keepalive idle is **2 hours**. Latch takes no power assertion of its own.

Four live Latch sessions, all `running`, one tmux client each, geometry **272×59**. Session directories total 52 KB. `latch doctor` is clean. No Latch/tmux crash reports.

The tmux server is 53 minutes old while Latch Desktop has been up for 25 hours. Whatever sessions existed before that server start are gone from tmux; only the four current metadata directories remain.

## Symptom 1 — terminals lock up / delayed typing

### Primary: iTerm2 is the renderer, and it is on the hot path

`latch attach` execs into `latch-tmux attach-session`. Bytes go:

iTerm2 PTY → tmux client → Unix socket → tmux server → pane (Claude/Codex/agent) → and back.

A 5-second `sample` of iTerm2 showed the main thread in `TokenExecutorImpl` / `VT100ScreenMutableState tokenExecutorSync` / `PTYSession screenSync`, including iTerm's own `slownessDetector`. AppKit delivers keystrokes on that same main thread (`nextEventMatchingMask`). When VT parse+sync is busy, typing feels delayed or dropped even though the pane process is healthy.

Claude Code and Codex run on the **alternate screen** and rewrite the visible grid rather than appending. tmux `history_size` is 0 on those panes, so Latch's `history-limit 50000` is not filling RAM for agent sessions. The cost is **live redraw rate × cell count**. At 272×59, one full-screen paint is ~16k cells; three Claude TUIs plus a Cursor agent, all marked `focused` by tmux, keep iTerm in that loop. iTerm scrollback is 2000 lines (not unlimited), so this is not an unbounded LineBuffer leak.

This matches "fine at launch, worse after 10+ minutes": agents spend the first minutes on startup UI, then enter a steady redraw loop as the conversation and tool panels grow.

### Amplifier: host memory pressure, not Latch RSS

Long-running Claude/Codex processes grow (160–321 MB RSS here; more over multi-hour turns). Cursor agents, Overlord's renderer (~1.4 GB), Arc, and ChatGPT push the 32 GB M1 Pro into the compressor. Under that, iTerm's VT work and tmux IPC hitch. This is the "something that requires computer resources accumulating" — it is **agent and Electron RSS**, not Latch's daemon.

Claude transcripts on disk are modest (hundreds of KB per live session; ~98 MB under `~/.claude/projects`). They are not the freeze.

### Amplifier: inactivity while the machine stays reachable

`pmset`: `sleep 0` (prevented), `displaysleep 5`, `powernap 1`, `ttyskeepawake 1`. The Mac remains on the network; the display still sleeps.

When the display sleeps, iTerm windows are not visible. App Nap / GPU idle can stop the terminal from draining its PTY. tmux still has those clients attached. A client that does not read will back-pressure the server; **every attach to that session stalls**, including the window you later try to type into. Waking the display then dumps a VT backlog into iTerm's already-busy TokenExecutor.

Latch itself never takes an `IOPMAssertion` / `NSProcessInfo.beginActivity`. Remote access can stay "on" while the helper is napped or the display is asleep.

### Amplifier: remote attach has no output backpressure (code)

`latch serve` terminal WebSockets spawn `latch attach` on a PTY and `await` every `socket.send` of pane output (`crates/latch/src/cli/serve/terminal.rs`). There is no bounded queue, snapshot resync, or slow-client drop. The old `latch-term` worker that did that was deleted in the tmux-kernel swap.

A phone that backgrounds, stalls on cell, or keeps WebSocket pings without reading terminal bytes can block:

pane stdout → tmux → attach client → PTY → WebSocket send → encrypted proxy write.

The LAN proxy's idle timeout is **30 minutes of no reads** (`PROXY_IDLE_TIMEOUT`). It does **not** fire if Claude is still producing output (outbound reads keep succeeding until the TCP send buffer fills) or if the phone still pings (inbound is not idle). macOS `net.inet.tcp.keepidle` is 7,200,000 ms (2 hours) and `always_keepalive` is 0. A half-open remote client can stall a session for tens of minutes to hours — the user-reported window.

Audit log: 63 `connection_opened`, 14 `connection_rejected`, **0 `connection_closed`**. Clean closes are not audited, so this is not proof of leaked connections, but it also means there is no trail of idle-timeout teardown. Helper start/stop is chatty (36 listener starts / 35 stops): supervision restarts drop live remote sockets; they do not restart tmux.

xterm.js in `@latch/terminal-react` has the same shape: `onData` → `renderer.write` with default scrollback 1000 and no `write` drain. Latch Desktop does not embed xterm; this hits mobile/web if those views stay open.

### What is not causing the freeze

- Latch Desktop 5s/60s `latch list` polling (coo:758 already gated `@Published` churn). FD and RSS are stable.
- Conversation Hub 250 ms poll + `capture-pane` every 1.5 s — **only while a conversation WebSocket is subscribed**. Serve currently has ~15 FDs; no extra attaches. Claude observer hooks are only `SessionStart` and `PermissionRequest`, each bounded to 1 MB, append-only sidecar. Not a per-token leak.
- tmux 50k history on these agent panes (size 0). It **would** matter for a long primary-screen shell (`cargo test`, etc.).
- Unbounded conversation journals (2 MB / 10k records, then compact).

## Symptom 2 — sessions appear `lost` in Latch Desktop

`lost` means `~/.latch/sessions/<id>` metadata exists and tmux `list-sessions` / `display-message` does not return that id (`engine.rs` `SessionState::Lost`, `manage.rs` list/inspect). Desktop only displays what `latch list --json` says.

True causes:

1. **tmux server gone.** The first `new-session` is the daemon. If it exits (last session killed, crash, explicit kill), the next `latch create` starts a **new** empty server on the same socket. All previous ids become `lost`. Observed: Desktop 25 h up, tmux server 53 min up, only four current session dirs. That is consistent with a server replacement earlier today, not with a UI bug.
2. **Session removed from tmux but not pruned** (force-kill, `kill-server`, leftover metadata).
3. **False lost from partial rows.** `list` retries only 3 times at 50 ms. A row tmux has not fully expanded is dropped; metadata without a live row renders as `lost` for that poll. Under load this can flicker.

Not the same as a hung `latch list`: Desktop's client timeout is 20 s, two consecutive failures before a banner, and **the last good snapshot is kept**. A timeout shows an error, not a mass flip to `lost`. A later successful list against an empty/new server *does* flip everything to `lost`.

Remote helper flaps and presence expiry make the **phone** think the Mac is gone. They do not, by themselves, mark desktop rows `lost`. Control-plane keychain items are `WhenUnlockedThisDeviceOnly`; a locked, idle Mac can fail presence publish while sessions are still running locally.

## Effect of multi-hour Claude Code / Codex sessions

| Layer | Grows with session length? | Effect on the freeze |
| --- | --- | --- |
| tmux pane history (alt screen) | No (`history_size=0`) | None for these harnesses |
| Agent process RSS / context | Yes | Host swap, every app hitch including iTerm/tmux |
| TUI redraw complexity | Yes (more transcript on screen) | Directly feeds iTerm TokenExecutor |
| Latch observer sidecar | Only start + permission hooks | Negligible |
| Conversation Hub | Only if a client is subscribed | Then `capture-pane` + JSONL tail every 250 ms contends with tmux |
| `~/.claude` JSONL | Yes, slowly | Hub would read 2 MB/poll if subscribed; disk itself is fine |

Codex/Claude as local apps (ChatGPT.app renderer, Cursor) add more RSS on the same 32 GB, independent of Latch.

## Environment (M1 Pro, "always reachable")

Reachable ≠ interactive. Sleep is disabled; display sleep and App Nap still run. Arc holds a WebRTC no-idle-sleep assertion; Latch does not. After minutes of no input, the likely sequence is: display sleeps → iTerm stops draining PTYs → tmux back-pressures → you return, type, and wait while iTerm catches up and memory decompresses.

## Recommended fixes (not done in this investigation)

1. **Slow-client isolation (highest leverage for remote + inactivity).** Stop awaiting unbounded WebSocket/PTY writes. Bound the output queue; on overflow, disconnect that client (or snapshot-resync) instead of blocking tmux. Same idea the deleted `latch-term` worker had.
2. **Shorter, write-aware remote timeouts.** Idle-timeout on stalled **writes**, not only silent reads. Log `connection_closed`. Consider TCP keepalive on the LAN listener. Take a prevent-sleep assertion while remote access is enabled.
3. **Desktop `lost` honesty.** If `list` fails or tmux is unresponsive, keep last snapshot and say "tmux did not answer" rather than painting `lost`. Retry partial rows longer than 150 ms. Surface attached-client count in inspect so extra remote attaches are visible.
4. **Lower default `history-limit`** for non-agent shells (50k is a footgun for `latch` as a login shell). Agent panes already use none.
5. **Host guidance.** Cap concurrent live agent TUIs; avoid 270-column maximized iTerm for Claude; Overlord renderer RSS is the largest process on this Mac and should be treated as part of the same pressure budget.
6. **Doctor metrics.** `latch doctor` should report tmux server age, client count, `history_size`, list latency, and helper uptime so the next incident is not a from-scratch forensic.

## Code map

- tmux config / `history-limit 50000` / `window-size latest`: `crates/latch/src/engine.rs`
- `lost` derivation: `crates/latch/src/cli/manage.rs` `list` / `inspect`
- Partial-row retries: `engine::list` / `inspect` (3 × 50 ms)
- Terminal WS relay (blocking send): `crates/latch/src/cli/serve/terminal.rs`
- Remote proxy idle 30 min: `crates/latch/src/cli/remote_access.rs` `PROXY_IDLE_TIMEOUT`
- Desktop poll + 20 s CLI timeout: `apps/LatchDesktop/.../SessionStore.swift`, `LatchClient.swift`
- xterm write with no drain: `packages/terminal-react/src/xterm.ts`, `packages/client/src/terminal.ts`
- Conversation poll 250 ms: `crates/latch/src/cli/serve/conversation.rs`
- Claude hooks (SessionStart, PermissionRequest only): `crates/latch/src/observer.rs`
