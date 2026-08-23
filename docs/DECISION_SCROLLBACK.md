# Decision: history on attach, and how long an exited session is kept

**Status:** decided, M2.
**Choices:**

1. A client that attaches is sent the **newest 200 lines** of the session's
   structured scrollback ring, under a hard ceiling of **32 KiB**, before the
   screen. A backpressure resync is sent no history at all. There is no paging.
2. An exited session stays on disk, readable, for **24 hours**, after which
   `latch prune` reclaims it. `latch prune --all` reclaims it sooner.

**Scope:** `manage::DEFAULT_EXITED_RETENTION`.

> **Partly superseded.** Choice 1 and everything below about history on attach
> describes the in-process terminal worker (`latch_term::HistoryPolicy`,
> `Screen::attach_snapshot`, `WorkerTuning`), which was archived under the
> `archive/latch-term-v1` tag. Latch no longer sends scrollback on attach at
> all: the patched kernel paints the **current frame** once and then forwards
> the pane's own bytes, so there is no history payload to bound. See
> [`DECISION_EXCLUSIVE_ATTACH.md`](DECISION_EXCLUSIVE_ATTACH.md).
>
> Choice 2 — 24-hour retention for exited sessions, reclaimed by `latch prune`
> — is current and is what the rest of this document is still authority for.

This closes open item 2 in
[`../planning/IMPLEMENTATION_PLAN.md`](../planning/IMPLEMENTATION_PLAN.md).

---

## The tension

Enough history to be useful when you pick up your phone mid-task; bounded enough
that reattaching on a cell network is not a transfer you wait through.

The second half of that is sharper than it first appears, and it is what set the
numbers. **On iOS every app-backgrounding is a detach and every return is a
reattach.** History on attach is therefore a *per-glance* cost on a metered
link, not a once-per-session one. A policy that costs 200 KB is not a 200 KB
decision; it is a 200 KB decision multiplied by how often you look at your
phone.

---

## What the measurements said

Every recorded stream in [`../fixtures/vt/`](../fixtures/vt) was replayed
through the screen model and its scrollback ring serialized. The suite that
keeps these numbers honest is
[`../crates/latch-term/tests/attach_history.rs`](../crates/latch-term/tests/attach_history.rs).

### Agent sessions produce no scrollback at all

| Recorded case | Alternate screen | Lines in the ring | History bytes | Screen bytes |
| --- | --- | --- | --- | --- |
| `claude-code-startup` | yes | 0 | 0 | 1144 |
| `claude-code-turn` | yes | 0 | 0 | 1475 |
| `claude-code-trust-prompt` | no | 0 | 0 | 1035 |
| `claude-code-resize-alt-screen` | yes | 0 | 0 | 1047 |
| `codex-startup` | no | 0 | 0 | 935 |
| `cursor-rewrite-progress` | no | 0 | 0 | 300 |
| `high-rate-output` | no | 49971 | 538575 | 265 |

This is the most important number in the table and it is a zero. Claude Code
runs on the alternate screen, which by definition has no scrollback. Codex
produces 12 KB of output and scrolls **nothing**, because it rewrites the
visible screen rather than appending to it.

So for the case M2 exists for — reaching an agent from a phone — the snapshot
alone is already the complete answer, and the history policy costs nothing.
That is not an argument for skipping the feature: a Latch session is a *shell*,
and the agent is one command inside it. You also reattach after the agent exits,
before you start one, or having run `cargo test` directly. But it is the reason
this decision is not the load-bearing one it looked like from the plan.

### A line count bounds nothing a link cares about

| Content | Columns | Bytes per line (p50) | Bytes per line (max) |
| --- | --- | --- | --- |
| Recorded `high-rate-output` | 100 | 11 | 11 |
| Build log, some color | 80 | 65 | 86 |
| Build log, some color | 200 | 65 | 206 |
| A distinct color in every cell | 80 | 1098 | 1125 |
| A distinct color in every cell | 200 | 2722 | 2761 |

