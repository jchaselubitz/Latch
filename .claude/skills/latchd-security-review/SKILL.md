---
name: latchd-security-review
description: Security review of the latchd session kernel (crates/latchd) and code that touches it — use before merging any change to crates/latchd, crates/latch-term's public surface, or the latch launcher (crates/latch/src/engine/latchd_kernel.rs), and whenever asked to "security review the kernel", audit latchd, or add a security regression test.
---

# latchd security review

The kernel is the one Latch component that runs as the user with no UI in
front of it, holding a hostile program in a PTY behind a private socket.
The full model, the findings from the first review, and the invariants are
in `docs/LATCHD_SECURITY.md` — read §1 (threat model) and §4 (invariants)
before reviewing; this skill is the working checklist.

## Who is hostile

- **The child** (the program in the PTY) and everything it writes: escape
  sequences, titles, volume. Assume an AI agent that has been prompt-injected.
- **Other users on the host**: `/tmp`, `ps`, pre-planted paths and symlinks.
- **Same-uid clients are trusted to drive the session but not to be
  careful**: malformed frames, absurd sizes, stalled readers, fd exhaustion.

## Procedure

1. **Scope the diff.** `git diff --stat -- crates/latchd crates/latch-term
   crates/latch/src/engine/latchd_kernel.rs`. Anything outside those paths
   is not the kernel; say so and stop unless asked.
2. **Walk the checklist below against every hunk.** For each item ask "can
   the child, a neighbour, or a careless client make this worse than
   before?" Record each answer as a finding (severity, location, scenario,
   fix) or as "checked, unchanged".
3. **Run the suite** and read the security tests that cover the area you
   touched: `crates/latchd/tests/security.rs`, plus the unit tests in
   `paths.rs`, `pty.rs`, `peer.rs`, `protocol.rs`, `render.rs`.
   ```sh
   just security-latchd
   ```
4. **Patch, then pin.** Every finding that gets fixed gets a regression test
   in `tests/security.rs` (integration, against the real binary) or the
   module's unit tests (pure logic). A fix without a test is not finished.
5. **Update the record.** Add the finding to the table in
   `docs/LATCHD_SECURITY.md` §2 and, if it changes an invariant, §4.
6. **Report** with the finding table first, then what was patched, then
   what was deliberately left (and why). Never describe an unpatched
   finding as low-risk without stating the scenario that makes it so.

## Checklist

### Filesystem (`paths.rs`, `daemon::listen`, anything under `/tmp`)
- Is every file/dir created with its final mode (`mode(0o600)`,
  `mkdir(0o700)`, `umask(0o077)` around `bind`)? A later `chmod` is a window.
- Does any operation follow a path that another user could have planted?
  `fs::metadata`, `set_permissions`, `remove_file`, `bind` all follow
  symlinks. Under `/tmp` use `O_NOFOLLOW` + `fstat` and **reject** bad
  state; never "repair" it.
- Is every identifier folded into a path validated first
  (`validate_session_id`)?
- Does a daemon ever replace a socket that a live daemon still owns?

### Peer identity (`peer.rs`, accept loop, every `UnixStream::connect`)
- Does every accepted connection pass `peer::is_same_user` before a byte
  is read?
- Does every client-side connect call `peer::require_same_user` before
  sending anything? (A planted socket must never receive keystrokes.)
- New platform `cfg`? It must fail closed (`Unsupported` → rejected).

### Process and pid (`pty.rs`, `reader_loop`, `shutdown`, `on_terminate`)
- Is `session.exit` recorded and `CHILD_TO_HUP` zeroed under the session
  lock **before** `pty::reap`? (`wait_exit` learns the status without
  reaping; the pid stays unreissuable until then.)
- Does every `kill(-pid)` happen under the session lock after checking
  `exit`? Any new signal path must go through `pty::signal_group`, which
  refuses non-positive pids.
- Between `fork` and `exec`: only async-signal-safe calls, dispositions and
  mask reset (`RESET_SIGNALS`), and **no fallthrough** — a failed `chdir`
  or `exec` must `_exit`.
- Is the daemon single-threaded wherever `umask` or `fork` is used?
- Does every exit path (normal, `Kill`, lifecycle, signal, fatal accept
  error) signal the child and unlink the socket and `kernel.json`?

### Memory and throughput bounds (`daemon.rs` queues, `protocol.rs`)
- Every buffer fed by the child is bounded: surface queue → evict
  (`SURFACE_QUEUE_CAP`), parser backlog → block the reader **outside the
  session lock** (`PARSER_BACKLOG_CAP`), subscribers → evict
  (`EVENT_QUEUE_CAP`). A new queue needs a cap and a counter in `Stat`.
- Every number a client sends that sizes an allocation has a cap:
  frames (`MAX_FRAME`, checked before allocating), dimensions
  (`MAX_DIMENSION`, clamp on attach / refuse on resize / refuse on the CLI),
  scrollback (`SCROLLBACK_LINES`).
- Does any thread block while holding the session lock? Only the
  reader's two queue pushes and short critical sections belong there.

### Child-controlled data leaving the kernel (`render.rs`, events, `Stat`)
- Any string the child authors that an observer will print (`title`) goes
  through `render::sanitize_title`. Cell text comes from the parser and is
  already printable; a new field of child origin needs the same treatment.
- Nothing from the child ever becomes a path, a command, or an argument.

### Panics and errors (`parser_loop`, `parser_query`, `handle`)
- The parser thread runs every item under `catch_unwind` and rebuilds the
  model on panic (`parser_resets`). A new item kind must be covered.
- `parser_query` returns `Result`; no `expect`/`unwrap` on a channel from
  another thread in request handling.
- A malformed or oversized request costs exactly that connection
  (`control_failures`), never the daemon.

### Protocol changes (`protocol.rs`)
- New request fields that size anything: capped and tested.
- New `Stat` fields: `#[serde(default)]` so old clients still parse.
- Bumping `PROTOCOL_VERSION` is the only way to change wire semantics; a
  mismatched attach is refused before any raw bytes flow.

### Launcher (`crates/latch/src/engine/latchd_kernel.rs`)
- Secrets never go on the `latchd` command line (`ps`-visible); `--env`
  carries only `LATCH_SESSION_ID`, the rest rides the manifest FIFO.
- The socket path still comes from `latchd::paths::socket_path` (validated
  id, checked directory).

## Writing a regression test

Integration tests live in `crates/latchd/tests/security.rs` and drive the
real binary through `Daemon::spawn(script)` / `Daemon::launch(Launch { .. })`.
Pattern: set up the hostile condition (a flooding child, a raw
`UnixStream` sending a bad frame, `ulimit -n` in the `prelude`), then
assert three things — the bound held (a `Stat` counter), the daemon still
answers, and the child is still alive (`daemon.child_alive()`). Name the
test after the property, not the bug.

Pure logic (path validation, title sanitizing, frame limits, `waitid`
ordering) goes in the module's `#[cfg(test)]` block.

## Severity guide

- **High**: another uid reads session output or drives a session; the
  child gains anything outside its own uid.
- **Medium**: the child or a neighbour can crash, wedge, or OOM the daemon,
  orphan a session, or make the daemon act on the wrong process.
- **Low**: a careless same-uid client can do one of the above, or a window
  exists that needs a race to exploit.
- **Info**: hygiene with no scenario today.
