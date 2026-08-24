# coo:840.qcrv — latch-tmux kernel patch 2 (deferred parse)

Patch level 2 on pinned tmux 3.7b. Exclusive raw-attach from patch 1 is unchanged.

## What changed

`input_parse_pane` no longer runs to completion on the pane read callback.
Raw forward stays first. Grid parse is scheduled on a 1 ms `parse_timer` in
slices of 4 KiB. If unparsed input exceeds 256 KiB, the callback consumes a
bounded 64 KiB catch-up slice and returns to the event loop instead of
parsing the entire backlog (that unbounded catch-up starved steal/MSG_EXIT
and `display-message`). Overflow never stalls the child to protect the grid.

Before an exclusive-attach snapshot, the pane is parsed fully so steal still
sees current alt-screen, cursor, and modes. `server_client_check_pane_buffer`
includes the live raw client's offset so a lagged parser cannot hold
`EV_READ` off incorrectly.

Reasoned exits stay 75 / 76 / 77. `bufferevent_disable(EV_READ)` is still
the one-loop debounce; it was not deleted as “the fix.” No reader thread.

Packaging: `patches/tmux/0002-latch-deferred-parse.patch`,
`patches/tmux/manifest.json` `patchLevel` 2. Rebuild with
`scripts/build-tmux.sh dist/latch-tmux`.

## Tests (against `dist/latch-tmux`)

`LATCH_E2E_TMUX_BIN=…/dist/latch-tmux cargo test -p latch --test exclusive_attach_e2e -- --test-threads=1`

- CSI writer ~70k frames/s while a raw client is attached (child not stalled)
- Steal during a redraw burst restores alt-screen / hidden cursor / bracketed paste
- Post-boundary amplification 1.0000x; quiet pane produces no extra live paint
- Slow-client eviction, then next attach gets a current frame; pane keeps ticking
- Full exclusive-attach e2e: 18 passed
- Phase 0 kernel suite: 5 passed
