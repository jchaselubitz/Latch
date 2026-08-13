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

The boundary is not one line between two products. There are **three channels**,
and each has exactly one owner:

```text
1  Agent  ->  Overlord     artifacts, shared state, recorded work, delivery,
                           mission attach
                           agent-initiated · authenticated · plug-in (MCP)
                           NO Latch dependency — works with Latch absent

2  Harness ->  Latch       turns, tool activity, awaiting_input, exit
                           mechanical · involuntary · transcript-derived
                           requires no cooperation from the agent

3  Human  ->  Agent        messages, keypresses, permission answers
                           requires PTY ownership — Latch only
```

**Channel 1 must keep working when Latch is not installed.** It is how an agent
reports into a mission, and that path is Overlord's alone: the agent calls
Overlord's tools directly, and neither the transport nor the authority passes
through Latch. Nothing in this plan is allowed to make artifacts, state updates,
or deliveries conditional on a Latch session existing.

**Channels 2 and 3 move to Latch**, because both need something only a session
host has. Channel 2 needs the harness's own session record; channel 3 needs a
PTY you own, which is the only mechanism that exists for injection today.

| Concern | Latch | Overlord |
| --- | --- | --- |
| Session hosting, PTY, injection mechanism | Owns | — |
| Observation: transcript parsing, normalized events | Owns | — |
| Interactive input: messages, keys, permission answers | Owns | Requests through Latch |
| Agent plug-ins — MCP tools, skills, briefing and context files | — | Owns |
| Artifacts, shared state, recorded work, delivery | — | Owns, received direct |
| Missions, objectives, execution requests, worktrees, runner | — | Owns |
| Mission binding authority | Never | Owns |

### Where the connector line falls

What moves is the **observation connector** — the per-harness code that derives
permissions, questions, and turn completion by watching a session. What stays is
the **plug-in surface** — the MCP tools an agent calls to attach, load context,
record work, add artifacts, and deliver.

The distinction is who initiates. An agent *asserting* something to Overlord is
Overlord's; a session *exhibiting* something observable is Latch's. This
**replaces invariant 5** of [`OVERLORD_INTEGRATION.md`](./OVERLORD_INTEGRATION.md),
which currently reserves both for Overlord.

### The precedence rule

The two observation paths are complementary, not redundant, and both should
exist. But they can describe the same fact — a turn completing, for instance —
so one rule settles it:

> **The agent's assertion to Overlord is the record. Latch's observation is
> presentation.**

Where only Latch can see something (a pending permission), Latch is the only
source and the question does not arise.

### Two consequences

**Binding is a declaration, not an inference.** The agent calls the
mission-attach tool itself rather than a connector inferring which session
belongs to which execution request. More trustworthy, and consistent with the
existing rule that a Latch environment marker is correlation only.

**The gap between the channels is informative.** An agent that never calls the
attach tool is invisible on channel 1 — models do not always call the tool they
were told to. Because channel 2 is involuntary, Overlord can see that a session
started and produced turns without ever attaching, and surface it. That is not a
substitute for the agent's assertion; it is how a missing assertion becomes
visible instead of silent.

### What Latch adds, and what works without it

Latch must be strictly additive. Stated plainly:

| | Agent launched bare | Agent launched under Latch |
| --- | --- | --- |
| Artifacts, state, work, delivery | Yes | Yes |
| Mission attach and authority | Yes | Yes |
| Chat view of the live session | No | Yes |
| Permission and question notifications | No | Yes |
| Send a message or answer a prompt remotely | No | Yes |
| Session survives the window closing | No | Yes |

Notification splits along the same line: delivery and artifact notifications
stay on Overlord's existing hooks, and permission or question notifications come
from Latch's `awaiting_input`. Neither should wait on the other.

### Migration cost this creates

Overlord's observation connectors are fixture-proven for permissions, questions,
and turn completion. Those fixtures — not the code — are what transfers: Latch's
connectors are ported to Rust and verified against them, which is why the port
is cheap and why Phase 1 depends on getting the fixtures out of Overlord early
rather than at migration time. See *Connectors are Rust* in Phase 1.

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

| Component | Lines | Fate |
| --- | --- | --- |
| `crates/latch-term` (screen model, snapshots) | ~1,770 | **Archived**, tag `archive/latch-term-v1` |
| `crates/latch-protocol` (framing, control vocabulary, msgpack) | ~780 | Deleted |
| `worker/` (PTY, registry, queue, journal, socket, resize, spawn) | ~3,400 | Deleted |
| `tests/` for the above | ~4,000 | Deleted |

Roughly 10,000 lines out of the workspace. What remains is the CLI, the metadata
sidecar, the viewer integrations, and — new — the engine API and connectors.
`fixtures/vt/` stays: it is recorded evidence about real harness behaviour, not
scaffolding for the crate that consumed it.

