# coo:714 — Overlord pre-commands and PATH vs iTerm

## Cause

Overlord launches Latch with `$SHELL -lc` (login, **not** interactive). That
skips `.zshrc`, so nvm never loads. On this machine:

- `agp` and `ovld` live in `~/.nvm/versions/node/v24.13.0/bin`
- iTerm is login **and** interactive, so both resolve
- a clean-env `zsh -lc` finds neither; `zsh -ilc` finds both

Mission history: the first two coo:714 launches used pre-command `agp` and the
pane exited 127 ("Pane is dead"). The third launch had no pre-command, so
Cursor started, but `ovld protocol attach` never ran because `ovld` was missing
from that same non-interactive PATH.

The Overlord inspect error (`tmux returned an unexpected session row: ses_…`)
is Latch rejecting a tmux status line. Tabs in the row render as underscores
in the UI; live panes also omit empty trailing dead-pane fields, so a strict
10-column split failed even on a healthy session.

## Latch changes

- Promote POSIX `shell -c` / `-lc` argv to `-ilc` at pane exec so Latch sessions
  load the same startup files as iTerm, including Overlord's current `-lc`
  wrapper.
- `latch shell` now uses `-il`.
- Parse tmux session rows with a unit-separator format, accept 8–10 columns,
  and keep a tab fallback for tests.

## Overlord sibling repo (not in this checkout's git)

`buildLatchCreateManifest` and the nested inline path now pass `-ilc` so a
future Overlord runner matches iTerm even with an older Latch.

## Verify

`cargo test` in Latch: 42 lib + 4 tmux_kernel tests passed.
