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
[Part 6](#part-6--recommendations-by-owning-layer) restates every one of
them as a recommendation, sorted by the layer that owns the fix, and ends
with an ordered worklist.

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

**4.1 — Protocol contradiction on who speaks first.** The architecture says "After
authentication and permission checks, the server sends either a recent snapshot
or a resumable mutation batch." Phase 4 step 3 says "Require the first client
message to be `resume`, even for a fresh client." These disagree, and the
client-first version costs a full RTT before the first conversation byte —
100–300 ms over TURN, on exactly the path the architecture is trying to make
feel fast. Today's events socket already carries `?cursor=` on the upgrade
(`serve/events.rs:33`). Do the same: put `generation` and `afterRevision` in the
upgrade URL, let the server push immediately, and keep `resume` as the
mid-connection re-sync message.

**4.2 — Request-id collisions.** `request_id()` looks for `request_id`, `requestId`,
`tool_use_id`, `uuid`, then falls back to
`permission:<tool>:<timestamp>` (`harness/permission.rs:57`). Live 2.1.228
records carry none of the first four — they carry `prompt_id`
(`fixtures/harness/claude-code/live-permission-2.1.228/raw.jsonl`). So real
permission requests land on the timestamp fallback, and two `Bash` permissions
in the same second produce the same id. Under "items are upserted by ID" they
merge into one item and one of them becomes unanswerable. Add `prompt_id` to
the key list and fold in a per-source sequence number for the fallback.

**4.3 — Reserve `partial` in `message.status` now.** The architecture says whole
assistant messages are enough initially and "The model supports upserts so
partial text can be added later without a new protocol." Adding a `partial`
status later *is* a protocol change for any client that switches exhaustively
over the enum — which Swift clients do by default. Put it in the v2 schema now,
even if nothing emits it, and require clients to tolerate unknown statuses.

**4.4 — One malformed line should not wedge a session.** `parse_records` collects
into `Result`, so a single bad mid-file line fails the entire parse
(`harness/mod.rs:748`). A short-lived `latch events` child recovers by dying;
a long-lived connector does not. Skip-and-count malformed records, surface the
count in state, and keep going. The ledger reader already takes this posture
(`LedgerView::pull`, `harness/mod.rs:617`) — the transcript parser should match.

**4.5 — The exclusive gateway lock needs a bounded retry.** "Starting a second
gateway for the same `LATCH_HOME` fails visibly by acquiring an exclusive
gateway lock" is right, but the desktop supervises `latch serve`
(`apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessSupervisor.swift`) and a
restart that races the old process's exit will hit a lock the dying process
still holds. Acquire with a short bounded retry before failing, or the
supervisor's restart path becomes flaky in exactly the situation it exists for.

**4.6 — `conversation_reset` looks redundant.** It carries "snapshot payload" and the
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

## Part 6 — Recommendations, by owning layer

Sorting the findings by which layer owns the fix is worth doing for its own
sake: it is a test of the boundary the architecture claims to draw. Four
layers are in play.

| Layer | What it owns | Must not know |
| --- | --- | --- |
| **Gateway edge** — `latch-remote`, `serve::http`, `serve::auth` | routing, Noise, device grant, credential injection, route allowlist | conversations, items, connectors |
| **Conversation Hub** — actor, cache, protocol | ordering, revisions, generations, backpressure, idempotency, subscriber fanout, scheduling | Claude, Codex, transcripts, panes, hooks |
| **Agent connector** — `connectors/claude`, `connectors/codex` | source discovery, parsing, branch, request lifecycle, action application | sockets, subscribers, ordinals, revisions |
| **Client** — `LatchMobileKit` | rendering, local cache, optimistic UI, retry | any of the above |

The split lands cleanly for most findings, which is a good sign for the design.
Where it does *not* land cleanly — 1.1, 1.4, 2.2 — the cause is the same in
each case: **the connector trait as specified is missing something, so the
knowledge leaks into the Hub.** Section 6.5 collects those into four trait
changes that resolve most of this list at once. Read that first if you read
only one section.

### 6.1 Gateway edge

**1.2 — `/v1/` hard-coding in the remote allowlist.** *Do this in Phase 0, same
commit as the router.*

Do not hand-edit the prefix. `permission_for_request`
(`remote_access.rs:1749`) is a second, hand-maintained copy of the route table
that lives in `serve::http::run`. That duplication is why the plan missed it.
Replace both with one shared table:

```rust
// One definition, consumed by the router and by the remote allowlist.
const ROUTES: &[Route] = &[
    Route { method: Get,  path: "/v2/capabilities",              grant: Observe },
    Route { method: Get,  path: "/v2/sessions",                  grant: Observe },
    Route { method: Get,  path: "/v2/sessions/{id}",             grant: Observe },
    Route { method: Get,  path: "/v2/sessions/{id}/terminal",    grant: Control, read_only_grant: Some(Observe) },
    Route { method: Get,  path: "/v2/sessions/{id}/conversation",grant: Observe },
];
```

Then add a test that every route the router serves appears in the table, so a
future endpoint cannot be silently unroutable through the tunnel. Phase 0's
"the terminal endpoint still works through its v2 path" must be exercised
**through the Noise tunnel**, not on loopback — `RemoteAccessEndToEndTests`
already has the harness for it.

**1.1 — carrying the device grant to the Hub.** *Phase 0 for the mechanism,
Phase 4 for enforcement.*

The edge's job is only to *state* the grant; the Hub's job is to enforce it
(6.2). `authorize_and_inject` already rewrites headers and already rejects
client-supplied `Authorization` and `Proxy-Authorization`
(`remote_access.rs:1691`), so injecting a grant header is safe by exactly the
argument that makes token injection safe:

```
X-Latch-Grant: interact
```

`serve` must reject the header on any connection that did not arrive from the
loopback proxy, and default to `control` for a direct loopback client (the CLI
and desktop on the same machine already have full authority). Add the header to
the forbidden-inbound list in the same match arm as `Authorization` so a remote
client cannot supply its own.

The alternative — per-device gateway tokens carrying a scope — is cleaner in
principle and more work: it changes token minting, rotation, and revocation.
The header is the right first step.

**3.3 — release skew.** *Phase 0, ~a dozen lines.*

Keep `GET /v1/capabilities` as a tombstone: it returns `protocolVersion: 2` and
nothing else, which is enough for `GatewayCompatibility.validate` to throw
`unsupportedProtocol(reported: 2, supported: 1)` on the old build. Then change
the *phone* — in a release shipped **before** the v2 Mac build — to render that
specific error as "Update Latch on this phone" rather than a generic failure.
That ordering matters: the phone fix has to be in the field first, so ship it
now, ahead of the rest of the work.

This is not a dual protocol. It is a version tombstone, and the plan's "no dual
protocol" rule should be amended to say so explicitly, or someone will delete it
during Phase 6 cleanup.

**4.5 — gateway lock retry.** *Phase 3.*

Acquire the exclusive home lock with a bounded retry — roughly 2 s at 50 ms
intervals — before failing. `RemoteAccessSupervisor` restarts `latch serve`,
and a restart that races the dying process's exit must not turn into a visible
error in exactly the situation the supervisor exists to handle. Fail loudly
only after the retry window.

### 6.2 Conversation Hub

**1.1 (enforcement half) — refuse operations above the grant.** The Hub holds
the grant per subscriber and checks it before dispatching to the connector. It
must not know that "send_message means interact" — see 6.5, change 1. Refusal
is an `operation_result { accepted: false, reason }`, not a socket close; a
read-only phone showing a disabled composer is a normal state, not an error.

**2.2 — the Hub assigns ordinals; connectors never do.** *Phase 1.*

This is the single highest-leverage rule in the review. Connectors emit
`(id, kind, payload, source_timestamp)` in observation order. The Hub stamps
`ordinal` monotonically at ingest and never renumbers.

- The two-source merge problem (transcript vs. hook sidecar) disappears: a late
  hook record lands at the current ordinal with an earlier `createdAt`.
- `beforeOrdinal` pagination becomes sound.
- Two clients cannot render different orders.
- It carries `docs/ARCHITECTURE_RULES.md`'s existing rule forward unchanged.

Write into the protocol schema: **clients sort by `ordinal`; `createdAt` is
display metadata only.** Add a reducer test that an out-of-order `createdAt`
does not move an item.

**2.4 — backpressure.** *Phase 3.*

Replace "drop and snapshot" with a three-tier degrade:

1. queue bounded in **bytes** as well as count;
2. on overflow, the queued mutations are **replaced by** a pending-snapshot
   marker — not appended behind them — and the snapshot is built at send time,
   not at overflow time, so repeated overflows collapse into one;
3. rate-limit snapshots per subscriber (say one per 2 s); if a subscriber
   overflows again inside that window, send `state_changed` only and let the
   client pull history at its own pace.

Test it over a **throttled** link, not a stalled reader. A reader that stops
reading and a link that delivers 20 kB/s fail differently, and only the second
one is what actually happens on TURN.

**2.5 — split the actor into two lanes.** *Phase 3.*

| Lane | Runs | Blocking | Owns |
| --- | --- | --- | --- |
| State lane | the actor mailbox | never | items, revision, generation, fanout |
| Action lane | `spawn_blocking`, one at a time per session | yes, with a timeout | `connector.apply` |

An action is dispatched from the state lane, executes off it, and returns as an
ordinary mailbox message. A wedged `tmux` then costs one pending operation, not
the session's entire broadcast. Give `apply` a hard timeout (5 s is generous for
five `tmux` spawns) and surface expiry as a refusal with a distinct reason so
the client can offer retry.

