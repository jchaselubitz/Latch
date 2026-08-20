# Review: Conversation Architecture and Implementation Plan

**Reviewing:** [`CONVERSATION_ARCHITECTURE.md`](./CONVERSATION_ARCHITECTURE.md) and
[`CONVERSATION_IMPLEMENTATION_PLAN.md`](./CONVERSATION_IMPLEMENTATION_PLAN.md)
against the code on `main`.

**Verdict:** the core judgment is right. The current pipeline really is
`O(transcript)` per append — `reconcile_if_changed` reads the whole transcript
and re-derives every event on every change
(`crates/latch/src/harness/mod.rs:532`), and the gateway forks a `latch events`
child per socket (`crates/latch/src/cli/serve/events.rs:210`). One shared,
checkpointed, in-process connector per session is the correct replacement, and
the connector boundary is drawn in the right place.

The problems are not in the shape. They are in four things the documents assume
already exist or already work, and that do not:

1. remote permission enforcement, which lives *outside* `latch serve` and is
   path-based;
2. an authoritative signal that a request was answered, which no source emits;
3. an incremental branch-selection story, which the checkpoint design
   contradicts;
4. a stable transcript binding, which today is "newest file by mtime."

Findings are ordered by how much rework they cause if found late.

---

## Part 1 — Blocking correctness issues

### 1.1 Moving send/resolve inside the WebSocket removes permission enforcement

This is the most serious finding.

Today `observe` / `interact` / `control` is enforced in `latch-remote`, by
parsing the initial HTTP request line and body before proxying to the loopback
gateway:

```rust
let required = permission_for_request(method, target, &request[end + 4..required_len])?;
if !permission.permits(required) {
    bail!("device permission does not allow this operation");
}
```

`crates/latch/src/cli/remote_access.rs:1704`, table at `:1749`. `GET
/v1/sessions/{id}/events` is `Observe`; `POST /v1/sessions/{id}/send` is
`Interact` (or `Control` if the body has `keys`); terminal is `Control` unless
the URL says `mode=read-only`.

`latch serve` never learns the grant. `authorize_and_inject` injects only the
bearer token (`remote_access.rs:1710`). The gateway has no concept of a device
at all.

The v2 design puts `send_message` and `resolve_request` inside a socket opened
by a GET that must be `Observe`-level, because an observe-only phone is
supposed to be able to read the conversation. The proxy inspects **only the
initial request**; it cannot see WebSocket frames. So an observe-only phone
opens the conversation socket and then sends whatever it likes.

Phase 4 step 2 says "Reuse the current paired transport, Noise authentication,
gateway credential injection, and permission grant." There is no permission
grant at the gateway to reuse. The phone's `DevicePermission`
(`apps/LatchMobile/Sources/LatchMobileKit/PairedDevice.swift`) is display-only —
its own doc comment says "The phone never decides its own permission."

**Fix, and it is a design change, not a detail.** The grant has to reach the
Hub. Either:

- `authorize_and_inject` injects a trusted grant header alongside the bearer
  (it already rewrites headers and already strips client-supplied
  `Authorization` / `Proxy-Authorization`, so the header is unforgeable by the
  same argument that makes the token injection safe), and the conversation
  socket enforces per-message; or
- mint per-device gateway tokens carrying a scope, and enforce in `serve`.

Either way, add "the Hub enforces `interact` per operation" to Phase 4 exit
criteria, and add a test that an observe-grant socket refuses `send_message`.

### 1.2 The remote-access allowlist is hard-coded to `/v1/` and no phase touches it

```rust
if words.next().is_some()
    || version != "HTTP/1.1"
    || !target.starts_with("/v1/")
```

`crates/latch/src/cli/remote_access.rs:1679`.

Phase 0 replaces the router with `/v2` routes. Neither the "target repository
shape" section nor any phase mentions `crates/latch/src/cli/remote_access.rs`
or `crates/latch-remote`. Ship Phase 0 as written and **every paired request
fails** with `request target is not permitted` — LAN and ICE alike, terminal
included.

`permission_for_request` also enumerates `capabilities | events | terminal |
send` by path segment and special-cases `mode=read-only`. All of it needs
rewriting in the same commit as the router.

Add the remote helper to the Phase 0 work list and to the exit criteria
("terminal still works through its v2 path" must be tested *through the Noise
tunnel*, not just on loopback, or this is missed).

### 1.3 No source tells the connector a request was answered

Phase 1 exit criterion: "A request remains pending until an explicit `resolved`
or `dismissed` mutation."

