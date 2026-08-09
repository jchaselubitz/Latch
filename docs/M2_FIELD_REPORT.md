# M2 field report — the phone, over SSH

M2 exists to answer one question, and it is not a technical one:

> **Do you actually reach for your agent from your phone?**

Everything else in the milestone — attach resilience, `--retry`, `--watch`,
scrollback bounds, geometry revert — is hardening done in service of finding
that out. If the answer is no, M4's transport is not worth building, and
discovering that is a success rather than a failure.

A milestone whose purpose is to validate demand is worth nothing if the
validation is assumed. So this document has two parts: the protocol to run, and
the results, and the results section is **empty until a person with a phone
fills it in.**

---

## Status

| | |
| --- | --- |
| **Setup documented** | Yes — [`SSH_SETUP.md`](SSH_SETUP.md) |
| **Simulated verification** | Yes — `crates/latch/tests/remote_attach.rs`, 15 tests |
| **Real-device verification** | **Not performed.** See *Why this is not filled in*. |
| **Verdict** | **Not recorded.** |

### Why this is not filled in

The verification half of M2 is a physical act: an iPhone, a cell network, and
being somewhere other than home. The agent sessions that implemented M1–M2 run
in a Linux container with no phone, no Termius, no Tailscale, and — as of this
writing — no `sshd` even to hop through locally. There is no honest way to
simulate "did you reach for it?", and a fabricated verdict would defeat the
only purpose the milestone has.

What *is* verified, and verified thoroughly, is the kernel behavior underneath
each exit criterion. The simulated suite is not a substitute for the field
test, but it does mean the field test is being run against code that has
already survived a deliberately hostile link. See *What the simulation already
covers*, below, so the field test can be read as new evidence rather than a
repeat.

---

## The protocol

Do this over about a week of ordinary work, not in one sitting. The question is
about habit, and a single deliberate session cannot answer it.

### Setup, once

Follow [`SSH_SETUP.md`](SSH_SETUP.md). Confirm before leaving the house:

- [ ] `latch list` from Termius on home Wi-Fi returns your sessions.
- [ ] `latch attach --retry` from Termius shows a live screen.
- [ ] `echo $TERM` inside the SSH session prints `xterm-256color`.

### Exit criterion 1 — answer a permission prompt, away, on cell

Leave your own network. **Turn Wi-Fi off on the phone** — hotel and café Wi-Fi
is not what this criterion means, and it is easy to be on it by accident.

1. Start a Claude Code session in Latch on the Mac and give it work that will
   need permission (a file write, a command).
2. Leave. On cell, `latch attach --retry <session>`.
3. Answer the prompt.
4. Confirm the agent continued.

- [ ] **Answered a real permission prompt from cell, away from my network, and
      the agent continued.**

Record: what network, roughly how long the attach took to paint, and whether
the prompt was legible at phone width.

> _(results)_

### Exit criterion 2 — background and reopen, **every time**