Two orders of magnitude between the ordinary case and the adversarial one — and
the adversarial one is not contrived. It is a progress bar, a syntax-highlighted
diff, `htop`. At 2.7 KB per line, 200 lines is **545 KB**.

A byte ceiling is therefore mandatory rather than defensive. The line count is
the *useful* bound; the byte ceiling is what makes it a bound at all.

### What 200 lines actually costs

| Content | Columns | 200 lines |
| --- | --- | --- |
| Recorded `high-rate-output` | 100 | 2.2 KB |
| Build log, some color | 80 | 11.2 KB |
| Build log, some color | 200 | 14.5 KB |

---

## Why 200 lines

Two hundred lines is four screens at a desk geometry (50 rows) and ten on a
phone (20 rows).

The question you have when you pick up your phone is *what did it just do* —
the tail of a build, the failing test, and the command above it that started
them. Two hundred lines covers that with room to spare. Past it you are reading
a log, and reading a log on a phone over a metered link is a different activity
that a bigger number would serve badly anyway.

It costs about 14 KB of real build output — roughly half a second on the bad
link below, and imperceptible on a good one.

## Why 32 KiB

At **250 kbit/s** — a genuinely bad cell link, one bar or a congested tower —
32 KiB is about one second. One second is where a reattach stops reading as
instant and starts reading as loading, and on a phone the alternative to
reattaching is seeing nothing at all. Past that point the user would rather have
less history than wait.

The ceiling only binds when lines are unusually expensive, which is exactly when
it should: it trades history depth for responsiveness, in the direction that
keeps the screen arriving quickly.

Lines are whole or absent. A line that will not fit is dropped rather than
truncated — half a line of build output is a claim the session never printed,
and the truncated end of a log line is where the error message is.

### What the ceiling does *not* cover

The visible screen. It is not optional and its size is set by the session's
geometry, so folding it into the history budget would mean either a policy no
geometry could satisfy or a screen delivered in pieces.

Worth recording while it is in view: the adversarial screen at 200×50
serializes to **109 KB** on its own. On a slow link that, not history, is the
thing that hurts — and it is sent on every attach *and* every resync. Nothing in
M2 addresses it. If reattach on a real link ever feels slow and the history
block is not the cause, this is where to look.

---

## Where history comes from, and why not the journal

From the **screen model's structured scrollback ring**, not the on-disk journal.

The journal is a byte log trimmed at an arbitrary offset. Replaying its head
into a fresh terminal can begin mid-escape-sequence, paint part of a redraw, and
restore no mode set before the window — which is precisely the byte-tail replay
the screen model exists to avoid, and the reason
[`../planning/PROJECT_ARCHITECTURE.md`](../planning/PROJECT_ARCHITECTURE.md)
rejects it for reattach. Using it for history would reintroduce the failure one
line above the part of the screen that was carefully protected from it.

The ring is already bounded, already structured, and already counts what it
dropped.

The journal keeps its own job: the raw record, for `latch inspect` and for
anything later that wants bytes rather than a screen.

## Where the history goes in the payload

There is exactly one position that works, and it is why the composition lives in
`latch-term` as `Screen::attach_snapshot` rather than being assembled by the
worker out of two calls:

```text
\x1bc                        hard reset
<history rows>               each: SGR runs, text, CR LF
<rows × LF>                  scrolls the block above the visible screen
\x1b[?1049h | \x1b[?1049l    the screen begins here
<contents_formatted>         the visible screen
<input modes, extras>
```

* It cannot go **before the reset**: a hard reset clears the receiving
  terminal's saved lines, so history sent first would be erased by it.
* It cannot go **after the screen**: it has to scroll *past* the screen to reach
  the client's scrollback, and the screen paint positions absolutely.

The trailing newlines are not padding. Without them the newest history lines sit
in the visible region and are erased by the screen paint that follows — and the
newest lines are the ones worth having. Exactly `rows` of them are emitted,
which leaves precisely one blank line between the history and the screen in the
client's scrollback, wherever printing the block left the cursor.