Add to the Phase 3 concurrency tests: **"a connector action that never returns
does not stall source mutations or fanout."**

**2.6 — make `operationId` survive a restart.** *Phase 3.*

Accepted operation ids are tiny. Persist them next to the connector checkpoint
(id, outcome, timestamp; bounded ring, TTL to match today's 10 minutes at
`serve/http.rs:43`). Keep `gatewayInstanceId` in `gateway-capabilities.v2` and
define the client rule explicitly: **on a changed instance id with an
in-flight operation, do not auto-retry — surface a manual retry.** Persisting
covers the common restart; the instance id covers cache eviction and the case
where the whole home was rebuilt.

Add to the Phase 8 failure list: "restart `latch serve` mid-send, then retry the
same operation id — assert exactly one message reaches the composer."

**1.3 (Hub half) — derive `pendingRequest`, do not track it.** *Phase 1.*

`ConversationState.pendingRequest` should be a **projection** computed by the
reducer — the newest `request` item whose status is `pending` — not an
independent field a connector sets. If both the connector and the Hub track
pending-ness they will diverge, and the divergence will present as a button
that does nothing. One source of truth: item status.

**4.1 — let the server speak first.** *Phase 4.*

Drop "the first client message must be `resume`." Put `generation` and
`afterRevision` on the upgrade URL, exactly as `?cursor=` works today
(`serve/events.rs:33`), and have the server push the snapshot or the mutation
batch immediately after the handshake. That removes a full round trip —
100–300 ms over TURN — from every cold open and every background/foreground
resume, on the path the architecture is explicitly trying to make feel fast.