Not "usually". Do this at least **20 times** across different conditions:
mid-output, on the alternate screen (Claude Code's normal state), while idle,
while moving between cell towers, and after leaving the app for long enough
that iOS kills the connection outright.

Each time, note whether the screen came back **exactly** right — no partial
paint, no wrapped lines, no stale cursor, no missing prompt.

- [ ] **20+ background/reopen cycles, screen correct every time.**

| # | Condition | Correct? | Notes |
| --- | --- | --- | --- |
| | | | |

**If any cycle is wrong, stop and read this.** That is the M1 snapshot path
failing under a condition the fixtures do not cover. The fix belongs in
`latch-term` with a new fixture reproducing it — capture the raw stream into
`fixtures/vt/` if you can. It does **not** belong in a client workaround, and a
client-side repaint that hides it makes the underlying defect unfindable.

> _(results)_

### Exit criterion 3 — desk geometry survives the phone

1. Note the desk session's size (`latch inspect NAME`).
2. Phone attaches with control at phone width; watch the desk reflow.
3. Disconnect the phone **both ways**: once by quitting Termius cleanly, and
   once by killing the network mid-session (airplane mode) — the second is what
   a phone actually does.
4. Check the desk size and screen after each.

- [ ] **Desk geometry restored after a clean disconnect.**
- [ ] **Desk geometry restored after a dropped disconnect.**
- [ ] **Desk screen contents intact, not just the size.**

> _(results)_

### Exit criterion 4 — a dropped connection loses no session state

With something stateful running (a shell with variables set, an agent mid-turn,
a long build), drop the link hard — airplane mode, not a clean exit — and
reattach.

- [ ] **No output lost across the drop.**
- [ ] **The child never noticed** (no `SIGHUP`, no re-prompt, work continued).

> _(results)_

### The actual question

Over the week, **without being prompted by this checklist**, count the times
you reached for the phone to check or steer an agent. Not test attaches — real
ones, because you wanted to know something or unblock something.

| | |
| --- | --- |
| Unprompted reaches, week of ____ | |
| Of those, how many mattered | |
| Times you wanted to and didn't, and why | |

> **Verdict:** _(not recorded)_
>
> _Answer plainly: yes, no, or "only for X". "Only for X" is the most useful
> answer available, because it tells M4 what to build for. If the honest answer
> is that you did not reach for it, say so — that is a finding, and it is worth
> more than a transport nobody wanted._

---

## What the simulation already covers

`crates/latch/tests/remote_attach.rs` runs the real `latch` binary against a
cuttable socket proxy standing in for the SSH tunnel. It can truncate a
connection mid-frame and then carry a healthy one, which is how reconnection is
observed against the actual client rather than a mock. Killing the worker
instead would test a much easier claim — that a dead session is detected —
rather than that a live session survives a dead link.

Covered there:

- Transport death mid-frame, mid-snapshot, and mid-handshake; detected rather
  than hung; session unaffected; reattach reproduces the screen exactly.
- Repeated rapid detach/reattach, including on the alternate screen and while
  output flows, with no state or control leakage across cycles.
- `--retry` backoff bounds, giving up rather than looping forever, and never
  escalating a watcher to a controller or taking a steal it was not asked for.
- A constrained link driving the M1 backpressure path: resync by snapshot
  rather than unbounded buffering, with the PTY read never blocked.
- D4 geometry revert end to end, including a drop rather than a clean close.
- `--watch` taking no control and never resizing.
- The scrollback bound holding on a slow link.

Two kernel defects were found by writing those tests, both invisible to the M1
fixtures, and both are exit-criterion failures the field test would otherwise
have hit:

1. The interactive loop polled the socket without first draining frames the
   previous read had already decoded. One read routinely carries both the
   `attached` frame and the snapshot behind it, so on an **idle** session the
   screen sat in the client's own buffer forever — precisely the "is this
   frozen?" failure this milestone exists to remove.
2. `Registry::attach` emitted the snapshot *before* the resize the new
   controller had just caused. A 40-column phone taking control of a 200-column
   session was sent 200 columns and painted it wrapped into 40, **every time**.
   That is exit criterion 2 failing by one line of ordering.

Both are fixed with regression tests at the level where the decision lives.

## What the simulation cannot cover

Read these as the specific reasons the field test is not redundant:

- **Real cell latency and jitter.** The 32 KiB scrollback ceiling is sized
  against an *assumed* 250 kbit/s bad link, not a measured one
  ([`DECISION_SCROLLBACK.md`](DECISION_SCROLLBACK.md)).
- **iOS backgrounding.** The suite simulates a dropped socket; it cannot
  simulate what iOS does to a suspended app's TCP connection, or Termius's own
  reconnect behavior layered on top of ours.
- **Snapshot size on a real link.** An adversarially colored 200×50 screen
  serializes to ~109 KB, and it is sent on *every* attach and every resync.
  Nothing in M2 addressed that, and a phone is where it would first be felt.
- **Legibility at 40 columns.** Whether a Claude Code permission prompt is
  *usable* on a phone is not a property any test asserts.
- **Whether you reach for it.** The only thing that actually matters here.