### Risks to close during the phase

- **Environment fidelity.** Two variables the child sees change under tmux, and
  they are not equally important.

  `$TMUX` is the easy one. **Assumption: nobody runs tmux inside a Latch
  session**, which collapses the choice to *unset it* — no nesting semantics to
  design, and the abstraction stops leaking into prompts and `env` output.
  Latch's own nesting guard uses `LATCH_SESSION_ID` and is unaffected.

  **`TERM` is the one that actually matters.** Today the worker passes the
  user's `TERM` straight through; under tmux the child gets tmux's
  `default-terminal` instead. That changes truecolor and italic detection and
  some key encodings — for exactly the full-screen TUI agents this product
  exists to host. `SSH_SETUP.md` already treats `TERM` as something to verify by
  hand. Set `default-terminal` deliberately, and check a real Claude Code
  session renders identically before and after.
- **Bundling tmux.** tmux is vendored and pinned rather than required from the
  user's machine, which removes the version floor, the missing-dependency case,
  and any interaction with a tmux the user already runs. Latch invokes it by
  absolute path with `-S` and `-f`, so the two never meet. What it costs:

  - **The updater changes shape.** `latch update` currently replaces *one file*
    and refuses a copy another package manager owns. It becomes a small
    payload — `latch` plus a vendored `tmux` — so the replace step, the
    checksum verification, and the Homebrew-cellar refusal all need revisiting
    rather than reusing.
  - **The vendored binary needs signing too.** The release already does
    Developer ID signing and notarization; the bundled tmux must carry the same
    identity and be included in the notarized payload, or Gatekeeper rejects it.
  - **You now own a CVE surface.** A pinned dependency is a dependency you have
    to bump deliberately. `latch doctor` should report the vendored version.

  This does weaken — though not overturn — the distribution argument that
  settled decision 3. "One file" becomes "one payload, two binaries, one of them
  vendored and rarely changing." That is still meaningfully different from a
  second *authored* artifact that has to stay in lockstep with the CLI.
- **Exit-status fidelity.** Verify `pane_dead_status` reports signals the way
  `exit.json` did, including the `128 + signal` convention.
- **Startup cost.** D1's whole justification was that a terminal profile pays
  CLI startup on every window, so this is a benchmark against the current
  binary — with three specifics, because a naive one measures the wrong thing:

  1. **Cold and warm are different cases.** Cold — the first window with no
     server running — pays tmux server startup and config parse. Warm — every
     window after — is one connect to a server that already exists, with no PTY
     allocation and no socket bind, and should be *faster* than today's spawn.
     A loop benchmark measures only warm and will flatter the result.
  2. **Measure to first prompt, not to process start.** That is what a person
     perceives, and it is the endpoint D1's claim was about.
  3. **The bar is "not perceptibly worse."** If warm start improves, which is
     likely, that is an argument for the swap rather than a risk cleared.

### Done when

The existing acceptance criteria still pass — close the window and the process
survives, reattach from anywhere, `latch list` is accurate — **plus** the three
that never passed: a second window attaches to a live session, reattaching
immediately after closing one always works, and `latch` inside a Latch session
attaches instead of failing.

---

## Phase 1 — Harness integration

**Goal:** a chat view of a live agent session, read-only, harness-agnostic.