Keep `resume` as a mid-connection message for the case where the client wants to
re-sync without reconnecting. The architecture text and Phase 4 currently
contradict each other here; this resolves it in favor of the architecture.

**4.3 — reserve `partial` in `message.status` now.** *Phase 0, schema only.*

Swift enums switch exhaustively. Adding `partial` later is a breaking change for
every shipped client, which defeats the stated reason for choosing upserts. Put
it in the v2 schema now with nothing emitting it, and require clients to render
an unknown status as `complete` rather than failing to decode.

**4.6 — delete `conversation_reset`.** *Phase 0.*

It carries a snapshot payload, and the resume rules already say a generation
mismatch produces a snapshot. Two message types with identical payloads is one
more thing for a client to get wrong. Add an optional `reason` to `snapshot`
instead.

**3.1 (Hub half) — a local subscriber path.** *Phase 4 or later.*

Whatever replaces `latch events` should be a **thin in-process Hub subscriber**,
not a second pipeline: `latch conversation <session> --json --follow` attaches
to the same actor and prints the same normalized items. That keeps one
observation implementation, which is the point of the whole exercise, and it
gives Overlord a successor contract (6.4).

### 6.3 Agent connector

Everything here is Claude-specific knowledge and must not appear in the Hub.
Each of these becomes a row in the connector conformance suite that Codex has
to satisfy too — which is the real value of writing them down now rather than
in Phase 7.

