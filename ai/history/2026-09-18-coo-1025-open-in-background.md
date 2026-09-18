# coo:1025.bkhy — `latch open --background`

Overlord-launched sessions always stole focus: Overlord's runner calls
`latch create` then `latch open --with iterm`, and the `open` AppleScript sent
`activate` unconditionally. Neither Overlord's per-target "Open in the
background" nor Latch Desktop's "Open in background" toggle reached this path
(the Desktop toggle only governs Desktop's own `TerminalLauncher`).

## Behavior

- `latch open` takes `--background` / `--foreground`; with neither,
  `open.background` in `~/.latch/config.toml` decides (default foreground).
- A background open omits `activate`, records the frontmost application via
  `path to frontmost application` (no System Events permission), and re-activates
  it if iTerm took focus while creating the window/tab. The restore is wrapped
  in `try`, so a refused `activate` never fails an open that already succeeded.
- `OpenReport` gains `background` (serde-defaulted).
- Overlord gates the flag on product version `0.2609181007.0`.
- If iTerm is not running, macOS may still flash it forward while it launches.
