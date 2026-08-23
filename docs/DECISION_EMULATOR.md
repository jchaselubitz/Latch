# Decision: the terminal emulator behind the screen model

**Status:** decided, M1.
**Choice:** [`vt100`](https://crates.io/crates/vt100) 0.16, over
[`wezterm-term`](https://github.com/wezterm/wezterm/tree/main/term).
**Scope:** `crates/latch-term`. The choice is named in `src/terminal.rs` and
nowhere else; the worker holds a `Screen`.

> **Superseded.** `crates/latch-term` was archived under the
> `archive/latch-term-v1` tag. Latch no longer runs a terminal emulator on the
> live path at all: the patched `latch-tmux` kernel owns the screen model, and
> a surface is painted one current frame and then the pane's own bytes. This
> record is kept for why the emulator was chosen when there was one. See
> [`DECISION_EXCLUSIVE_ATTACH.md`](DECISION_EXCLUSIVE_ATTACH.md).

This closes open item 2 in [`../planning/IMPLEMENTATION_PLAN.md`](../planning/IMPLEMENTATION_PLAN.md).

## How it was decided

Both candidates were built against every case in [`../fixtures/vt/`](../fixtures/vt)
— eleven recorded PTY streams from real Claude Code and Codex sessions — and
measured against the hand-authored `expected.json` for each, rather than
compared on documentation. Disagreements were then reproduced in isolation so a
fixture-reader bug could not be mistaken for a library one.

The criteria, in the order the product cares about them: snapshot fidelity for
alternate-screen applications, resize and reflow correctness, wide-character and
combining-mark handling, scrollback access, maintenance status, and dependency
weight.

## What the measurements said

### Snapshot fidelity — vt100

This is the criterion the product rests on: criterion 4 of the success criteria
is that a reattaching client sees exactly what a continuously attached one sees,
and every mobile app-backgrounding is a detach and a reattach.

`vt100` ships `contents_formatted` and `state_formatted`, purpose-built
serialization of a screen back into escape sequences. Fed each fixture,
snapshotted, and replayed into a fresh parser, it reproduced the screen
identically on **11 of 11 cases**, in 71 to 1431 bytes per screen.

`wezterm-term` has no equivalent. Its serialization exists for its own GUI, not
for a wire. Choosing it would have meant writing and owning the single most
load-bearing piece of this crate from scratch.

### Resize and wide characters — vt100

| Case | vt100 | wezterm-term |
| --- | --- | --- |
| `resize-midstream` | matches | 3 mismatches: loses a line, cursor one row high |
| `unicode-wide-combining-emoji` | matches all 12 asserted cells | 4 mismatches |

The wide-character failure is the serious one, and it reproduces in four lines:
print five `漢` to a nine-column screen. `vt100` fills columns 0–7, leaves
column 8 blank, and wraps the fifth character whole to the next row.
`wezterm-term` puts the fifth character at `cell_index` 8 with width 2, hanging
it over the right margin. A wide character that will not fit is supposed to wrap
whole; splitting it drifts everything to its right by a column, which is
invisible in a diff and obvious on a phone.

### Scrollback access — wezterm-term

The one criterion `wezterm-term` wins. It exposes structured lines including
scrollback; `vt100`'s buffer can only be read through a scroll offset, and it
cannot be drained at all, which is why `Terminal` mirrors lines out of it as
they arrive. The ring semantics Latch needs — alternate-screen output excluded,
oldest dropped first, dropped lines counted — are custom either way.

### Maintenance and dependency weight — vt100, decisively

`wezterm-term` **is not published to crates.io**. It is consumable only as a git
dependency pinned to a commit in the wezterm monorepo, with no semver and no
release cadence of its own.

Measured, on this machine, with the release profile this repo ships:

| | binary | transitive crates | clean build |
| --- | --- | --- | --- |
| empty Rust binary | 329 KB | 1 | 2.1 s |
| with `vt100` | 373 KB | 7 | 4.8 s |
| with `wezterm-term` | 4.88 MB | 240 | 82 s |

`wezterm-term` hard-enables `use_image` and depends unconditionally on `image`
and `termwiz`, so the tree includes an AV1 encoder (`rav1e`), JPEG/PNG/WebP/EXR
codecs, ICU, `rayon`, `url` and `uuid`, and there is no feature flag that
removes them. Decision D2 puts a terminal profile in front of the `latch`
binary, so **every terminal window pays its startup cost**. A 4.5 MB, 240-crate
increment for a screen model is not a cost this binary can absorb.

## What vt100 does not do, and what the adapter does about it

`vt100` is a smaller library than `wezterm-term` and the gaps are real. Each one
is closed in `crates/latch-term`, and each closure is asserted by the suite.

| Gap | How it is closed |
| --- | --- |
| No window-title tracking | It parses OSC 0/2 and drops the result. `modes.rs` tracks the title off the same stream. |
| `CSI s` / `CSI u` (SCOSC/SCORC) unimplemented | Rewritten to `ESC 7` / `ESC 8`, which it does implement. Confirmed necessary: without it the `cursor-rewrite-progress` fixture's thirty spinner frames land end to end across three lines instead of once in place. |
| No public getters for scroll region, origin mode, autowrap, insert mode, focus reporting, cursor shape | Tracked in `modes.rs` from the same bytes. |
| No way to drain its scrollback buffer | `Terminal` mirrors lines out as they arrive and empties the buffer by rebuilding the parser from a snapshot — the same operation, and the same guarantee, as a client reattaching. |
| A `process()` boundary landing inside a multi-byte character can shift a column | The filter emits only whole sequences and whole characters, so the backend never sees such a boundary regardless of where the PTY read fell. Found by running a recorded Claude Code stream through every chunk size; now covered for chunk sizes 1 through 120 across every fixture. |
| Narrowing a resize can leave half a wide character in the last column | Detected after resize and repaired by rebuilding through a snapshot, which wraps the character the way a real terminal would and leaves the grid representable again. |

### Accepted limitations

**No blink, conceal or strikethrough cell attributes.** `vt100` models
foreground, background, bold, dim, italic, underline and reverse. A survey of
every recorded stream in `fixtures/vt/` found only bold, dim, underline and
reverse ever used — no `SGR 5`, `8` or `9` anywhere in Claude Code or Codex
output. A reattaching client would render blinking or struck-through text plain.
`wezterm-term` models all of them.

**Autowrap and insert mode are tracked but not enforced.** `vt100` always
autowraps and does not implement IRM, so a program that turns either off is
reported correctly in the model but rendered as though it had not. Neither
appears in any recorded stream.

**Origin mode combined with a cursor parked past the last column.** The snapshot
addresses the cursor relative to the scroll region in that case, which cannot
express a pending wrap. Both parts are rare; together they have not been
observed.

## What would justify revisiting this

Any one of these is grounds to re-run the evaluation, not to switch on sight:

- An agent TUI is found to use blink, conceal, strikethrough, or to disable
  autowrap or use insert mode, in a way a user notices after reattaching.
- The list of gaps closed in `modes.rs` grows far enough that the adapter is
  doing more emulation than adaptation. The current line is: it tracks state and
  rewrites two sequences; it does not maintain a grid.
- `vt100` stops being maintained, or a `vt100` defect is found that cannot be
  worked around from outside it. The two found here — the `process`-boundary
  column shift and the split wide character on narrowing — both could be, but a
  third of a different kind would change the picture.
- `wezterm-term` is published to crates.io **and** gains a way to build without
  `image`/`termwiz`, **and** fixes wide-character placement at the right margin.
  All three, not any one.
- The startup budget stops mattering — that is, decision D2 is reversed and
  `latch` is no longer on a terminal profile. This is the only change that would
  make the 240-crate tree acceptable on its own.

The re-run is cheap and should be repeated rather than reasoned about: build
both against `fixtures/vt/`, compare against the recorded expectations, and
measure the binary.