**2.3 — bind the transcript authoritatively, never by mtime.** *Before Phase 2.
This is the highest-value fix on the list per line of code.*

Register a `SessionStart` hook alongside the existing `PermissionRequest` one
(`harness/mod.rs:263` already generates `hooks.json`). Its payload carries
`session_id` and `transcript_path` — the live fixture proves it
(`fixtures/harness/claude-code/live-permission-2.1.228/raw.jsonl`). Pin both on
first observation, store them in the checkpoint, and verify file identity
against them thereafter.

`detect()` gains a third outcome: `Pending` — supported connector, binding not
yet observed — which the Hub renders as `phase: starting`. Delete
`newest_jsonl` (`harness/mod.rs:1208`); do not port it. Guessing is worse than
waiting, because a wrong guess under a checkpointed connector is a
generation-reset loop, not a cosmetic error.

**2.1 — classify each appended record instead of rebuilding.** *Phase 2.*

Persist the **active-chain uuid list** (O(items already in the snapshot),
compacted alongside it) rather than the full record graph. On each append:

| `parentUuid` of the new record | Emit | Cost |
| --- | --- | --- |
| == current chain tail | `Upsert` | O(1) — the overwhelming majority |
| an earlier uuid on the active chain | `TruncateAfter(that item)` then `Upsert` | O(truncated items) |
| unknown / off-chain | `Rebuild` | O(transcript), rare |

The middle row is the one that matters. Interrupts and rewinds are routine in
Claude and today they hard-fail the stream (`harness/mod.rs:409`). If every
rewind becomes a rebuild plus a new generation plus a full snapshot to every
subscriber, generation resets are common, and the architecture treats them as
exceptional. This also gives `items_removed` a job — as specified it has none,
since every invalidating change is defined to produce a new generation instead.

Add an explicit rule for sidechain records: they append continuously during a
`Task` and never move the chain tail, so they must not trigger reclassification.

**1.3 (connector half) — define the request lifecycle rule.** *Phase 2.*

No Claude source emits "resolved." The connector must own the inference, and the
existing heuristic is the right one — promote it from the phone
(`Transcript.swift:84`) into the connector:

> A `pending` request becomes `dismissed` as soon as any later source record
> other than that same request is observed. It becomes `resolved` when Latch
> itself applied the resolution, or when the pre-action screen check confirms
> the prompt is gone.

Then make it a conformance rule that binds Codex identically: **no connector may
leave a request `pending` once a later item has been observed.** Amend Phase 1's
exit criterion, which currently forbids exactly this inference, to say that the
mutation must be explicit *from the connector* — not that the connector needs an
explicit source signal, because none exists.

**1.4 (connector half) — make state refresh cheap and self-throttling.**
*Phase 2.*

