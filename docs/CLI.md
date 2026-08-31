# Latch CLI

`latch` is the durable session provider. It stores session metadata under
`~/.latch`, while each session's process is owned by its selected kernel. No
account or network connection is required for ordinary local sessions.

Run `latch --help` for the command surface installed on a machine and
`latch COMMAND --help` for flags. The following describes the current public
surface.

## Create, attach, and inspect

```bash
latch                              # persistent interactive shell, attached
latch shell --name notes --title "Release notes"
latch run --name migration -- ./migrate-db
latch list
latch inspect migration
latch attach migration
latch attach migration --retry
```

`latch attach` with no session selects the most recently active session.
Attaching is exclusive: it takes the single terminal surface from any prior
viewer. `--retry` retries transient transport loss only; it does not retry a
deliberate steal.

`latch open SESSION --with iterm [--as window|tab]` asks iTerm on macOS to
open an attachment. The default shape is a new window; set
`open.behavior` to `new-window` or `new-tab` to change it:

```bash
latch config open.behavior new-tab
latch config open.behavior
latch config                         # print all non-secret preferences
```

## Manage sessions

```bash
latch rename SESSION useful-name
latch resize SESSION --cols 120 --rows 40
latch resize SESSION --cols 120 --rows 40 --pin
latch stop SESSION
latch stop --all --yes
latch remove SESSION                 # removes an exited/lost record
latch remove SESSION --force         # stops a live session, then removes it
latch prune --dry-run
latch prune
latch prune --all                    # reclaim exited sessions now
```

`stop` ends the session process but preserves its retained screen and
metadata. `remove` deletes that retained record. `resize --pin` keeps the
explicit size against controller-driven changes.

## Machine-readable output and capability discovery

Commands that report session state accept `--json`, including `create`,
`list`, `inspect`, `stop`, `remove`, `rename`, `resize`, `prune`, `doctor`,
`config`, `update`, and `capabilities`. Use it instead of parsing terminal
output.

```bash
latch capabilities --json
latch list --json
latch inspect SESSION --json
latch doctor --json
```

`latch capabilities --json` reports the product version, protocol version,
and flags such as `create`, `openViewer`, `localAttach`, and `selfUpdate`.
An integration should check it before relying on a feature. The session
creation document and integration flow are in [Integrations](INTEGRATIONS.md).

## Repair and update

```bash
latch doctor
latch update --check
latch update
latch update --force
```

An update replaces and verifies the entire three-binary payload (`latch`,
`latch-remote`, and `latchd`). If the binary
belongs to a package manager, Latch refuses to replace it; update it through
that package manager instead.

## Local gateway and remote access

`latch serve` provides a local HTTP/WebSocket gateway for clients. It listens
on `127.0.0.1:4610` by default and uses a bearer token. Keep it on loopback and
reach it through an SSH tunnel when needed; a non-loopback bind is plaintext
and requires the explicit `--allow-remote` opt-in.

```bash
latch serve token                    # mint or rotate the bearer token
latch serve                          # loopback gateway
latch serve --bind 127.0.0.1:0 --ready-file /tmp/latch-ready.json
```

The paired remote-access service is separate from that gateway. It encrypts
and authenticates its transport, supervises an ephemeral loopback gateway, and
never exposes `latch serve` publicly:

```bash
latch remote-access enable
latch remote-access status --json
latch remote-access pair create --json
latch remote-access devices --json
latch remote-access grant DEVICE_ID control
latch remote-access revoke DEVICE_ID
latch remote-access relay disable
latch remote-access relay never       # also publish host candidates only
latch remote-access diagnostics
latch remote-access audit --json
latch remote-access disable
```

Permissions form a ladder: `observe`, `interact`, then `control`. Opening a
terminal requires `control` and takes the session's exclusive terminal
surface. `lan-serve`, `offer`, and `direct-probe` are helper and diagnostics
surfaces; Latch Desktop normally supervises the helper for you. See
[Desktop](DESKTOP.md) for the supported user flow and
[REMOTE_ACCESS_DESKTOP.md](REMOTE_ACCESS_DESKTOP.md) for its security boundary.
