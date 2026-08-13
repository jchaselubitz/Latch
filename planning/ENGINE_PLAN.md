# Latch as a session engine — plan

**Status:** supersedes the M1–M6 sequence in
[`IMPLEMENTATION_PLAN.md`](./IMPLEMENTATION_PLAN.md). The diagnosis behind it is
[`ATTACHMENT_ARCHITECTURE_REVIEW.md`](./ATTACHMENT_ARCHITECTURE_REVIEW.md).

## The decision

Latch stops writing a terminal server and starts driving one. A private tmux
server becomes the session kernel; Latch keeps its CLI contract, its metadata,
its integrations, and adds the thing that is actually differentiated — a
harness-aware engine other software can build on.

Four responsibilities, in the order they are built:

```text
1. Session host          persistence, PTY ownership, multi-client   (tmux)
2. Public engine API     the contract every client integrates against
3. Harness observation   transcript -> normalized events            (read-only)
4. Harness interaction   capability-gated injection                 (later)
```

Everything else the current codebase does — the worker, the wire protocol, the
screen model, the attachment registry, the resize authority — is deleted rather
than fixed.

## Assumption this plan rests on

**Latch owns mechanical harness observation. Overlord keeps semantic
authority.**

Latch reads a harness's transcript and emits normalized events. It does not know
what a mission is, does not bind a session to one, and does not decide anything
consequential. Overlord consumes Latch's events, enriches them with mission
context, and remains the authority for permissions, delivery, and objectives.

This **amends invariant 5** of [`OVERLORD_INTEGRATION.md`](./OVERLORD_INTEGRATION.md),
which currently states that Latch does not supply the semantic integration path.
The amended version: Latch supplies *events*; Overlord supplies *authority*.
Where both can observe the same interaction, Overlord's connector wins for
anything actionable, under the first-writer-wins rule that document already
defines.

Without this written down, both products will implement the same connector twice
and disagree about which one may answer a permission prompt.

## What is no longer true

Two decisions from the original plan are withdrawn.