`\r\n` rather than `\n`, because a line as wide as the screen leaves a deferred
wrap pending and a carriage return is what cancels it.

The alternate-screen statement doubles as the marker separating the two halves.
Nothing above it can imitate it: cells hold printable graphemes, so the only
escapes the history block contains are the SGR sequences it writes itself.

## A resync carries no history

`Effect::Snapshot` is produced by `Registry::attach` and by nothing else. The
other place a snapshot is sent — a client whose bounded queue overflowed — calls
`Screen::snapshot` directly.

That client already received its history when it attached, and a link slow
enough to overflow a queue is the last one to send scrollback down twice.
Re-sending it would turn the mechanism that *protects* a slow link into the
thing that saturates it.

## No paging

The plan left room for paging "if the snapshot alone proves insufficient". It is
not being built, for two measured reasons:

1. **For the agent case there is nothing to page.** The ring is empty while an
   alternate-screen application is running. Paging over it would return zero
   rows however many times it was asked.
2. **For the shell case the client's own terminal is the pager.** Everything
   sent lands in Termius's (or iTerm's) scrollback, where the platform's own
   scrolling already works, with gestures a phone user already knows.

Paging also costs protocol surface — a request/response round trip — in a
milestone whose entire point is that reconnect is just attach, with no second
code path for a broken snapshot to hide in.

**What would justify building it:** wanting history older than the bound,
repeatedly, in real use — not once, and not hypothetically. That is a thing
dogfooding in objective 4 can observe. The cheaper answers to try first are
raising `attach_history_lines`, and `latch inspect`'s access to the journal.

---

## How long an exited session is kept

**24 hours**, then `latch prune` reclaims it. `--all` reclaims regardless of
age. A `lost` session — no socket, no exit record — is reclaimed on sight,
because it has nothing to show.

The question this answers is: *I was away from my desk and something finished —
can I still see how it went?* The honest span of being away is overnight.
Shorter, and the answer is no for the ordinary case. Longer, and `latch prune`
stops being something you can run without thinking, which makes it something you
stop running.

Nothing prunes automatically. There is no daemon, and putting a filesystem sweep
on session creation would put it on the startup path of every terminal window —
the one cost the architecture is most protective of.

### What "still attachable" means concretely

The kernel runs with `remain-on-exit on`, so a pane whose command has finished
keeps its window and its final grid. Attaching to a retained session paints
that last screen through the ordinary exclusive attach, reports how the session
ended, and exits with the kernel's `session_exited` reason. It is a frame, not
a journal to be replayed, and there is nothing left to type at.

This is not a read-only attach — that no longer exists as a mode. It is the
same single-surface attach, over a pane that has already exited.

`--retry` does not treat an exited session as a link that might come back.

`latch prune` is what makes the last screen unavailable; until then it is
there. A session whose kernel was killed outright keeps no grid and still
reports its exit, because the screen is a courtesy and a courtesy that becomes
a precondition is a new way to fail.

---

## What would change these decisions

**The history bound:**

- A recorded agent fixture starts producing scrollback. The 200/32 KiB split was
  chosen on the basis that agent sessions contribute none; if Claude Code or
  Codex begin appending to the primary screen, the per-glance cost changes and
  the numbers should be re-derived. `attach_history.rs` asserts this and will
  fail rather than drift.
- Reattach on a real cell link measurably drags, and the history block is the
  cause rather than the screen. Lower the byte ceiling first; it is the bound
  that a link feels.
- M4's transport gains compression or delta encoding, at which point the byte
  ceiling is measuring the wrong thing and should be restated in terms of what
  actually crosses the link.

**The retention window:**

- Session directories accumulate to a size somebody notices. The journal cap is
  10 MB, so 24 hours of exited sessions is bounded by how many sessions a day
  you open — with a terminal profile pointed at `latch`, that could be dozens.
  If it becomes a real number, shorten the window rather than adding a daemon.
- Coming back to a finished session after more than a day turns out to be
  ordinary. Lengthen it; the cost is disk, and disk is cheap next to losing the
  screen you came back for.