A connector translates one harness's session record into normalized events. The
event is the contract, so it is defined as a schema rather than as any one
language's types — `fixtures/harness/harness-event.v1.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "harness-event.v1.json",
  "title": "HarnessEvent",
  "type": "object",
  "required": ["type", "sessionId", "at"],
  "properties": {
    "type": {
      "enum": ["user_message", "assistant_delta", "assistant_message",
               "tool_started", "tool_finished", "awaiting_input", "status"]
    },
    "sessionId":      { "type": "string" },
    "at":             { "type": "string", "format": "date-time" },
    "harnessVersion": { "type": "string" },
    "connectorEpoch": { "type": "integer" }
  },
  "oneOf": [
    { "properties": { "type": { "const": "user_message" },
                      "text": { "type": "string" } },
      "required": ["text"] },

    { "properties": { "type": { "const": "assistant_delta" },
                      "text": { "type": "string" } },
      "required": ["text"] },

    { "properties": { "type": { "const": "assistant_message" },
                      "text": { "type": "string" } },
      "required": ["text"] },

    { "properties": { "type":  { "const": "tool_started" },
                      "tool":  { "type": "string" },
                      "input": {} },
      "required": ["tool"] },

    { "properties": { "type":   { "const": "tool_finished" },
                      "tool":   { "type": "string" },
                      "output": {} },
      "required": ["tool"] },

    { "properties": { "type":      { "const": "awaiting_input" },
                      "requestId": { "type": "string" },
                      "kind":      { "enum": ["permission", "question"] },
                      "prompt":    { "type": "string" },
                      "choices":   { "type": "array", "items": { "type": "string" } } },
      "required": ["requestId", "kind", "prompt"] },

    { "properties": { "type":   { "const": "status" },
                      "status": { "type": "string" } },
      "required": ["status"] }
  ]
}
```

`harnessVersion` is the stamp described under *format stability* below. It is on
every event rather than sent once, so a transcript that changes shape mid-session
— a harness upgraded while a session was open — is still attributable.

`connectorEpoch` is the connector's derivation stamp, and it is not the same
fact as `harnessVersion`: a connector patch can change how events are derived
from an unchanged transcript — splitting deltas differently, emitting a new
event type — while the harness version stays the same. The epoch is bumped
whenever derivation changes in a way that can shift event indexes. Its purpose
is cursor safety for resumable consumers (see `--from` under *The engine API*):
within one epoch, event indexes are stable and a stored cursor may resume;
across an epoch change, the cursor is invalid and the consumer re-syncs from
the start instead of resuming into a shifted stream. Adding the field now costs
one line; adding it after clients hold cursors means designing an invalidation
story retroactively.

`HarnessConnector` itself is an internal Rust trait, not a published interface:
`detect`, `subscribe`, and `capabilities` over a session reference. What other
software integrates against is the schema above and the CLI in *The engine API*,
which is why the trait's exact shape can change without breaking anyone.

First connector: Claude Code, tailing
`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` — append-only, typed
records chained by `parentUuid`.

`awaiting_input` is added to the original event set deliberately. It is what
makes a notification possible, and notification is what makes a remote chat view
worth opening at all.

### The format-stability problem — deliberately deferred

Anthropic's documentation states plainly that the transcript format *"is
internal to Claude Code and changes between versions, so scripts that parse
these files directly can break on any release."*

**Decision: build the parser first, solve this later.** It is a maintenance risk
rather than a design risk — it changes nothing about the architecture — and a
mitigation designed before the format has churned once is a guess. `latch
attach` remains the fallback in the meantime, which is what makes deferring it
safe.

Two things cost almost nothing now and are expensive to retrofit, so do them
while building rather than as a later project:

- **Keep the raw records next to the parsed events.** A fixture corpus then
  accumulates for free. Reconstructing one later means going back to sessions
  that no longer exist.
- **Stamp the harness version on what was parsed.** One field. Without it, when
  something does break, there is no way to tell which release did it.

### Connectors are Rust, verified against Overlord's fixtures

**Decided.** The connector layer lives in the `latch` binary rather than in a
TypeScript sidecar.

The deciding factor is not performance. Connectors are not on the every-window
path — they run when something subscribes to `latch events`, which is a UI
action, not a window action — so D1's startup argument does not transfer here.
Nor is an always-on process needed: the transcript is an append-only file, so a
late subscriber backfills by reading from a cursor. No daemon, no missed events.

What decides it is distribution. `latch update` replaces **one file**, refuses a
copy another package manager owns, and verifies a Developer ID signature. That
is shipped, documented behaviour. A sidecar means rewriting it, or taking a Node
dependency on the user's machine, or shipping an artifact roughly ten times
larger.

And the rewrite is small. A JSONL tail, a `parentUuid` chain walk, and a state
machine — a few hundred lines. **Overlord's fixtures transfer unchanged**,
because this repo already builds fixtures as language-neutral data: recorded
input, expected output, a description, in `fixtures/protocol/` and
`fixtures/vt/`. They are not a reason to stay in TypeScript; they are the oracle
that makes a port safe.

The argument against — that a format expected to churn should live in the
faster-iterating language — is real but smaller than it looks. A break needs a
fixture, a parser fix, and a release either way. Rust adds a compile step, not a
redesign.

**Revisit if** Latch supports four or more harnesses. Per-harness connector
volume is what would make iteration speed outweigh distribution; at one or two
it does not.

### One schema, two languages

`HarnessEvent` is defined **once, as a JSON schema in `fixtures/`**, and the
Rust types and the TypeScript types are both generated from it. The protocol
fixtures already work this way, and it removes the duplicate-definition cost
that is otherwise the real price of keeping connectors out of TypeScript — the
chat UI consumes the same contract the connector emits, checked by the same
fixtures.

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

### Retained: the plug-in surface, and it must stand alone

Channel 1 stays exactly where it is — the MCP tools an agent calls to attach to
a mission, load context, record work, add artifacts, and deliver; any installed
skills or commands; and the briefing and context files the runner constructs.

**The acceptance test for this phase is that channel 1 still works with Latch
uninstalled.** Artifacts, shared state, and deliveries reach a mission from an
agent launched in a bare terminal, exactly as they do today. If any of that
starts depending on a Latch session, the phase has failed regardless of what
else it delivered.

### Decommissioned: the observation connectors

Per-harness code that derives permissions, questions, and turn completion by
watching a session moves to Latch. Overlord subscribes to `latch events`
instead of maintaining its own.

- **Binding is the agent's declaration.** Confirm the plug-in path covers every
  case the connector's `ovld protocol attach` covered, and add reconciliation
  against Latch's involuntary event stream so a launched-but-never-attached
  session is surfaced rather than silently missing.
- **Permission and question events** arrive as `awaiting_input` and are answered
  through `resolve`. Overlord displays, applies mission policy, and relays.
- **Apply the precedence rule** wherever both channels can report the same fact:
  the agent's assertion is the record, Latch's observation is presentation.
- **Split notification** to match: delivery and artifact notifications stay on
  Overlord's hooks; permission and question notifications come from Latch.

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

So injection is capability-gated, not assumed. Capabilities are data, reported by
`latch capabilities --json` per session and schema'd alongside the event —
`fixtures/harness/interaction-capabilities.v1.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "interaction-capabilities.v1.json",
  "title": "InteractionCapabilities",
  "type": "object",
  "required": ["sendMessage", "sendKeys", "resolve", "canSend"],
  "properties": {
    "sendMessage": { "type": "boolean", "description": "free text" },
    "sendKeys":    { "type": "boolean", "description": "keypresses — what permission prompts need" },
    "resolve":     { "type": "boolean", "description": "answer a specific requestId" },
    "canSend": {
      "type": "object",
      "required": ["ok"],
      "properties": {
        "ok":     { "type": "boolean" },
        "reason": { "type": "string", "description": "why not, when ok is false" }
      }
    }
  }
}
```

The operations themselves are the `latch send` surface in *The engine API* —
`--message`, `--keys`, `--resolve <id>=<choice>` — rather than a library
interface, so a client in any language reaches them the same way.

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
latch events <session> --json [--from <cursor>]   # NDJSON stream of HarnessEvent
latch send <session> --message - | --keys <keys> | --resolve <id>=<choice>
```

