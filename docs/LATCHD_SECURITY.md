# latchd security: threat model, review findings, invariants

`latchd` is the session kernel: one process per persistent terminal session,
holding a child in a PTY and a private unix socket. It is the one component
that runs as the user with no UI in front of it, so it is where a mistake
turns into a foothold. This document is the record of the first full review
(2026-08-30), the model it was reviewed against, and the invariants that
`crates/latchd/tests/security.rs` pins so they stay true.

The companion skill for future reviews is
`.claude/skills/latchd-security-review/SKILL.md`.

## 1. Threat model

Three parties can reach the kernel, and they are trusted differently.

| Party | Reaches the kernel through | Trust |
|---|---|---|
| **Same-uid processes** (`latch`, the desktop app, the remote gateway, scripts, and the session's own child) | The unix socket | Trusted to *drive* the session — that is the product. Not trusted to be careful: a confused client must not be able to take the daemon down or size it into the gigabytes. |
| **The child program and its descendants** | The PTY (bytes it writes), the filesystem it shares with the user | **Hostile.** Latch exists to run AI agents and arbitrary programs. Everything the child writes — escape sequences, titles, volume — is attacker-controlled input to the screen model, the event stream, and every observer that renders a snapshot. |
| **Other users on the host** | `/tmp`, the process table, signals to their own processes | **Hostile.** They can pre-create paths in `/tmp`, plant symlinks, see argv in `ps`, and connect to any socket whose file mode lets them. They cannot read the user's home, session directory, or socket directory once those are `0700`. |

What the kernel must guarantee:

1. **Confidentiality.** Nothing the kernel creates (socket, `kernel.json`,
   `exit.json`) is readable or connectable by another uid, at any instant.
   Every accepted connection is checked against the peer's credentials, and
   clients check the daemon's credentials before sending a keystroke.
2. **Integrity of the session.** Only the real child is ever signalled.
   The program runs where it was told to or not at all. The screen model
   cannot be corrupted into misreporting by output the observers do not see.
3. **Availability against the child.** No byte the child writes can grow
   the daemon's memory without bound, crash it, or hang the control plane.
   The live surface is never slowed by the model; a surface that cannot
   keep up is evicted, never waited on.
4. **Availability against careless clients.** Malformed frames, oversized
   frames, absurd dimensions, descriptor exhaustion, and stalled observers
   cost at most that client's connection.
5. **No orphaned children.** If the daemon ends for any reason, the child is
   signalled and the socket and record are gone; a session never runs on
   behind a path nothing points at.

Out of scope for the kernel (owned elsewhere): the session directory's
`0700` mode and the launch manifest FIFO (`latch`), the escape-sequence
parser's own correctness (`latch-term`), and everything past the socket
(the remote transport has `docs/REMOTE_ACCESS_THREAT_MODEL.md`).

## 2. Findings and fixes

Severity is for a single-user workstation running untrusted agents; on a
shared host the `/tmp` findings rank higher.

| # | Severity | Finding | Fix | Regression test |
|---|---|---|---|---|
| 1 | Medium | `paths::socket_dir` created `/tmp/latchd-<uid>`, then `chmod`ed and `stat`ed it **through the path**. Both follow symlinks, so another user who pre-planted `/tmp/latchd-<uid> -> <any dir the victim owns>` would have the victim's daemon `chmod 0700` that directory and put its sockets there. | `mkdir(0700)` (mode applied at creation), then `open(O_DIRECTORY\|O_NOFOLLOW)` and `fstat` the descriptor: must be a directory, owned by the uid, with no group/other bits. A failing directory is **rejected, never repaired**. | `paths::tests::socket_dir_rejects_*` |
| 2 | Medium | The parser queue was unbounded. A child writing faster than the screen model parses (`yes`, `cat /dev/urandom`) grew it until the OS killed the daemon. | `PARSER_BACKLOG_CAP` (32 MiB): the reader waits, outside the session lock, until the parser drains below it. The PTY buffer then fills and the *child* blocks, as it would on a slow terminal. Surfaces are fed before the wait and are unaffected. | `child_output_flood_is_bounded_by_backpressure` |
| 3 | Medium | Any `accept` error other than `EINTR` returned from `run`, exiting the daemon **without** signalling the child or removing the socket. `EMFILE` — reachable by an observer opening enough connections under a login shell's 256-fd soft limit — orphaned the session behind a stale socket and a `kernel.json` pointing at a dead pid. | Transient errors (`EMFILE`, `ENFILE`, `ENOBUFS`, `ENOMEM`, `ECONNABORTED`) back off and retry; a broken listener runs `shutdown` (child HUPed, socket and record removed). The soft `RLIMIT_NOFILE` is raised to the hard limit at start. | `descriptor_exhaustion_does_not_end_the_session` |
| 4 | Medium | A panic in the screen model on child output killed the parser thread for good; every later control request then hit `expect("parser thread is alive")` and panicked its connection thread, while the reader kept pushing into a queue nobody drained. One malformed byte sequence would have been a permanent denial of the control plane. | Each parser item runs under `catch_unwind`; a panic rebuilds the model empty at the current size, bumps `parser_resets`, and the session continues (the surface path never depended on the model). `parser_query` returns `Result`; a failed query is an error for that request only. | `random_bytes_from_the_child_do_not_take_the_kernel_down` (asserts `parser_resets == 0`, so a `latch-term` panic surfaces as a test failure rather than being silently absorbed) |
| 5 | Low–Med | Three pid-reuse windows around exit: (a) `Signal` checked `exit` and then called `kill(-pid)` without a lock; (b) the `SIGTERM` handler kept HUPing the child's pid after it was reaped; (c) the reader reaped the child (`waitpid`) *before* recording the exit, so for that window a reissued pid could be signalled as if it were the child. | `pty::wait_exit` uses `waitid(WEXITED\|WNOWAIT)` to learn the status while the child is still a zombie (pid unreissuable); the exit is recorded and the pid reaped under the session lock; `CHILD_TO_HUP` is zeroed there; `Signal` and `shutdown` check-and-kill under the same lock; `signal_group` refuses non-positive pids and negative signals. | `pty::tests::wait_exit_reports_before_reaping`, `signal_is_refused_once_the_child_has_exited` |
| 6 | Low | The socket was created with the daemon's umask (typically `0755`) and `chmod 0600`ed afterwards — a window in which it was world-connectable (the peer check was the only line). | `bind` runs under `umask(0077)`; the mask is restored before the child is spawned. | `socket_and_records_are_owner_only` |
| 7 | Low | A second daemon pointed at a live socket path silently unlinked it and took over, stranding the first daemon's child with nothing that could reach it. | `listen` probes the path; a live peer refuses the start with "another session kernel is already listening". Only dead socket files are replaced. | `a_live_socket_is_not_hijacked_by_a_second_daemon` |
| 8 | Low | Event subscribers were unbounded `mpsc` channels. A child alternating titles into a daemon with one stalled observer grew that channel forever. | `sync_channel(EVENT_QUEUE_CAP)`; a full subscriber is dropped (`subscriber_evictions`) and must reconnect and resnapshot. | `daemon::tests::stalled_event_subscriber_is_evicted_and_can_resubscribe` |
| 9 | Low | `cols`/`rows` were accepted up to `u16::MAX` from attach, resize, and the command line. Every screen row is stored at full width and scrollback keeps 50 000 rows, so a dimension is a memory multiplier (65535 × 65535 cells is 4 G cells). | `MAX_DIMENSION` (2048): attach clamps, resize refuses, the CLI refuses. | `dimensions_are_bounded_on_resize_and_attach`, `command_line_rejects_unsafe_ids_and_dimensions` |
| 10 | Low | The child-set window title was reported verbatim in `stat`, `title-changed` events, and JSON snapshots. An `OSC 0` payload can carry control characters, so a child could inject escape sequences into whatever terminal, tab bar, or log an observer prints the title into. | `render::sanitize_title`: control characters (C0, DEL, C1) removed, length capped at `MAX_TITLE_CHARS`. | `titles_from_the_child_are_display_text_only` |
| 11 | Low | The session id was folded into the socket file name unvalidated; `--id ../../x` would place (and `remove_file`) the socket anywhere the user can write. | `paths::validate_session_id`: `[A-Za-z0-9_-]`, 1–64 bytes, enforced in `socket_path` and the CLI. | `paths::tests::socket_paths_reject_ids_that_could_escape_the_directory`, `command_line_rejects_unsafe_ids_and_dimensions` |
| 12 | Low | `kernel.json` was created with the umask mode and `chmod`ed after; `exit.json` was never restricted. Neither is secret, but a stale temp file from a crash kept its old mode through `create(true)`. | `paths::write_json`: `create_new` with `mode(0600)`, stale temp removed first, renamed into place. | `socket_and_records_are_owner_only`, `paths::tests::write_json_replaces_a_stale_temp_file_and_its_mode` |
| 13 | Low | The forked child reset only four signal dispositions and never its mask. `SIG_IGN` and a blocked mask survive `exec`, so whatever the daemon's ancestor (a launcher, an app, a service manager) had ignored or blocked reached the user's shell. | Thirteen dispositions reset to `SIG_DFL` and the mask emptied between `fork` and `exec`. | none automated (needs a launcher with a hostile mask); documented in `pty::RESET_SIGNALS` |
| 14 | Low | A `chdir` failure printed a warning and **execed anyway** in the daemon's own cwd. For an agent whose commands are cwd-relative, running in the wrong directory is the hazard. | Fail closed: the child writes "cannot enter the session directory" to its terminal and exits `126` (`EXIT_BAD_CWD`). | `a_cwd_that_cannot_be_entered_fails_closed` |
| 15 | Low | Clients never checked who was on the other end of a socket. A socket planted at a path a client trusts would have received keystrokes and painted arbitrary bytes onto the user's terminal. | `peer::require_same_user` on every client connection; the daemon-side check moved to the same module. Unsupported platforms fail closed. | `peer::tests::a_socketpair_within_one_process_is_the_same_user` (a cross-uid case cannot be manufactured unprivileged) |
| 16 | Info | `SIGQUIT` was left at default (exit with no cleanup); the termination handler unlinked the socket but not `kernel.json`. | `SIGQUIT` routed to the handler; the record path is unlinked too. | covered by existing lifecycle tests |

### Reviewed and left as designed

- **Same-uid trust is total.** Any process of the user — including the
  child itself — can attach, inject keys, snapshot, and kill the session.
  This is the tmux posture and the product's design; the fix for a
  compromised agent is containment outside the kernel.
- **`--env` on the command line** carries only `LATCH_SESSION_ID`; secrets
  reach the child through the launch manifest FIFO, not `ps`-visible argv.
- **Frames are capped at 16 MiB** before allocation, and JSON that fails to
  parse ends only that connection.

## 3. Known limitations (deferred, not fixed here)

- **Input can wedge on a child that stops reading.** `write_input` holds the
  input lock across a blocking `write` to the PTY master; when the tty's
  input queue is full (the child is stopped or never reads stdin) every
  later write request and the attached surface's keystrokes block behind
  it. `stat`, `snapshot`, and `kill` still work. tmux instead buffers input
  without bound; neither is free. A non-blocking master with a bounded
  input queue is the right long-term shape.
- **Scrollback is line-bounded, not byte-bounded.** `latch-term` keeps each
  retained row at full width (≈40 bytes per cell). At 2048 columns the
  50 000-line ring can reach several gigabytes. `MAX_DIMENSION` caps the
  multiplier; a byte budget in `latch-term` (it already has one for the
  attach preamble) is the real fix.
- **Replies larger than `MAX_FRAME` fail the request.** A `history` or
  styled `snapshot` over a very wide, very deep scrollback can exceed 16 MiB;
  the daemon drops that connection instead of answering.
- **A subscriber's thread lingers** until the next event after its client
  disconnects.
- **Cross-uid behaviour is not exercised by tests** — unprivileged tests
  cannot create a second uid. The peer checks are simple enough to review
  by eye; keep them that way.

## 4. Invariants a reviewer checks

These are the properties the test suite pins. A change that breaks one
needs a reason written next to it.

1. Every path the kernel creates is `0600`/`0700` **at creation**, never by
   a later `chmod`, and nothing under `/tmp` is ever followed through a
   symlink. Directories that fail the check are rejected, not repaired.
2. Every connection the daemon accepts and every socket a client talks to
   passes `peer::is_same_user`.
3. `session.exit` is set, and `CHILD_TO_HUP` cleared, under the session lock
   **before** the child is reaped; every `kill(-pid)` happens under that lock
   after checking `exit`.
4. Every queue fed by the child is bounded: surface queue (evict), parser
   backlog (backpressure on the child), event subscribers (evict).
5. The parser thread cannot die. A panic costs the model, not the session.
6. No request from a client can make the daemon allocate proportionally to
   a number the client chose without a documented cap (`MAX_FRAME`,
   `MAX_DIMENSION`, `SCROLLBACK_LINES`).
7. Child-authored strings that observers render (`title`) are display text
   only.
8. The daemon never exits without `shutdown`'s cleanup except through the
   signal handler, which performs the same cleanup.
9. The program runs in its requested cwd or not at all.

## 5. Running the suite

```sh
cargo test -p latchd                      # unit + contract + security
cargo test -p latchd --test security      # the adversarial cases only
cargo clippy -p latchd --all-targets -- -D warnings
just security-latchd                      # all of the above
```
