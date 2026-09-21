# Decision: conversation observation preserves session geometry

**Status:** decided; existing kernel behavior retained and regression-tested.
**Date:** 2026-09-21
**Mission:** coo:1035.ygzy
**Scope:** the rendered screen used by JSONL conversation connectors. Removing
mobile Chat's terminal claim belongs to the next objective, coo:1035.kyw6.

## Policy

Conversation observation and connector actions do not attach, pin, resize, or
restore the PTY. The session owns its geometry:

- Before the first attachment, use the supplied launch size. The unattended
  default is **80 columns × 24 rows**, already used by `latchd run`, remote
  creation (`REMOTE_INITIAL_SIZE`), and the CLI's no-terminal fallback. A
  manifest's explicit size takes precedence; “never attached” does not imply
  that every session is 80×24.
- A human terminal attachment resizes an unpinned session to its own grid.
  Detachment retains that grid, even with zero remaining surfaces. Do not
  reset it to 80×24: detachment must not cause an extra reflow or SIGWINCH.
- Preserve explicit user pinning. Do not automatically pin connector sessions.
- Keep last-moment send/resolve validation. If a choice has wrapped or the
  request cannot be identified, refuse with the existing reason and emit no
  action input. Observation must not silently acquire terminal ownership to
  make an action pass. The user can explicitly open the terminal to answer a
  prompt the connector cannot identify.

This adopts the existing defined launch default plus last-size retention as
an explicit contract. It does **not** promise width-insensitive approvals.
Removing Chat's attach removes a geometry side effect; successful resolution
still depends on what the agent actually paints at the retained width.

## Measurements

Measured on macOS against the repository-built `latchd`, using real PTYs,
Unix sockets, `ConversationControl`, and `JsonlConnector::apply`. The child
reports its winsize through `stty size`; kernel `stat` independently checks
columns, rows, attachment, and pin state. No external agent credentials or
live user sessions are involved.

| Lifecycle | Child PTY and kernel measurement |
| --- | --- |
| Default launch, never attached | 80×24, unattached, unpinned |
| Explicit 101×37 launch, never attached | 101×37, unattached |
| Desktop attached | 160×48, attached, unpinned |
| Phone-sized terminal attached | 32×24, attached, unpinned |
| Desktop 160×48 attached, then disconnected | 160×48, unattached; no restoration |

Each action case paints a deterministic screen through the real child PTY
**after** the target attachment, then invokes the production connector action.
Accepted input is verified at the child. Refusal is verified both by its exact
reason and by a subsequent sentinel being the child's first received input.
Actions leave geometry and attachment state unchanged.

| Action / screen | Desktop 160×48 | Phone 32×24 | Never attached 80×24 |
| --- | --- | --- | --- |
| Send, empty Claude `❯` composer | Accepted | Accepted | Accepted |
| Send, empty Codex `›` composer | Accepted | Accepted | Accepted |
| Send, Claude/Codex composer containing `draft` | Refused: composer | Refused: composer | Refused: composer |
| Resolve visible `1. Yes` | Accepted, key `1` | Accepted, key `1` | Accepted, key `1` |
| Resolve `1. Allow this command to read the project configuration files` | Accepted, key `1` | Refused: choice | Accepted, key `1` |
| Resolve after prompt replaced by unrelated screen | Refused: prompt | Refused: prompt | Refused: prompt |

Exact refusal reasons:

- Composer: `the claude composer is no longer empty` or
  `the codex composer is no longer empty`.
- Choice: `the requested choice is not identifiable on the current screen`.
- Prompt: `the requested Claude prompt is no longer visible`.

The long choice wraps through the actual terminal parser at 32 columns. Its
short request title remains visible, isolating the exact single-line choice
match as the reason for refusal. Longer titles/choices can also fail at 80 or
160 columns; neither is a universal safe width.

These are controlled screen-contract measurements, not a claim that a specific
Claude or Codex release always paints these layouts. Application word wrapping,
clipping, scrolling, and modal layout can introduce additional refusals. Source
binding and pending-request state are seeded test fixtures; the snapshot,
validation, input delivery, and geometry are real. Existing substring/history
and composer-marker heuristics are unchanged, not certified by this decision.

## Rationale and rejected alternatives

**Automatically pin every connector session:** rejected. A fixed width would
make a narrow terminal crop or misrepresent the agent's native display and
ignore the established human attachment resize contract. No finite pin width
guarantees that arbitrary choice labels fit. Pinning also changes every desktop
session to accommodate a read-only observer.

**Reset to a default whenever the last surface detaches:** rejected. This adds
another resize and agent redraw to every detach, loses the last human's layout,
and still does not guarantee a matching label. The default belongs to launch,
not to observation or detach.

**Join wrapped rendered lines before matching:** rejected for this objective.
The connector's plain text snapshot does not carry sufficient semantic
provenance to distinguish a soft wrap from a separate option, description, or
neighboring content. Broad normalization could turn a safe refusal into an
incorrect approval. Reliable structured request resolution is the better
long-term replacement; it is outside this geometry investigation.

**Keep the phone attach to establish a width:** rejected. The measurement shows
that a narrow attach can itself break matching. It also steals the desktop's
terminal and makes reading Chat require terminal authentication.

## Regression gate

`crates/latch/src/conversation/connectors/jsonl/geometry_tests.rs` contains two
real-kernel tests: the 21-case action matrix and launch/attach/detach geometry
measurements. Missing daemon binaries fail explicitly rather than skipping.

```sh
cargo build -p latchd
cargo test -p latch --lib geometry_tests -- --nocapture
```

Set `LATCH_E2E_LATCHD_BIN` to use a separately built daemon. The environment must
permit PTY creation and local Unix socket binding. The sandbox blocked socket
binding on the first attempt; the same tests passed outside that restriction.

The next objective can remove Chat's attach/drain/authentication path without
introducing a replacement surface or a geometry manager. It must preserve
these refusal checks and the existing terminal exclusivity contract.

## Follow-through (coo:1035.kyw6)

Mobile Chat's attach, drain, and owner-authentication prompt are removed. Chat
opens only the conversation socket; no observer surface replaced the attach,
because the connector reads the screen over `ConversationControl`.
`geometry_tests.rs` gained
`chat_observes_and_sends_beside_an_attached_desktop_terminal`: repeated screen
reads and one send beside a 160×48 desktop attachment leave it attached and
unresized, deliver no `SIGWINCH` to the child, and submit exactly one prompt.
`a_terminal_attach_is_what_the_winch_trap_reports` is its control.
