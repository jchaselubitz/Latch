# Building a Latch integration

Latch is a persistent-session provider, not an agent lifecycle manager. An
integration chooses a command, working directory, environment, and optional
viewer; Latch creates and owns the PTY session. The integrating product keeps
ownership of its job, workspace, credentials, and domain protocol.

Overlord is the reference shape: it creates a Latch session for an agent, may
open an iTerm viewer, and continues to use `ovld protocol` for mission and
objective lifecycle. It does not proxy terminal bytes, parse Latch's private
state, or import Latch code.

## Separate the worker from the viewer

Session creation and presentation are independent operations:

```text
Integration: resolve command, cwd, environment, and external run id
Latch:       create a persistent PTY session
Viewer:      optionally attach one human terminal surface
```

Creation can succeed when opening a viewer fails or is skipped. A viewer can
also be closed and reopened without changing the worker. The terminal surface
is exclusive: a later `latch attach` or remote terminal connection takes it
from the previous viewer.

Before relying on a feature, inspect the installed binary:

```bash
latch capabilities --json
```

Require the reported protocol version and flags your integration needs rather
than inferring capability from an executable name or local files.

## Create a session from a manifest

Use `latch create --manifest-file - --json` for programmatic creation. The
manifest goes over standard input so secrets do not appear in process arguments
or persisted session metadata. Its current format is version 1:

```json
{
  "format_version": 1,
  "launch": {
    "argv": ["codex", "exec", "Implement the task"],
    "cwd": "/absolute/path/to/worktree",
    "env": {"TASK_ID": "task_123"},
    "inherit_env": true,
    "size": {"cols": 120, "rows": 40},
    "term": "xterm-256color"
  },
  "display": {
    "name": "task-123",
    "title": "Implement the task",
    "command_label": "codex exec",
    "source": {"kind": "example-provider", "external_run_id": "task_123"}
  }
}
```

`launch.argv` must contain a program, `launch.cwd` must be absolute, and both
terminal dimensions must be non-zero. `env` is applied only to the child;
display metadata is sanitized and retained. Treat `command_label` and the
display fields as safe-to-show text, never as a place for a secret.

The command returns a stable JSON report and does not attach the caller:

```json
{
  "protocolVersion": 2,
  "session": {
    "id": "ses_…",
    "name": "task-123",
    "state": "running",
    "createdAt": "2026-08-30T12:00:00Z"
  }
}
```

Pass the manifest through stdin from the integration's own process. Do not put
secrets in a temporary manifest unless that file is protected and removed by
the integration.

## Discover, manage, and show the session

Use the JSON interfaces for lifecycle observations:

```bash
latch list --json
latch inspect SESSION --json
latch stop SESSION --json
latch remove SESSION --json
```

To offer an iTerm window on macOS, call:

```bash
latch open SESSION --with iterm --as window --json
```

An integration that owns the viewer preference should pass `--as window` or
`--as tab` explicitly instead of depending on the user's `open.behavior`
setting. `latch open` currently supports iTerm; Latch Desktop manages its own
terminal choices through its native UI.

Never read, write, or infer state from `~/.latch`. Do not use a private kernel
or tmux server as an integration API. Use the CLI's JSON output and process
exit status instead.

## Remote and embedded clients

For a client that connects to a local or tunneled gateway, first query
`GET /v2/capabilities` and require protocol major 2. The terminal endpoint is
`WS /v2/sessions/{id}/terminal`, authenticated through the `latch.v2.*`
WebSocket subprotocol. It requires the `control` grant and takes the session's
only terminal surface. A client must supply a terminal size before the steal
commits and must not blindly reconnect after a `stolen` close: reconnecting
would take the session back from the person who just claimed it.

The private workspace packages `@latch/client` and `@latch/terminal-react`
provide the gateway and React terminal seams for repository development. Their
scope, limits, and v2 protocol details are in [REMOTE_SDK.md](REMOTE_SDK.md).
They are not published SDKs.

The conversation socket is a different integration: use the canonical schemas
under [`schemas/remote-access/v2/`](../schemas/remote-access/v2/) and let the
Conversation Hub own ordering, pending requests, and action durability. Do not
parse Codex or Claude transcripts, synthesize a second event stream, or send
terminal input to imitate conversation actions.

## Overlord-specific notes

For Overlord, set `display.source.kind` to `overlord` and use the Overlord run
or objective id as `display.source.external_run_id`. Latch then supplies the
persistent terminal; Overlord remains responsible for its `ovld protocol
attach`, `update`, and `deliver` lifecycle.

Use `latch attach` as the terminal fallback. Do not route terminal bytes
through the Overlord backend. The historical design and the exact ownership
boundary are retained in
[planning/OVERLORD_INTEGRATION.md](../planning/OVERLORD_INTEGRATION.md).