Nothing emits that mutation. Claude's plugin captures `PermissionRequest` only
(`crates/latch/src/harness/mod.rs:263`) — there is no `PermissionResolved`
hook, and the transcript has no resolution record. Latch's own
`harness_resolutions` ledger (`interaction.rs:563`) records *only* resolutions
Latch itself sent; a prompt answered at the computer leaves no trace anywhere.

Two mechanisms cover this today, and the plan deletes both:

- the client heuristic — `Transcript.apply` drops `pendingRequest` as soon as
  any non-`awaitingInput` event arrives, with the comment "the prompt was
  answered, here or at the computer"
  (`apps/LatchMobile/Sources/LatchMobileKit/Transcript.swift:84`);
- the last-moment screen check — `classify_screen` re-derives the pending
  request from the live pane and refuses a stale resolve
  (`interaction.rs:391`).

The second survives as action validation, but it only runs when the user
presses a button. In a **pushed**-state model with no screen polling, the phone
displays a live permission card for a prompt that was answered on the Mac ten
minutes ago, and the user finds out by tapping it and getting a refusal.

Specify the rule explicitly. The cheapest correct one is the existing
heuristic, promoted into the connector: any source record observed after a
request, other than that request, resolves it as `dismissed` (agent moved on).
Add screen-derived confirmation on the state-refresh path (1.4).

### 1.4 Pushed interaction state needs pane polling that the budget doesn't include

`ConversationState.sendMessage` (enabled + reason) and `pendingRequest` are
exactly what `capabilities_for` computes today, and it computes them from the
live pane:

```rust
let screen = engine::capture_pane(home, id)?;
let state = classify_screen(&paths, &screen)?;
```

`crates/latch/src/harness/interaction.rs:307`. `capture_pane` spawns a `tmux`
subprocess (`engine.rs:687`); `inspect` spawns another and retries up to three
times (`engine.rs:644`).

The architecture's entire polling budget is "A 100–250 ms stat poll is
acceptable initially" — a `stat` on a file. Success criterion 6 ("Permission
and question controls update from pushed state") cannot be met by stat'ing a
file, because the composer being empty or full is not in the transcript at all.
At 100–250 ms you are spawning 4–10 `tmux` processes per second per warm
session, times every warm session.

Resolve it one way or the other in the document:

- make state refresh **event-driven** — capture the pane after any source
  append, after any action, and on a slow idle heartbeat (1–2 s), hashing the
  capture so unchanged screens produce no mutation; and