`capture_pane` and `inspect` are `tmux` subprocess spawns (`engine.rs:687`,
`:644`). The connector must:

- hash the captured pane and report **unchanged** so the Hub emits no mutation
  and no revision is burned;
- never capture more than once per Hub poll, whatever the poll cadence;
- treat a capture failure as `unavailable` with a reason, not as an error that
  kills the connector task.

The cadence itself is a Hub decision (6.5, change 2). One useful consequence:
the same event-driven refresh also catches third-party changes — a prompt
answered at the computer, or a `latch send` from another process — so 1.3, 1.4,
and the CLI-coexistence half of 3.1 are all served by one mechanism.

**4.2 — fix request-id derivation.** *Phase 2, small.*

`request_id()` checks `request_id`, `requestId`, `tool_use_id`, `uuid`, then
falls back to `permission:<tool>:<timestamp>` (`harness/permission.rs:57`). Live
2.1.228 records carry none of the first four — they carry `prompt_id`. So real
permission requests land on the timestamp fallback, and two `Bash` permissions
in the same second collide into one item under upsert-by-id, leaving one of them
unanswerable. Add `prompt_id` to the key list, and give the fallback a
per-source monotonic sequence rather than a timestamp.

**4.4 — one malformed record must not wedge a session.** *Phase 2, small.*

`parse_records` collects into `Result`, so a single bad mid-file line fails the
whole parse (`harness/mod.rs:748`). A short-lived `latch events` child recovered
by dying; a long-lived connector does not. Skip and count malformed records,
expose the count in connector state, and keep going — the ledger reader already
takes this posture (`harness/mod.rs:617`).

### 6.4 Packaging and product scope

**3.1 — decide about Overlord.** `planning/OVERLORD_INTEGRATION.md` names
`latch events` as the observation contract in five places. Either ship the
successor from 6.2 (`latch conversation --json --follow`) and update that
document in the same release, or state in the plan that Overlord integration is
knowingly broken and the document is retired. Silently deleting a documented
contract for a separate product is the one thing that should not happen.

Also write down the CLI/Hub coexistence rule, which the plan never states:
`latch send` and `latch capabilities` remain separate processes
(`main.rs:248`, `:306`). They already serialize against the Hub through the
`harness_interaction_lock` flock (`interaction.rs:278`); what they do *not* do
is tell the Hub its cached state is stale. The event-driven refresh from 1.4
covers this — say so explicitly rather than leaving it to be discovered.

**3.2 — `@latch/chat-react` has a consumer.** `examples/remote-sdk-react`
imports `useTranscript`, `Composer`, `AwaitingInputPrompt`, and
`TranscriptItem`. Decide in Phase 6: rebuild the example on v2, or delete
example and package together. `packages/terminal-react` depends on
`@latch/client` and needs a build check after the events exports go.
`docs/REMOTE_SDK.md` documents the `/v1` boundary rules as a published contract
and is not currently in the plan.

### 6.5 The four changes that do most of the work

Three findings resisted a clean layer assignment — 1.1 (grant enforcement), 1.4
(state refresh cadence), 2.2 (ordinals). In each case the cause is the same: the
connector trait as drafted does not carry something the Hub needs, so the Hub
has to infer it, and inferring it means knowing something agent-specific. Four
changes to the trait and mutation vocabulary fix that, and most of Parts 1 and 2
fall out:

```text
detect(metadata)          -> Unsupported | Pending { reason } | Supported { id, version }
load(checkpoint)          -> Projection + Position | CacheIncompatible
poll(budget)              -> Vec<Mutation>            // sources AND live state, one cadence
actions()                 -> Vec<ActionDescriptor>    // { id, required_grant, enabled, reason }
apply(action_id, payload) -> Accepted { correlation } | Refused { reason }   // blocking, budgeted
reconcile(outstanding, observed) -> Vec<Mutation>
checkpoint()              -> bytes                    // bounded by active items, never by history

Mutation =
  | Upsert(Item)            // Hub assigns the ordinal
  | TruncateAfter(ItemId)   // same generation
  | State(ConversationState)
  | Rebuild { reason }      // Hub bumps the generation
```

