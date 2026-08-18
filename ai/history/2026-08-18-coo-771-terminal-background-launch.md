# coo:771.3xd2 — Open connected terminals in the background

Latch Desktop's Terminal settings now include **Open in background**. When
enabled, attaching a session no longer brings the preferred terminal to the
front.

## Behavior

- Default remains foreground (AppleScript `activate`, Ghostty `activates = true`).
- Background launches omit `activate` for Terminal/iTerm window opens and iTerm
  tabs, set Ghostty `activates` to false, then restore the previously frontmost
  app so Terminal.app cannot keep focus after `do script`.
- Terminal.app tabs still send Command-T (which needs Terminal frontmost), then
  restore focus; that path may flash briefly. If System Events is refused, Latch
  still falls back to a new window.
- Custom terminals follow their argument template; Latch restores focus after
  `Process.run()`, but a custom app that activates itself later can still steal
  it.
- The CLI installer (`Run in Terminal…`) is unchanged and still takes focus.