- state in the doc that `sendMessage` availability is advisory between
  refreshes, which the architecture already half-says ("advertised availability
  is user-interface guidance").

Add pane-capture rate to the Phase 8 measurement list. It is likely the
dominant steady-state cost of the whole system, and it is the one cost the
document never mentions.

---

## Part 2 — Performance and design gaps

### 2.1 Incremental parsing and active-branch selection contradict each other

`active_records` defines the active set **from the tail backwards**: find the
last non-sidechain record, walk `parentUuid` to the root, keep that chain plus
every record with no `uuid` (`crates/latch/src/harness/mod.rs:759`).

So "read only the bytes after the offset" is not sufficient on its own — you
need to know whether the new record's parent is the current chain tail.

The plan's answer is to put "active-record graph state required for branch
selection" in the checkpoint (Phase 2 step 3). That graph is O(all records).
Combined with "atomically persist the new checkpoint after journal mutations
are durable," **every append writes an O(N) file** — the same cost being
removed, moved from read to write. It also contradicts the architecture's "On
startup the actor loads the snapshot and journal once, resumes the connector
from its checkpoint, and processes only appended source bytes," which is only
true if the graph was persisted.

You do not need the graph. You need the uuid set of the *active chain*, which
is O(items already in the snapshot). Classify each appended record:

| new record's `parentUuid` | action |
| --- | --- |
| equals the current chain tail | append; O(1) — the overwhelmingly common case |
| an earlier uuid on the active chain | truncate items after it via `items_removed`, continue in the **same** generation |
| unknown / not on chain | rebuild, new generation |

The middle row matters: rewinds and interrupts are routine in Claude, and
today they hard-fail the stream ("Claude Code changed the active transcript
branch; restart from cursor 0", `harness/mod.rs:409`). If every rewind becomes
a full reparse plus a new generation plus a full snapshot to every subscriber,
generation resets are common, not exceptional — and the document treats them as
exceptional. Handling on-chain rewinds as truncation also gives `items_removed`
a job; as specified it has none, since every invalidating change is defined to
produce a new generation instead.

Sidechain (subagent) records are also worth an explicit rule: they append
continuously during a `Task` and never move the chain tail.

### 2.2 Two sources merged by wall-clock cannot yield a stable incremental ordinal

The transcript is written by Claude. The hook sidecar is written by a separate
`latch __harness-hook` process (`harness/mod.rs:208`), stamping its own
`timestamp` at capture. Reconciliation today merges the union and sorts by `at`
(`harness/mod.rs:556`) — correct precisely *because* it re-derives the whole
list each time.

With two independent byte offsets and immediate emission, a hook record can be
observed after a transcript item that carries a later timestamp. `ordinal` is
described as "a stable ordering key" and `history_request` pages by
`beforeOrdinal` — but nothing says how ordinals are assigned, or what happens to
a late record that belongs earlier.

`docs/ARCHITECTURE_RULES.md` already fixed this rule for the current system:
"emitted cursor positions are ledger indexes and are never renumbered when one
source writes late." Carry it forward and write it down:

- `ordinal` = observation order, monotonic, never renumbered;
- `createdAt` = source timestamp, for display only;
- clients sort by `ordinal`, never by `createdAt`.

Without this, `beforeOrdinal` pagination is unsound and two clients can render
different orders.

### 2.3 Transcript binding by mtime will thrash generations

When a session has no `external_run_id`, transcript discovery falls back to the
newest `.jsonl` in the encoded project directory (`harness/mod.rs:1189`,
`newest_jsonl` at `:1208`).

Two Latch sessions in the same working directory therefore resolve to the *same
file*, and which one wins flips with every write. Given how Latch is used —
several agent sessions on one repo — this is not a corner case.

Today the consequence is a wrong chat view. Under a checkpointed connector, the
consequence is a loop: file identity changes → "source replacement" → rebuild →
new generation → full snapshot to every subscriber → repeat on the next write.
That is a pathological CPU and bandwidth loop, and Phase 2 inherits it verbatim
("Move reusable Claude knowledge behind `connectors/claude`: ... transcript
discovery").

Fix it before Phase 2, and the mechanism already exists: the hook payload
carries the authoritative binding.

```json
{"session_id":"851b055e-…","transcript_path":"/Users/jake/.claude/projects/…/851b055e-….jsonl","hook_event_name":"PermissionRequest", …}
```

`fixtures/harness/claude-code/live-permission-2.1.228/raw.jsonl`

Register a `SessionStart` hook in the generated plugin
(`harness/mod.rs:240` already writes `hooks.json`), pin `transcript_path` and
`session_id` on first observation, and report `phase: starting` until it
arrives instead of guessing by mtime. `newest_jsonl` should be deleted, not
ported.

### 2.4 Overflow → snapshot makes a slow link worse

"On subscriber overflow, the Hub drops queued mutations and sends a fresh
snapshot."

A snapshot is "the newest 100 items by default." That is almost always *more*
bytes than the mutations it replaces. On the link where overflow actually
happens — TURN — this is overflow → larger payload → overflow, with the client
never converging.

Specify:

- bound the subscriber queue in **bytes**, not just count;
- the replacement snapshot **replaces** queued work rather than being appended
  behind it;
- rate-limit resnapshots per subscriber, with a back-off;
- on repeated overflow, fall back to `state_changed` only and let the client
  pull history at its own pace.

Phase 8 already tests "stalled-subscriber recovery time" — test it over a
throttled link, not a stalled reader, since those fail differently.

### 2.5 The actor mailbox serializes tmux actions with source processing

One `send_message` is, at minimum: `inspect` (1–3 tmux spawns, with a sleep
between retries), `capture_pane` (1), `load-buffer` (1), `paste-buffer` (1),
`send-keys` (1) — `engine.rs:644`, `:687`, `:702`, `:756`. All blocking, all
subprocess.

Phase 3 step 4: "Serialize all source mutations and user actions through the
actor mailbox." So a slow or wedged tmux stalls source processing and broadcast
for every subscriber of that session. The architecture guarantees a slow client
cannot block the connector; it never claims the inverse, and the inverse is the
one that is easy to hit.

Give actions their own lane: dispatch them to a blocking pool with a timeout,
have the actor own only the state transition when the result comes back, and
keep the mailbox free meanwhile. Add "a wedged tmux does not stall broadcast"
to the Phase 3 concurrency tests.

### 2.6 `operationId` idempotency does not survive a gateway restart

Today the idempotency store is in-memory with a 10-minute TTL
(`crates/latch/src/cli/serve/http.rs:43`, `:482`), and discovery advertises
`gatewayInstanceId` (`:587`) precisely so a client can tell that the store is
gone.

The plan keeps a "bounded idempotent operation results" ledger in the actor,
and the Phase 8 failure list has "Background the phone during send and retry the
same operation ID" — but not "restart `latch serve`, then retry." It does have
"Kill and restart `latch serve` during an active conversation," which is the
same scenario without the retry.

A lost ledger turns a retry into a second paste into a live composer. That is
user-visible and not undoable. Either persist accepted operation ids next to
the connector checkpoint, or keep `gatewayInstanceId` in `gateway-capabilities.v2`
and require the client to surface a manual retry across an instance change
rather than auto-retrying. The plan should say which.

---

## Part 3 — Scope the plan under-counts

### 3.1 `latch events` is a documented external contract

`planning/OVERLORD_INTEGRATION.md` names it as Overlord's observation surface
in five places (lines 19, 65, 383, 524, 567), including "Owns `latch events` /
`awaiting_input`" in the ownership table. Phase 2 step 2 deletes the command;
nothing replaces it, and Phase 6 only cleans up TypeScript packages.

The architecture's justification — "There is no compatibility requirement
because Latch has one user and the desktop app, mobile app, CLI, and packages
can move together" — does not cover a separate product consuming the CLI.

Either define the successor (`latch conversation <session> --json --follow`
streaming the same normalized items the Hub serves, which is a thin client of
the Hub and worth having regardless), or state in the plan that Overlord
integration is knowingly broken by this release and `OVERLORD_INTEGRATION.md`
is being retired.

Related and unaddressed: `latch send` and `latch capabilities` remain
CLI commands (`crates/latch/src/main.rs:248`, `:306`) that run as **separate
processes**. They bypass the Hub's actor, its operation ledger, and its pushed
state entirely. The `flock` on `harness_interaction_lock` (`interaction.rs:278`)
keeps tmux writes mutually exclusive, but the Hub's `pendingRequest` and
`sendMessage` state will be stale immediately after a CLI send, and a
CLI-originated message will be reconciled as a directly-typed message. That may
be fine — but the document should say so rather than leave it undiscovered.

### 3.2 `@latch/chat-react` has a consumer

Phase 6 step 2: "Delete `@latch/chat-react` if no current application consumes
it."

It has one. `examples/remote-sdk-react/src/SessionSurface.tsx:2` imports
`AwaitingInputPrompt`, `Composer`, `useTranscript`; `TranscriptTimeline.tsx`
imports `TranscriptItem`. `packages/terminal-react` depends on `@latch/client`
and needs verifying after the events exports are removed. `docs/REMOTE_SDK.md`
documents the `/v1` compatibility rules as a published contract and is not in
the plan.

None of this is hard, but "if no consumer" reads as though the answer were
already known, and it isn't.

### 3.3 A hard protocol cutover cannot be shipped atomically to an App Store app

`GatewayCompatibility.supports` returns `false` for **every** endpoint when the
protocol major disagrees
(`apps/LatchMobile/Sources/LatchMobileKit/GatewayCompatibility.swift:60`), and
`validate` throws. So an older phone build against a v2 Mac loses the terminal
too — not just chat. The user's fallback, which the architecture explicitly
relies on ("The terminal remains the universal fallback"), is the thing that
breaks.

And the release cadences do not match: the CLI ships as one signed archive with
`latch-remote` and self-updates (`docs/ARCHITECTURE_RULES.md`, "Distribution is
one signed payload"); the phone ships through the App Store. "Ship matching
CLI, desktop, remote helper, and mobile builds" is not achievable as an atomic
step.

Minimum viable answer, without reintroducing dual protocols: keep `GET
/v1/capabilities` alive as a **tombstone** that returns protocol major 2 and an
`upgradeRequired` marker, and have the phone render "update Latch on your
phone" instead of a generic connection failure. That is a dozen lines and it is
the difference between a bad afternoon and a bricked phone with no diagnostic.

---

## Part 4 — Smaller items worth fixing while the documents are open

**Protocol contradiction on who speaks first.** The architecture says "After
authentication and permission checks, the server sends either a recent snapshot
or a resumable mutation batch." Phase 4 step 3 says "Require the first client
message to be `resume`, even for a fresh client." These disagree, and the
client-first version costs a full RTT before the first conversation byte —
100–300 ms over TURN, on exactly the path the architecture is trying to make
feel fast. Today's events socket already carries `?cursor=` on the upgrade
(`serve/events.rs:33`). Do the same: put `generation` and `afterRevision` in the
upgrade URL, let the server push immediately, and keep `resume` as the
mid-connection re-sync message.

**Request-id collisions.** `request_id()` looks for `request_id`, `requestId`,
`tool_use_id`, `uuid`, then falls back to
`permission:<tool>:<timestamp>` (`harness/permission.rs:57`). Live 2.1.228
records carry none of the first four — they carry `prompt_id`
(`fixtures/harness/claude-code/live-permission-2.1.228/raw.jsonl`). So real
permission requests land on the timestamp fallback, and two `Bash` permissions
in the same second produce the same id. Under "items are upserted by ID" they
merge into one item and one of them becomes unanswerable. Add `prompt_id` to
the key list and fold in a per-source sequence number for the fallback.

**Reserve `partial` in `message.status` now.** The architecture says whole
assistant messages are enough initially and "The model supports upserts so
partial text can be added later without a new protocol." Adding a `partial`
status later *is* a protocol change for any client that switches exhaustively
over the enum — which Swift clients do by default. Put it in the v2 schema now,
even if nothing emits it, and require clients to tolerate unknown statuses.

**One malformed line should not wedge a session.** `parse_records` collects
into `Result`, so a single bad mid-file line fails the entire parse
(`harness/mod.rs:748`). A short-lived `latch events` child recovers by dying;
a long-lived connector does not. Skip-and-count malformed records, surface the
count in state, and keep going. The ledger reader already takes this posture
(`LedgerView::pull`, `harness/mod.rs:617`) — the transcript parser should match.

**The exclusive gateway lock needs a bounded retry.** "Starting a second
gateway for the same `LATCH_HOME` fails visibly by acquiring an exclusive
gateway lock" is right, but the desktop supervises `latch serve`
(`apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessSupervisor.swift`) and a
restart that races the old process's exit will hit a lock the dying process
still holds. Acquire with a short bounded retry before failing, or the
supervisor's restart path becomes flaky in exactly the situation it exists for.

**`conversation_reset` looks redundant.** It carries "snapshot payload" and the
resume rules already say a generation mismatch always produces a snapshot. Two
messages with identical payloads and near-identical semantics is one more thing
for a client to get wrong. Either drop it, or make `snapshot` carry a `reason`
field and delete the second message type.

---

## Part 5 — What is right and should not be traded away

- **One connector per session, shared by all subscribers.** This is the
  substance of the change and it is correct.
- **Deleting the child-process relay.** Forking `latch events` per socket
  (`serve/events.rs:210`), each with its own poll loop and its own full
  transcript reparse, is the current design's worst property.
- **Stable item IDs with upsert, instead of an ordered event stream folded by
  the client.** Cursor folding is why the current mobile code needs `resync`,
  `reset()`, close-code 4422, and connector-epoch invalidation. Removing that
  category of bug is worth the rewrite by itself.
- **Refusing a generic terminal connector.** Correct, and worth keeping in the
  document as a stated non-goal so it does not get relitigated.
- **Codex as the agnosticism test rather than a claim.** Right call. Consider
  pulling one piece of it earlier: writing the *fixture corpus* for Codex during
  Phase 1 costs little and would catch connector-trait assumptions before three
  phases are built on them.
- **The performance properties in Phase 8 are stated asymptotically.** That is
  the right way to write them. Add two: steady-state `tmux` invocations per
  session per second (2.4 above), and bytes sent per subscriber per minute on
  an idle session.

---

## Suggested sequencing changes

The plan is vertical, which is right. Two adjustments:

1. **Move transcript binding (2.3) and the remote-access permission work (1.1,
   1.2) into Phase 0.** Both are preconditions. Discovering 1.1 during Phase 4
   means redesigning the socket after the mobile client is half-written;
   discovering 1.2 means Phase 0's "the terminal endpoint still works" exit
   criterion passed on loopback and the paired path was broken the whole time.
2. **Add an explicit "state derivation" work item to Phase 2**, covering 1.3
   and 1.4 together: what refreshes `ConversationState`, how often, from which
   sources, and what the client is allowed to assume between refreshes. Right
   now that behavior is spread across a sequence diagram, an exit criterion, and
   a success criterion, and no phase owns it.
