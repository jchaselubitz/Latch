# Latch as a session engine — plan

**Status:** supersedes the M1–M6 sequence in
[`IMPLEMENTATION_PLAN.md`](./IMPLEMENTATION_PLAN.md). The diagnosis behind it is
[`ATTACHMENT_ARCHITECTURE_REVIEW.md`](./ATTACHMENT_ARCHITECTURE_REVIEW.md).

## The decision

Latch stops writing a terminal server and starts driving one. A private tmux
server becomes the session kernel; Latch keeps its CLI contract, its metadata,
its integrations, and adds the thing that is actually differentiated — a
harness-aware engine other software can build on.

Four responsibilities, built in two parallel tracks:

```text
Phase 0   Session host          persistence, PTY ownership, multi-client   (tmux)
Phase 1   Harness integration   transcript -> normalized events       (read-only)
              ^ these two are independent; run them at the same time

Phase 2   Overlord migration    plug-ins stay, connectors move to Latch
Phase 3   Harness interaction   capability-gated injection
```

The public engine API spans all four: it is the contract every client
integrates against, and Overlord is its first consumer rather than its only
possible one.

Everything else the current codebase does — the worker, the wire protocol, the
screen model, the attachment registry, the resize authority — is deleted rather
than fixed.

## The ownership split

**Latch is the harness integrator. Overlord keeps the agent plug-ins.**

| Concern | Latch | Overlord |
| --- | --- | --- |
| Session hosting, PTY, injection mechanism | Owns | — |
| Harness connectors, transcript parsing, normalized events | Owns | — |
| Agent plug-ins — MCP tools, skills, briefing and context files | — | Owns |
| Missions, objectives, execution requests, worktrees, runner | — | Owns |
| Mission binding authority | Never | Owns |

The decomposition is orthogonal **by direction of flow**, which is what makes it
clean:

```text
Latch          reads the agent        per-harness,          mission-agnostic
Overlord       instructs the agent    per-mission-concept,  harness-agnostic
```

Neither side needs per-harness code on the other's axis. An MCP tool that tells
an agent how to attach to a mission works on any harness; a transcript connector
works for any mission. This also **removes an arbitration rule** an earlier draft
of this plan needed: there is no longer a case where both products observe the
same interaction and have to decide whose event wins.

This **replaces invariant 5** of [`OVERLORD_INTEGRATION.md`](./OVERLORD_INTEGRATION.md),
which currently reserves the semantic integration path for Overlord's connectors.
Overlord does not keep connectors. It keeps the plug-ins.

### Three consequences

**Binding moves from observation to declaration.** Today a connector performs
`ovld protocol attach` after inferring which session belongs to which execution
request. Under the plug-in model the *agent* calls the mission-attach tool
itself. That is strictly more trustworthy — an authenticated, intentional act
rather than an inference — and it is consistent with the existing rule that a
Latch environment marker is correlation only and confers no mission authority.

**New risk: an agent that never calls the tool is invisible to Overlord.**
Compliance becomes a prompt-following problem, and models do not always call the
tool they were told to. The mitigation falls out of the split: Latch's event
stream gives Overlord an *independent, mechanical* view — session started, turns
happening, session exited — so "launched but never attached" is detectable
rather than silent. Overlord should reconcile the two and surface the gap.

**Permissions become Latch end to end.** Observed as `awaiting_input`, answered
through `resolve`. Overlord displays them and relays the user's choice, and may
apply mission policy on top, but there is one mechanism and one request id
rather than two implementations racing.

### Migration cost this creates

Overlord's connectors are described as fixture-proven for permissions,
questions, and turn completion. That code and those fixtures are the obvious
seed for Latch's connector layer rather than something to rebuild — which makes
open decision 3 (where connectors run) urgent rather than deferrable, and
couples Phase 1 to Phase 2 below.

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

## Phase 1 — Harness integration

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

### Seed it from Overlord's connectors

Overlord's existing connectors already normalize permissions, questions, and
turn completion, with fixtures. Port that rather than rebuild it — the fixtures
in particular are the expensive part, and they encode harness behaviour nobody
should discover twice. Whether the port is to Rust or the code moves as a
TypeScript sidecar is open decision 3, and it has to be settled before this
phase starts rather than during it.

### This phase does not depend on Phase 0

A transcript can be tailed for a session launched any way at all, so harness
integration needs nothing from the kernel swap. **Run the two in parallel.**
This is also the only work in the plan that answers the question
`M2_FIELD_REPORT.md` still records as unanswered: *do you actually reach for
your agent from your phone?* Read-only observation answers it without waiting
for injection or for tmux.

---

## Phase 2 — Overlord migration

**Goal:** Overlord keeps the plug-ins and consumes Latch for everything else.

The launch contract is unaffected — Overlord integrates against the CLI, and the
CLI does not change. `capabilities`, `create --manifest-file -`, `open`,
`inspect`, `stop`, the two-settings model, the session-identity mapping, and
acceptance criteria 1–10 all hold as written. So does everything about
missions, objectives, execution requests, worktrees, and the runner.

### Retained: the agent plug-ins

Everything that instructs an agent how to use Overlord stays exactly where it
is — the MCP tools an agent calls to attach to a mission, load context, record
work, add artifacts, and deliver; any installed skills or commands; and the
briefing and context files the runner constructs. These are the mission
semantics *expressed to the agent*, and they are harness-agnostic already.

### Decommissioned: the connectors

Harness observation moves to Latch. Overlord stops maintaining per-harness
transcript or event code and subscribes to `latch events` instead.

- **Binding is now the agent's declaration**, not the connector's inference.
  Confirm the plug-in path covers every case the connector's
  `ovld protocol attach` covered, and add reconciliation against Latch's
  mechanical event stream so a launched-but-never-attached session is surfaced
  rather than silently missing.
- **Permission and question events** arrive as `awaiting_input` and are answered
  through `resolve`. Overlord displays, applies mission policy, and relays.

### Corrections to `OVERLORD_INTEGRATION.md`

- **Delete the "session input control" row** from the ownership table, and the
  UI affordances behind it. There is no controller to display and none to
  request.
- **Replace invariant 5** with the ownership split above.
- **Simplify the state list.** `lost` becomes rare enough to describe as an
  error rather than a state. `creating → running → stopping → exited` stands.
- **Rewrite Stage 3.** "Embed the TypeScript Latch client, attaching directly to
  the worker socket" describes a socket that no longer exists. The embedded view
  is fed by the engine's event stream; an embedded *terminal* becomes optional
  and later, since `latch attach` already covers it.
- **Collapse the two-sources-for-widgets section.** There is one source now.

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
3. **Where connectors run — now urgent.** In the Rust CLI (one binary, no
   runtime, but Overlord's fixture-proven connector code has to be ported) or as
   a TypeScript sidecar (the existing code moves largely intact, at the cost of a
   runtime dependency and D1's one-binary claim). Making Latch the harness
   integrator turns this from a design preference into a Phase 1 blocker.
4. **Whether `latch-term` is deleted or archived.** It is good work and it is
   most of a mosh server; M4's remote transport may want it back.

## What this plan does not change

- Latch remains useful with no Overlord, no account, and no cloud.
- Launch material still arrives over stdin, never argv; only bounded display
  metadata is stored.
- Externally supplied names and titles are still sanitized at ingest.
- The terminal remains authoritative and available as the universal fallback,
  for every interaction the chat view cannot express.
