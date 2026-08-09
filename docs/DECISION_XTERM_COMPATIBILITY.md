# Decision: xterm compatibility

**Status:** answered, M1a.
**Scope:** `crates/latch-protocol`, `crates/latch-term`, and one M2 item this
raises but does not resolve.

This is not a re-litigation of [`DECISION_EMULATOR.md`](DECISION_EMULATOR.md).
That decision picked `vt100` as the library behind the screen *model*. This
answers a different question the mission asked directly: is Latch, as a
system, compatible with a program that expects to be talking to xterm — and
should it be? The two questions get confused easily because "we chose vt100"
sounds like an answer to "are we xterm compatible", and it is not.

## The question has two different answers, because there are two paths

**The live path — already xterm-compatible, by construction, and it should
stay that way.** `terminal.output` and `terminal.input` carry raw bytes with
no structured decode (`docs/../planning/IMPLEMENTATION_PLAN.md`, "Wire
protocol"; enforced by `crates/latch-protocol`). A byte a program writes
because it assumes an xterm — SGR truecolor, any private mode, OSC 8
hyperlinks, the kitty keyboard protocol, synchronized-output brackets, any
mouse protocol — reaches an attached client unmodified. Compatibility on this
path is the client renderer's job, not this crate's: xterm.js on web,
"a maintained native terminal-emulation core" on Swift
(`planning/PROJECT_ARCHITECTURE.md`, component 3), both real xterm-family
emulators. `latch-term` never sits between a live byte and a live client. It
only ever gets a *copy* of the stream, through `Screen::feed`, to build a
model on the side.

**The reattach path — partially compatible, deliberately.** Per
`planning/IMPLEMENTATION_PLAN.md` ("The snapshot is not a message"), a
reattaching client is caught up with a *synthesized* snapshot — bytes
`vt100::Screen::contents_formatted`/`state_formatted` produces from its own
model, plus what `crates/latch-term` tracks alongside it (`modes.rs`) —
before live output resumes. Only what the model captured survives a detach.
This is where "xterm compatible" actually has teeth, because it is the one
place Latch itself is required to speak the dialect, not just carry it.

## What the reattach path restores today

Confirmed by reading `vt100` 0.16.2's source and by
`crates/latch-term/src/{terminal,modes}.rs`, not by documentation:

- SGR attributes (bold, dim, italic, underline, reverse, 256/truecolor),
  cursor position/visibility/shape, alternate screen, scroll region, origin
  mode, autowrap, insert mode, focus reporting, bracketed paste, cursor keys,
  application keypad, and all five mouse tracking/encoding modes vt100
  implements (X10 through SGR encoding) — these round-trip. This is most of
  what a real Claude Code or Codex session in `fixtures/vt/` actually turns
  on.
- Window title and `CSI s`/`CSI u` are restored by the adapter specifically
  *because* vt100 doesn't have them — that's the precedent this document
  follows.

## What it does not restore, checked against what the fixtures actually contain

Two of the eleven recorded fixtures — `codex-startup` and `high-rate-output`
— contain synchronized-output brackets (`CSI ?2026h`/`l`), kitty keyboard
protocol flags, and OSC 8 hyperlinks. None of the three is tracked anywhere
in `latch-term`, and `vt100` has no support for any of them (verified by
reading its source: no `2026`, `kitty`, or `hyperlink` handling exists in the
crate at all). They are not gaps in `vt100`'s SGR/mode coverage the way blink
or conceal are — `docs/DECISION_EMULATOR.md` already accepted those as
unused. These are gaps this document is the first to name, because they
weren't in scope for "does the screen render the same" — they're in scope
for "does the program still work the same after reattach."

Ranked by what actually breaks:

1. **Kitty keyboard protocol flags — real risk, cheap to close.** If a
   program pushes a disambiguation flag stack (`CSI > 1 u`) before a client
   detaches, and the flags aren't restored, the program still believes it's
   in kitty mode after reattach while the reattached client encodes keys the
   legacy way. That's a program misreading its own input, not a rendering
   glitch — the same class of bug `CSI s`/`CSI u` was rewritten to prevent.
   It closes the same way: track the flag stack in `modes.rs` from the same
   byte stream, replay it in the snapshot. Deliberately not done in this
   objective — it's a `latch-term` code change with test and fixture-model
   impact, and this objective's job was to answer the compatibility
   question, not reopen a delivered one. Recommended as a follow-up
   objective against `crates/latch-term`.
2. **Query answering has no owner anywhere in the system — a design gap,
   not a code gap.** `vt100` is a passive parser: it has no API to produce a
   reply to a query, so nothing in `latch-term` can answer Device Attributes
   (`CSI c`), cursor position reports (`CSI 6n`), or `DECRQM` mode queries
   on a program's behalf. Today the only thing that *can* answer is a live
   client's own real emulator (xterm.js does answer a subset of these). That
   means a session with **no client currently attached** — the gap between a
   worker starting and the first attach, or during any detach window — has
   nothing to answer such a query, ever. A TUI that blocks its startup on a
   DA response would hang. This isn't `latch-term`'s to fix; it's a worker
   question for M2 (which process owns the PTY's read side when nobody is
   attached, and does it need a minimal canned-response layer). It should be
   decided explicitly there rather than discovered as a hang. Flagging it
   here because this objective is the first place in the plan the question
   of query-answering ownership comes up at all.
3. **Synchronized-output mode and OSC 8 hyperlink spans — cosmetic,
   accepted.** Losing the "don't paint until this closes" hint after a
   reattach costs at most one visible flicker on the first frame, not
   incorrect state. A stale hyperlink span is a cosmetic loss on a link that
   already scrolled into the restored viewport — clicking it does nothing
   until the program repaints it. Neither is exercised as a functional
   requirement by any fixture. Documented, not closed, same standard
   `DECISION_EMULATOR.md` already applied to blink/conceal/strikethrough.

## Should Latch be fully xterm compatible?

Not by making `latch-term` an xterm implementation. That was already weighed
and rejected in `DECISION_EMULATOR.md`: `wezterm-term` is the closer-to-xterm
candidate of the two evaluated, and it lost on weight (4.88 MB, 240 crates)
and on two correctness regressions vt100 didn't have. Chasing full xterm
compatibility inside this crate reopens a decision that was made on measured
grounds, not on a documentation reading, and nothing in this objective's
research changed those measurements.

The right shape is the one already in place, made explicit:

- **Full compatibility on the live path, unconditionally** — this is a
  requirement, not a feature, and it already holds by construction
  (`crates/latch-protocol`'s no-structured-decode rule on the raw frame
  types). Nothing should ever be inserted into that path that inspects or
  rewrites bytes beyond what `modes.rs` already does for the two known
  backend gaps.
- **Deliberately partial compatibility on the reattach path**, scoped to
  what real Claude Code and Codex sessions are observed to use — which is
  most, not all, of xterm. The kitty-flag gap is worth closing because it's
  functional; the rest is worth documenting because it isn't.
- **Query-answering ownership is undecided and should be decided in M2**,
  explicitly, rather than left to whichever client happens to be attached
  when a program asks.

## What would justify revisiting this

- A recorded Claude Code or Codex stream is found to rely on a synchronized-
  output or OSC 8 state surviving a reattach — same bar `DECISION_EMULATOR.md`
  sets for its own accepted gaps.
- A TUI is observed hanging at startup with no client attached, confirming
  the query-answering gap is live rather than theoretical.
- `vt100` gains kitty-protocol or hyperlink tracking upstream, which would
  make closing gap 1 a matter of exposing it rather than tracking it
  independently.
