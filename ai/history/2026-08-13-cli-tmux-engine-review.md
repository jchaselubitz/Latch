# CLI / Tmux Engine Review — Bugs, Code Rot, and Legacy Code

**Date:** 2026-08-13
**Mission:** coo:715 — Refactor CLI Tmux Engine for Stability
**Scope:** `crates/latch` (engine, CLI, session state, harness, serve, update) — the
Rust local plane. TypeScript packages and the desktop app were out of scope.

## Method

- Read every source file under `crates/latch/src` (~8.6k LOC) plus the
  integration suite (`tests/tmux_kernel.rs`), CI workflow, and boundary script.
- Ran the full verification suite on current stable Rust (1.97.1):
  - `cargo test --workspace` — **47/47 tests pass** (unit + fixture + integration).
  - `cargo clippy --workspace --all-targets -- -D warnings` — **FAILS** (2 warnings).
  - `./scripts/check-boundaries.sh` — passes.
- The Overlord sibling repo was not mounted in this environment, so claims about
  what Overlord consumes are marked as assumptions.

## Verdict

The engine core is in good shape: the tmux invocation layer is disciplined
(private socket, pinned binary, generated config), launch material travels over
a FIFO and never touches argv or disk, metadata writes are atomic, and the
fixture-driven harness tests are strong. The problems are concentrated in three
places: (1) a handful of real bugs, one of them an input-safety gap; (2)
performance rot in the event-ledger polling loop; and (3) a visible layer of
legacy API and documentation left over from the pre-tmux "worker" architecture
that was replaced in the kernel swap (commits `bb5ce16`…`ba4ee34`).

---

## A. Bugs

