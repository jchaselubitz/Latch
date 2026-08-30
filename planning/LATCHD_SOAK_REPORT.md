# latchd bounded dogfood soak

Date: 2026-08-30  
Objective: `coo:847.wff1`  
Decision: **GO for the default-cutover objective**, with tmux retained as the
documented escape hatch and no live-session migration.

## Scope and evidence

This was a bounded pre-cutover soak, not an endurance claim. It combined live
dogfood with deterministic real-PTY and real-WebSocket stress cases:

- Three concurrent Codex sessions were hosted by the installed latchd build.
  At the final sample they had run for 14–37 minutes. A tmux-selected build of
  the fixed CLI listed all three as `running`; lost-session count was zero.
- The user had already exercised Claude and Latch session creation manually
  with latchd selected. This run independently exercised the same durable
  create/launch/attach path with Codex plus shell and alternate-screen TUI
  fixtures; it did not start a second paid Claude model solely for the soak.
- The real latch+latchd suite covered create, detach/reattach, retained exit,
  typed-byte identity, control-plane submit/key/snapshot, pinned and
  attach-driven resize, process suspension/resume, abrupt daemon failure, and
  cleanup.
- A real authenticated loopback WebSocket gateway, launched from a
  tmux-selected caller, attached to a latchd session, stole the local surface,
  painted the current silent frame, and was then stolen back with close code
  4409 (`stolen`).
- The four-binary updater test replaced both kernel executable paths while
  live tmux and latchd processes continued to completion.
- Host restart is outside the persistence contract: neither kernel preserves a
  live PTY across an operating-system reboot. Suspension/resume is the relevant
  sleep/wake process boundary and is covered deterministically.

## Measurements

Measurements are from the arm64 macOS development host, debug binaries. They
are acceptance evidence, not release-hardware promises.

| Signal | Result |
| --- | --- |
| Post-boundary byte identity | exact, 2,000,000 / 2,000,000 bytes |
| Byte amplification | 1.0000x |
| Sustained ten-byte frames | 3,395,869 frames/s (70,000 required) |
| Parser backlog, throughput burst | 1,733,760-byte peak; drained to zero |
| Healthy surface queue peak | 7,168 bytes |
| Slow-client case | one eviction after exceeding 4 MiB; child reached `DONE` |
| Repeated surface steals | 200; 307 us mean, 676 us max |
| Structured snapshots | 500; 737 us mean, 1.364 ms max |
| Healthy-run control failures | 0 |
| Healthy-run slow-client evictions | 0 |
| Live daemon CPU | 0.0–0.1% per session at sample |
| Live daemon RSS | 2,224–2,976 KiB per session at sample |
| Live lost sessions through fixed routing | 0 / 3 |

The parser queue remains intentionally unbounded because blocking it would put
the screen model on the child-to-surface hot path and dropping it would make
snapshots lie. The measured burst peak above is visible in `stat` now, and it
drained without affecting the exact 1.0000x surface stream. The bounded surface
queue remains the protection against a slow attached client.

## Defects found and fixed

### Caller-selected kernel produced false lost sessions

`LATCH_KERNEL` used to route every operation in the calling process. Desktop
could inherit `latchd` while Overlord or another terminal created a tmux
session (or the inverse), and list/inspect/attach would query the wrong kernel.
The process was healthy but appeared `lost`.

`LATCH_KERNEL` now selects only the kernel for creation. Every operation on an
existing session routes from its protected `kernel.json`; absence is the
legacy tmux marker. List merges both populations. Mixed-selector CLI and
gateway tests prove list, inspect, attach, drive, resize, stop, and remove.

### Abandoned launch could leak a daemon and child

A creator that died after latchd printed `ready` but before writing the launch
FIFO could leave the launcher blocked forever. latchd now receives that FIFO as
a launch marker, allows a bounded 15-second handoff, and terminates the child,
socket, and kernel record if it never completes. It also exits if its durable
session directory disappears. Both paths have real-process regression tests.

### Soak signals were not observable

The `stat` response now exposes bytes from the child, bytes delivered to live
surfaces, current/peak parser and surface backlog, attaches, steals,
slow-client evictions, and rejected control requests. Fields default to zero
when a new client talks to an older daemon, so protocol version 1 remains
compatible during the dual-binary update window.

## Residual risks and cutover conditions

- This was bounded to tens of minutes, not an overnight or multi-day soak.
  Counters now make a longer field soak inspectable without another code
  change.
- Actual macOS sleep was not forced during an active development session;
  deterministic `SIGSTOP`/`SIGCONT` coverage proves the daemon and snapshot
  survive process suspension, and no wall-clock lease expires while asleep.
- `SIGKILL` necessarily bypasses daemon cleanup. The child loses its PTY, the
  session is reported `lost`, and `remove --force` clears the retained record;
  no silent healthy state is claimed.
- Keep `LATCH_KERNEL=tmux` for at least one release window, preserve existing
  tmux sessions, do not live-migrate, and keep kernel identity visible in
  inspect/doctor during cutover.

With those boundaries, no soak result requires staying on tmux as the default
for new sessions. Proceed to `coo:847.qaw3`.

## Reproducible gate

- `scripts/check-boundaries.sh`: green.
- `cargo fmt --all -- --check`: green.
- `cargo clippy --workspace --all-targets -- -D warnings`: green.
- Linux cross-target check and all-target clippy for latch/latchd: green. The
  cross-check also fixed two test-only sockaddr/openpty portability errors so
  the gateway and PTY harnesses compile on Linux as CI expects.
- `cargo test --workspace --all-targets -- --test-threads=1`: green, including
  136 latch unit tests, 18 tmux exclusive-attach cases, 14 latchd engine cases,
  23 explicit tmux escape-hatch cases, 18 latchd daemon cases, and all terminal,
  transport, updater, gateway, and fixture suites.
- `cargo test --workspace --doc`: green.
- `scripts/generate-remote-access-types.py --check` remains stale on this
  branch for the pre-existing schema checksum-only regeneration already fixed
  on local main as `acbed28`; the generated wire shapes are unchanged and this
  soak did not duplicate that unrelated mainline change.