`latch events` streams newline-delimited JSON on stdout and exits when the
session ends. This keeps the no-daemon principle (D2) intact: a subscription is
a long-lived child process, not a service. A local socket or HTTP surface can
come later without changing the event model.

`--from <cursor>` resumes the stream from an event index instead of the
beginning, which is what makes a remote consumer's reconnect cheap — the SDK
plan's events channel is defined by it. The flag is a promise, not a mechanism:
replaying the derivation and skipping the first N events is a valid
implementation. What it commits the connector to is determinism — within one
`connectorEpoch`, the event sequence is a pure function of the transcript, so
the same cursor always names the same position. When the epoch changes,
outstanding cursors are invalid and consumers restart from zero.

Overlord is the first consumer. The API is designed so it is not the only
possible one.

---

## Open decisions

1. ~~**`$TMUX` handling.**~~ **Settled:** unset it, on the assumption that nobody
   runs tmux inside a Latch session. What remains is `TERM` — pick a
   `default-terminal` and verify a real agent renders identically. See
   *Environment fidelity* in Phase 0.
2. ~~**Bundle tmux or require it.**~~ **Settled: bundle it.** A pinned, vendored
   tmux removes an entire class of "works on my machine" — no version floor to
   police, no user config to collide with, no missing dependency on a fresh
   install. See *Bundling tmux* in Phase 0 for what it costs.
3. ~~**Where connectors run.**~~ **Settled: Rust, in the `latch` binary.**
   Connectors are not on the every-window path, so D1's startup argument does not
   apply; what decides it is the one-file install and update story, and a port
   verified against language-neutral fixtures is small. `HarnessEvent` is
   schema-first so both languages generate from one definition. See *Connectors
   are Rust* in Phase 1.
4. ~~**Whether `latch-term` is deleted or archived.**~~ **Settled: archived.**
   Removed from the workspace, but the commit that last contains it is tagged
   `archive/latch-term-v1` so it stays retrievable without rotting in-tree
   against a build nothing exercises. It is most of a mosh server, and the remote
   transport may want it back.

   **`fixtures/vt/` stays in the tree regardless.** Those are recorded streams
   from real Claude Code and Codex sessions — evidence about how these harnesses
   actually behave, language-neutral, and costing nothing to keep. They outlive
   the crate that motivated them, and re-recording them later would mean
   reproducing conditions that no longer exist.

## What this plan does not change

- Latch remains useful with no Overlord, no account, and no cloud.
- Launch material still arrives over stdin, never argv; only bounded display
  metadata is stored.
- Externally supplied names and titles are still sanitized at ingest.
- The terminal remains authoritative and available as the universal fallback,
  for every interaction the chat view cannot express.