**D5 (exclusive write) is withdrawn entirely.** There is no product reason to
enforce a single control point. Any number of interfaces may attach and any of
them may type — the same trust boundary the architecture doc already concedes
("any process running as the same user can attach to a session and type into
it"). This removes `control_busy` from the attach path, and with it the
second-window failure, the reattach race, and the always-broken nesting attach.

**D4 (resize authority) is withdrawn with it.** The claim stack existed only to
answer "what is the controller's size". Session geometry becomes tmux's
`window-size` policy — `latest` by default, recomputed over the remaining
clients on detach, with `latch resize --pin` mapping to `manual`.

D1 (one binary), D2 (no daemon), and D3 (a screen model) survive in spirit; D3's
implementation moves into tmux.

---

## Phase 0 — Kernel swap

**Goal:** every existing `latch` command behaves the same, on top of tmux.

Latch drives a private server so the user never encounters tmux as an interface:

```bash
tmux -S ~/.latch/server -f ~/.latch/tmux.conf …
```

`tmux.conf` is generated, not user-authored: no status bar, no prefix binding,
no copy-mode keys, `remain-on-exit on`, `window-size latest`. A private socket
means the user's own tmux configuration and sessions are untouched.

### Command mapping

| Latch command | Becomes |
| --- | --- |
| `latch` / `latch shell` | `new-session -A -s <id>` then attach |
| `latch run -- <cmd>` | `new-session -d -s <id> -- <cmd>` then attach |
| `latch create --manifest-file -` | `new-session -d` with resolved env, no attach |
| `latch attach [<id>]` | `attach-session -t <id>` |
| `latch list --json` | `list-sessions -F` + the metadata sidecar |
| `latch inspect --json` | `display-message -p -F` + sidecar |
| `latch stop` | `kill-session` (graceful signal first, per policy) |
| `latch resize --pin` | `resize-window` + `window-size manual` |
| `latch open --with <viewer>` | unchanged — still launches a terminal running `latch attach` |

### What replaces the derived-state machinery

State stops being probed and starts being asked. `has-session` is one round trip
to one server, so the 500 ms `LIVENESS_PATIENCE` window and the `lost` state
largely disappear. Exit status comes from `remain-on-exit` plus
`#{pane_dead_status}` rather than from `exit.json`.

Latch keeps a small metadata sidecar for what tmux has no field for — display
name, title, redacted command label, `source.externalRunId` — keyed by session
id. Launch material still arrives over stdin and is still never written down.

### Deleted

| Component | Lines |
| --- | --- |
| `crates/latch-term` (screen model, snapshots, VT fixtures) | ~1,770 |
| `crates/latch-protocol` (framing, control vocabulary, msgpack) | ~780 |
| `worker/` (PTY, registry, queue, journal, socket, resize, spawn) | ~3,400 |
| `tests/` for the above | ~4,000 |

Roughly 10,000 lines out. What remains is the CLI, the metadata sidecar, the
viewer integrations, and — new — the engine API and connectors.

### Risks to close during the phase

- **`$TMUX` leakage.** The child sees `$TMUX` pointing at Latch's private
  socket. A user who runs their own tmux inside a Latch session will hit
  nesting refusals. Decide explicitly: unset it, rename it to
  `LATCH_TMUX`, or set `allow-nested` behaviour.
- **tmux availability and version.** Pin a minimum version, bundle the binary
  inside `Latch.app`, and have `latch doctor` report which one is in use.
- **Exit-status fidelity.** Verify `pane_dead_status` reports signals the way
  `exit.json` did, including the `128 + signal` convention.
- **Startup cost.** D1's whole justification was that a terminal profile pays
  CLI startup on every window. Measure `latch` → first prompt against the
  current binary before committing.

### Done when

The existing acceptance criteria still pass — close the window and the process
survives, reattach from anywhere, `latch list` is accurate — **plus** the three
that never passed: a second window attaches to a live session, reattaching
immediately after closing one always works, and `latch` inside a Latch session
attaches instead of failing.

---

## Phase 1 — Overlord realignment

**Most of `OVERLORD_INTEGRATION.md` survives**, because Overlord integrates
against the CLI contract and the CLI contract does not change. `capabilities`,
`create --manifest-file -`, `open`, `inspect`, `stop`, the two-settings model,
the session-identity mapping, and acceptance criteria 1–10 all hold as written.

What changes:

- **Delete the "session input control" row** from the ownership table, and the
  UI affordances behind it. There is no controller to display and none to
  request.
- **Simplify the state list.** `lost` becomes rare enough to describe as an
  error rather than a state. `creating → running → stopping → exited` stands.
- **Rewrite Stage 3.** "Embed the TypeScript Latch client, attaching directly to
  the worker socket" describes a socket that no longer exists. The embedded
  view is fed by the engine's event stream (Phase 2); an embedded *terminal*
  becomes optional and later, since `latch attach` in the user's own terminal
  already covers it.
- **Reconcile the widget-source section** with the amended invariant above:
  Latch is now a first-class event source, not only a terminal plane.

Everything about worktrees, briefing files, Agent Session Exchange bootstrap,
and `ovld protocol attach` is untouched.

---

## Phase 2 — Observation

**Goal:** a chat view of a live agent session, read-only, harness-agnostic.

A connector translates one harness's session record into normalized events:

```ts
interface HarnessConnector {
  detect(session: SessionRef): Promise<boolean>;
  subscribe(session: SessionRef): AsyncIterable<HarnessEvent>;
  capabilities(session: SessionRef): Promise<InteractionCapabilities>;
}

type HarnessEvent =
  | { type: "user_message";     text: string }
  | { type: "assistant_delta";  text: string }
  | { type: "assistant_message"; text: string }
  | { type: "tool_started";     tool: string; input?: unknown }
  | { type: "tool_finished";    output?: unknown }
  | { type: "awaiting_input";   requestId: string; kind: "permission" | "question"; prompt: string; choices?: string[] }
  | { type: "status";           status: string };
```

First connector: Claude Code, tailing
`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` — append-only, typed
records chained by `parentUuid`.

`awaiting_input` is added to the original event set deliberately. It is what
makes a notification possible, and notification is what makes a remote chat view
worth opening at all.

### The format-stability problem

Anthropic's documentation states plainly that the transcript format *"is
internal to Claude Code and changes between versions, so scripts that parse
these files directly can break on any release."* Promoting the transcript to the
primary interface puts that on the critical path. Mitigations, all required:

- A recorded-transcript fixture suite per harness, with the same discipline
  `fixtures/vt/` had.
- A version probe at connector start, and a **visible degraded state** in the UI
  — never a chat view that silently stops updating.
- `latch attach` as the always-available fallback.

### This phase has no dependencies

It needs neither Phase 0 nor Phase 1. A transcript can be tailed for a session
launched any way at all. **Start it in parallel with Phase 0** — it is the only
work here that answers the question `M2_FIELD_REPORT.md` still records as
unanswered: *do you actually reach for your agent from your phone?*

---

## Phase 3 — Injection

Deferred, but the shape is fixed by what is actually possible today.

There is **no harness-native mechanism** for sending a message into a running
interactive Claude Code session — it is an open, repeatedly-filed feature
request, and `--resume` starts a new process rather than injecting into a live
one. `TIOCSTI` has been disabled by default since Linux 6.2 as a
privilege-escalation vector. **The only mechanism that works is writing to a PTY
you own**, which is exactly what Phase 0 buys.

So injection is capability-gated, not assumed:

```ts
interface InteractionCapabilities {
  sendMessage: boolean;   // free text
  sendKeys:    boolean;   // keypresses — what permission prompts need
  resolve:     boolean;   // answer a specific requestId
}

interface HarnessInteraction {
  canSend(session): Promise<{ ok: boolean; reason?: string }>;
  sendMessage(session, text): Promise<void>;
  sendKeys(session, keys): Promise<void>;
  resolve(session, requestId, choice): Promise<void>;
}
```

Three notes that decide the design:

- **`sendMessage(text)` cannot answer a permission prompt.** That is a keypress.
  `sendKeys` is the capability the highest-value interaction needs.
- **`canSend` must read the screen, not the transcript.** The transcript records
  what completed; it cannot see a half-typed composer or an open menu.
  `capture-pane` can.
- **`resolve` is bound to a `requestId`.** The Component 4 invariant — "a widget
  cannot answer a different or later prompt" — cannot be built on free text.

When a harness-native channel does appear, it becomes a connector implementation
detail and nothing above changes.

---

## The engine API

The contract other software builds against. Existing commands keep their
`--json` shape; three are added.

```bash
latch capabilities --json                 # discovery, incl. connectors + interaction
latch events <session> --json             # NDJSON stream of HarnessEvent, one per line
latch send <session> --message - | --keys <keys> | --resolve <id>=<choice>
```

`latch events` streams newline-delimited JSON on stdout and exits when the
session ends. This keeps the no-daemon principle (D2) intact: a subscription is
a long-lived child process, not a service. A local socket or HTTP surface can
come later without changing the event model.

Overlord is the first consumer. The API is designed so it is not the only
possible one.

---

## Open decisions

1. **`$TMUX` handling** — unset, rename, or allow nesting. Phase 0 blocker.
2. **Bundle tmux or require it.** Bundling makes `Latch.app` self-contained and
   pins the version; requiring it keeps the CLI a single file. Affects D1's
   one-binary claim.
3. **Where connectors run.** In the Rust CLI (one binary, no runtime) or as a
   TypeScript sidecar (shares code with Overlord's existing connectors, adds a
   runtime dependency). This is the largest remaining design choice.
4. **Whether `latch-term` is deleted or archived.** It is good work and it is
   most of a mosh server; M4's remote transport may want it back.

## What this plan does not change

- Latch remains useful with no Overlord, no account, and no cloud.
- Launch material still arrives over stdin, never argv; only bounded display
  metadata is stored.
- Externally supplied names and titles are still sanitized at ingest.
- The terminal remains authoritative and available as the universal fallback,
  for every interaction the chat view cannot express.
