# Getting started

Latch makes terminal sessions persistent. Closing a terminal window or losing
its connection detaches the window; it does not stop the program in the
session. A session has one live terminal surface at a time, so attaching from
another terminal moves that surface there.

## Install Latch

Latch currently supports macOS on Apple Silicon and Intel. Install the CLI
payload with:

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The installer downloads the matching release, verifies its checksum and
Developer ID signatures, then installs `latch`, `latchd`, `latch-remote`, and
`latch-tmux` in `~/.local/bin`. Add that directory to your `PATH` if needed:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Check the complete installation before creating a session:

```bash
latch --version
latch doctor
```

The default kernel is `latchd`, Latch's per-session daemon. `LATCH_KERNEL=tmux`
selects the private, pinned tmux fallback only for *new* sessions; it does not
move or change existing sessions.

## Create and resume a session

```bash
latch                         # start a persistent shell and attach to it
latch run -- claude           # run a persistent command
latch run --name review -- npm test
latch list                    # find sessions, most recently active first
latch attach review           # take the session's terminal surface
```

Close the terminal window to detach. The process keeps running. Use `exit` in
the session, or `latch stop SESSION`, to stop its process. Recently exited
sessions retain their final screen for up to 24 hours; `latch prune` reclaims
expired records and `latch prune --all` reclaims exited records immediately.

Sessions cannot nest. Running bare `latch` inside a Latch session attaches to
the enclosing session; `latch run` and `latch create` refuse to create a nested
session.

## Make every iTerm window persistent

The smoothest adoption path is an iTerm profile whose command is `latch`.
Every new window or tab opened with that profile starts a persistent shell,
while the terminal itself remains iTerm. See the complete
[iTerm setup guide](ITERM_SETUP.md).

For a session you created elsewhere, open an iTerm attachment with:

```bash
latch open SESSION --with iterm
latch open SESSION --with iterm --as tab
```

`latch open` is currently an iTerm integration on macOS. It opens a viewer but
does not create, stop, or otherwise change the session process.

## Where to go next

- Use the [CLI reference](CLI.md) for session management, automation, and
  troubleshooting.
- Install [Latch Desktop](DESKTOP.md) for a native macOS session manager and
  paired iPhone remote access.
- Read [Integrations](INTEGRATIONS.md) if another product needs to create or
  display Latch sessions.