1. **`ActionDescriptor` carries `required_grant`.** The Hub enforces the grant
   without knowing what an action means. Adding an action later — a Codex-only
   operation, an interrupt, a mode switch — needs no Hub change. Resolves the
   enforcement half of 1.1 and keeps invariant 3 honest.
2. **`poll(budget)` covers sources *and* live state.** One cadence, one place
   that does I/O, one place to instrument. The connector self-throttles the
   expensive part (pane capture) inside its budget; the Hub schedules — fast
   after an append or an action, slow (1–2 s) when idle, never while no
   subscriber is attached. Resolves 1.4, and gives Phase 8 one number to
   measure instead of two.
3. **`TruncateAfter` in the mutation vocabulary.** Rewinds stop being
   generation resets. Resolves 2.1 and gives `items_removed` a purpose.
4. **Connectors never emit `ordinal`, `revision`, or `generation`.** They emit
   in observation order; the Hub stamps. Resolves 2.2 and makes the multi-source
   merge a non-problem by construction.

Two supporting rules worth writing into the trait doc: **`poll` is the only
place a connector may perform I/O or spawn a process**, and **`apply` is the
only place a connector may mutate the agent.** Those two sentences are what make
the Hub's scheduling and lane-splitting (6.2) enforceable rather than
aspirational.

### 6.6 Ordered worklist

| # | Finding | Layer | When |
| --- | --- | --- | --- |
| 1 | Phone renders `unsupportedProtocol` as "update this phone" (3.3) | Client | **Ship now**, ahead of everything |
| 2 | Shared route/grant table; `/v2` in the remote allowlist (1.2) | Gateway edge | Phase 0 |
| 3 | `X-Latch-Grant` injection + inbound rejection (1.1a) | Gateway edge | Phase 0 |
| 4 | `/v1/capabilities` tombstone (3.3) | Gateway edge | Phase 0 |
| 5 | Ordinal ownership; `partial` reserved; drop `conversation_reset` (2.2, 4.3, 4.6) | Hub schema | Phase 0 |
| 6 | Four trait changes (6.5) | Boundary | Phase 1 |
| 7 | `pendingRequest` derived from item status (1.3a) | Hub | Phase 1 |
| 8 | `SessionStart` binding; delete `newest_jsonl` (2.3) | Connector | Phase 2 |
| 9 | Chain-classified appends with `TruncateAfter` (2.1) | Connector | Phase 2 |
| 10 | Request lifecycle rule + conformance test (1.3b) | Connector | Phase 2 |
| 11 | Pane hashing and self-throttling (1.4b) | Connector | Phase 2 |
| 12 | `prompt_id` in request ids; skip malformed records (4.2, 4.4) | Connector | Phase 2 |
| 13 | Two-lane actor; byte-bounded queues; tiered overflow (2.5, 2.4) | Hub | Phase 3 |
| 14 | Persisted operation ledger + instance-id client rule (2.6) | Hub + Client | Phase 3 |
| 15 | Bounded lock-acquire retry (4.5) | Gateway | Phase 3 |
| 16 | Server speaks first on the upgrade (4.1) | Hub protocol | Phase 4 |
| 17 | `latch conversation --json --follow`; Overlord decision (3.1) | Hub + scope | Phase 4+ |
| 18 | chat-react / example / REMOTE_SDK decision (3.2) | Packaging | Phase 6 |

Rows 1–5 are the ones that are cheap now and expensive later: each is a
schema, route table, or shipped-client change that becomes a migration once
anything depends on it.

---

## Suggested sequencing changes

6.6 has the per-item schedule. This is the reasoning behind the two phase
moves it assumes. The plan is vertical, which is right; two adjustments:

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