### A1. `latch send --message` can execute text in a plain shell session — input-safety gap
**Severity: high.** `classify_screen` (`harness/interaction.rs:298`) treats any
visible line consisting of `❯` plus whitespace as an *empty Claude composer*
and unlocks `sendMessage`. But `❯` is also the default prompt glyph for
Starship, Pure, and several popular zsh themes. For a plain `latch` shell
session whose owner uses one of those prompts, `latch send <id> --message -`
pastes the text into the shell and presses Enter — executing it as a shell
command. The README promises the opposite ("unrecognized screens are refused
rather than receiving input").

*Fix:* gate `send_message`/`resolve` on the session actually hosting a known
harness — e.g. require the harness hooks sidecar to exist, or check
`meta.command_label`/`source` for a Claude launch — and keep only `send_keys`
(already the explicit, caller-owns-the-risk path) for unknown sessions.

### A2. The CI lint gate is broken on current stable clippy
**Severity: high (CI red).** `.github/workflows/ci.yml` runs
`cargo clippy --workspace --all-targets -- -D warnings` on `dtolnay/rust-toolchain@stable`.
Current stable produces two errors:

- `cli/serve/pty.rs:56` — `clippy::unnecessary_mut_passed`: `libc::openpty`
  takes `*const winsize` for the size argument; pass `&size`, not `&mut size`.
- `engine.rs:738` — `clippy::collapsible_str_replace`:
  `line.replace(SESSION_ROW_SEPARATOR, ",").replace('\t', ",")` →
  `line.replace([SESSION_ROW_SEPARATOR, '\t'], ",")`.

Both are one-line fixes.

### A3. `latch serve` terminal attach resizes every attached terminal to 80×24
**Severity: medium-high.** `serve/terminal.rs` spawns `latch attach` on a PTY
hardcoded to `DEFAULT_COLS`/`DEFAULT_ROWS` (80×24). The tmux config uses
`window-size latest`, so the moment a web/desktop client connects, the shared
session snaps to 80×24 — visibly disrupting every attached terminal — until the
client sends its first `resize` control frame.

*Fix:* accept `cols`/`rows` query parameters on the WebSocket URL (or require a
resize frame before spawning the PTY) and spawn the PTY at the client's real
size.

### A4. `latch events` transcript discovery likely fails for cwds containing `.` (and other non-alphanumerics)
**Severity: medium.** `encode_project_path` (`harness/mod.rs:1084`) replaces
only `/` with `-`. Claude Code's `~/.claude/projects` encoding replaces other
characters as well (dots, underscores). For a session whose cwd is e.g.
`~/dev/my.app`, the derived project directory won't exist and `latch events`
errors with "no Claude Code transcript found". *Verify against a real
`~/.claude/projects` entry, then match Claude's encoding exactly — or better,
scan project directories for the transcript whose `sessionId`/cwd matches,
removing the encoding dependency.*

### A5. `latch stop` reports success when the process refused to die
**Severity: medium.** `engine::stop` returns `SessionState::Running` after the
escalation window expires; `manage::stop` passes it through and `main.rs`
prints `<id> running` with **exit code 0**. A scripted caller (Overlord)
cannot tell a stop that worked from one that didn't without re-inspecting.
*Fix:* non-zero exit (or an explicit `stopped: false` field) when the session
is still running after SIGKILL.

### A6. Duplicated permission-request derivation can disagree between `events` and `send --resolve`
**Severity: medium (latent).** The request-id/prompt/choices derivation exists
twice: `harness/mod.rs` (`permission_request_id`, `permission_prompt`,
`permission_choices`, `question_*`) and `harness/interaction.rs`
(`latest_pending_request`, its own `question_*`/`permission_choices`/`string`).
The fallback ids already differ (`permission:<tool>:1970-01-01T00:00:00Z` vs
`permission:<tool>:unknown` when a hook record lacks a timestamp). Today
`capture_claude_hook` always injects a timestamp so the paths agree, but any
drift here breaks `--resolve <requestId>` against ids advertised by
`latch events`. *Fix:* extract one shared derivation module both sides call.

### A7. `latch attach --retry` retries permanent failures and swallows the error
**Severity: low.** `attach_with_retry` retries on *any* engine error — including
"session gone", which cannot succeed — and discards the intermediate error
(`let _ = error;`). Five doomed re-attaches with backoff before the user sees
the message. *Fix:* retry only on transient failures (server not yet up), or
inspect between attempts.

### A8. `latch rename` allows duplicate names, making both sessions unaddressable by name
**Severity: low.** `resolve_session` bails on ambiguous names, but `rename`
never checks for collisions. After renaming two sessions to the same name,
every by-name command fails with "ambiguous" until one is renamed by id.
*Fix:* refuse a rename that collides with a live session's name.

### A9. `latch shell --name X` inside a session silently ignores the request
**Severity: low.** `create_and_attach` maps `AttachToEnclosing` to a plain
attach, dropping the requested `--name`/`--title` without a word. `run` and
`create` refuse loudly in the same situation. Either warn on stderr or refuse
consistently.

### A10. `paste_message` failure can leave an unsubmitted message in the composer
**Severity: low.** `engine::paste_message` pastes the buffer, then sends
`Enter` as a second tmux call. If the Enter fails (or the process is killed
between the two), the text sits in the composer, and the next capability check
will refuse everything with "composer already contains text" — correct, but the
caller's error doesn't say the paste half succeeded. Worth a clearer error and
a documented recovery (`latch send --keys C-u`).

---

## B. Correctness / robustness risks (no observed failure yet)

- **B1. tmux stderr sniffing.** `engine::list`/`inspect` classify "no server
  running", "can't find session", "no sessions" by substring. Workable only
  because tmux is pinned at 3.7b; centralize the strings in one helper next to
  the `TMUX_VERSION` constant so a kernel bump revisits them once.
- **B2. `tmux()` vs `tmux_binary()` divergence.** `tmux()` silently builds a
  `/nonexistent/latch-tmux` path when `current_exe` fails and never checks the
  binary exists, so most commands report a raw "No such file or directory"
  instead of `tmux_binary()`'s helpful "run `latch update` to repair" message.
  Route `tmux()` through `tmux_binary()`.
- **B3. `most_recent_session` ordering.** Bare `latch attach` picks the
  lexically-largest session id. Ids are `ses_<ms-hex><pid-hex><seq-hex>`, so
  ordering is creation-time only down to the millisecond; within the same
  millisecond, pid/sequence hex ordering is arbitrary. Also "newest created" ≠
  "most recently active". Use `engine::list` activity (already fetched) to pick.
- **B4. `map_engine_error` string matching** (`serve/http.rs:200`): "no
  session"/"not available" substring checks decide 404 vs 500, and raw internal
  error text is returned to remote clients. Fine for loopback; tighten before
  exposing beyond localhost.
- **B5. Non-loopback serve allows any Origin and speaks plaintext HTTP.**
  `origin_allowed` returns `true` for all origins when the bind is
  non-loopback, and the bearer token then travels unencrypted. If the supported
  remote story is "SSH tunnel to loopback", say so in `--help` and consider
  refusing non-loopback binds without an explicit opt-in flag.
- **B6. `main.rs` uses `.expect()` for user-facing failures** (`shell_options`,
  `run_options`: missing `$HOME`, vanished cwd) — the user gets a panic and
  backtrace instead of a one-line error. Propagate as `Result` like every other
  path.
- **B7. Interaction lock asymmetry.** `send` takes
  `harness-interaction.lock`, but the read-only `capabilities` query doesn't,
  while `paths.rs` documents the lock as "serializing capability checks with
  PTY writes". Harmless today (queries are advisory) but the doc and the code
  should agree.

## C. Performance rot

### C1. The event-ledger poll loop is O(n²) over session lifetime
`stream_ledger` (`harness/mod.rs:350`), every 100 ms, unconditionally:

1. re-reads and re-parses the **entire** Claude transcript plus the hooks
   sidecar (`reconcile_ledger`),
2. re-reads and re-parses the **entire** event ledger,
3. re-serializes **every** existing event to a `String` to build the
   occurrence-count map,
4. then `read_event_ledger` re-reads and re-parses the whole ledger *again*.

`stream_transcript` similarly re-parses the full transcript each poll and keeps
a full `prior` vector for prefix comparison. For a long-running agent session
with a multi-megabyte transcript this is significant sustained CPU — exactly
the sessions Latch is for.

*Fix (incremental, in order of payoff):* (a) stat the transcript/hooks/ledger
first and skip the cycle when sizes+mtimes are unchanged; (b) remember the
ledger read offset and parse only appended bytes; (c) keep the existing-counts
map in memory across iterations instead of rebuilding it from re-serialized
events.

### C2. `latch list` reads every session's `meta.json` sequentially
Fine at 10 sessions; noticeable at hundreds after long uptimes since `prune`
retains exited sessions for 24 h. Not urgent — worth a note, not a change.

## D. Legacy code and dead API (pre-tmux "worker" architecture leftovers)

The kernel swap (see `planning/ENGINE_PLAN.md`; `latch-term` deliberately
archived at tag `archive/latch-term-v1`) left a compatibility layer with no
remaining consumers — `publish = false`, and the binary itself doesn't use it:

- **D1. `cli/attach.rs`:** `ConnectionState` (self-described "Legacy… kept for
  library compatibility"), `RetryPolicy::budget()`, `RetryPolicy::NONE` — all
  unused outside re-exports.
- **D2. `latch attach --watch` / `--steal`** are accepted and silently ignored
  ("Compatibility flag"). A silent no-op flag is worse than no flag. Either
  remove them, or make `--watch` real by mapping it to read-only attach
  (`tmux attach-session -r`) — genuinely useful for observers.
- **D3. `AttachOutcome`** is computed by `attach()` but discarded by every
  caller in `main.rs`. Either surface it (exit code / message when the session
  exited while attached) or remove it.
- **D4. `nesting_decision`** never returns `Decline` — that variant and the
  three `Decline` match arms in `main.rs`/`create.rs` are dead. The
  `wants_create` parameter is `true` at every call site. Collapse the function
  to its actual two outcomes.
- **D5. `cli/json.rs::parse_state`** — unused wrapper.
- **D6. Fabricated attachment data.** `manage::shared_attachments` synthesizes
  `{"id":"client-1","mode":"shared","client_kind":"tmux","client_name":"attached terminal"}`
  objects from a bare count, while the doc comments still promise
  `watch`/`control` modes and `cli|desktop|web|mobile` client kinds from the
  worker era. Replace with an honest `attached: <count>` (JSON contract change —
  coordinate with Overlord/desktop consumers).
- **D7. Tab fallback in `engine::split_session_row`** exists only because the
  fake tmux in `tests/tmux_kernel.rs` joins fields with `\t`; the real pinned
  tmux emits the requested `\u{1f}`. Update `FAKE_TMUX` to print `\x1f` and
  delete the fallback (and `display_session_row`'s tab arm).
- **D8. Empty scaffold directories:** `crates/latch-protocol/` and
  `crates/latch-term/` (empty `src`/`tests/support`, no `Cargo.toml`, not in
  the workspace, not tracked by git), `crates/latch/src/worker/`, and
  `crates/latch/tests/support/`. Delete them; the archive tag preserves the
  history.

## E. Documentation rot

- **E1. `main.rs` module doc** still describes the two-mode binary: "invoked
  with the hidden `worker` subcommand it is the per-session PTY owner. Decision
  D1." There is no `worker` subcommand; the internal entry points are
  `__launch` and `__harness-hook`.
- **E2. `cli/mod.rs`** still says "Until then the signatures exist so the suite
  compiles and fails at runtime with `todo!`" — everything is implemented.
- **E3. Worker-era phrasing** in `cli/json.rs` ("when the worker answered" ×2),
  `cli/create.rs` ("detached worker"), and `session/manifest.rs` ("size
  supplied to the worker").
- **E4. `AttachmentSummary` field docs** describe values the code never
  produces (see D6).
- (Historical decision docs under `docs/` referencing `latch-term` are fine as
  records; no change needed.)

## F. Housekeeping

- **F1.** `dist/` correctly git-ignored but accumulating 12 release archives
  locally; consider having `release-cli.sh` prune old ones.
- **F2.** The `stop` escalation loop blocks up to ~7 s spawning a tmux
  `display-message` every 20 ms; acceptable for a CLI, but worth a longer poll
  interval (50–100 ms) to cut process churn.
- **F3.** `redact_secrets` covers only `out_`/`sk_`/`AKIA` prefixes — already
  acknowledged by tests; extend as harnesses are added (e.g. `ghp_`, `xoxb-`,
  `AIza`).

## G. Suggested follow-up objective sequence

1. **Unbreak CI + trivial cleanups** — A2 clippy fixes; delete dead API and
   empty dirs (D1–D5, D7, D8); fix doc rot (E1–E3). Zero behavior change,
   makes the tree honest.
2. **Input-safety hardening** — A1 (gate message/resolve on a known harness),
   plus A10 error clarity. This is the highest-risk bug.
3. **Events pipeline efficiency** — C1 (stat-gate + incremental ledger reads),
   A6 (single shared permission-derivation module), A4 (transcript discovery).
4. **CLI ergonomics** — A5 stop exit code, A7 retry policy, A8 rename
   collision, A9 nested-shell warning, B2 unified tmux binary resolution,
   B3 most-recent-by-activity, B6 expect→Result.
5. **Serve polish (pre-desktop/SDK)** — A3 initial PTY size, D6 attachment
   contract (coordinate consumers), B4/B5 error mapping and non-loopback
   policy.

Items in group 1–2 are small and safe; group 3 changes hot paths and deserves
its own tests (a large-transcript fixture would pay for itself).
