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

### Launch a hosted agent

Latch identifies a Claude Code or Codex session from the launch itself. It
records the session's harness marker, which selects its conversation
connector for the session list, the Conversation Hub, and mobile Chat. It
also prepares the agent's conversation observer, such as Claude's hook plugin.
It never looks inside shell text, so `["/bin/zsh", "-lc", "claude"]` is a
plain shell session with no connector.

To start an agent the way a terminal would, with the owner's login-shell
PATH, declare it and let Latch build the shell wrapper:

```json
{
  "format_version": 1,
  "launch": {
    "argv": ["claude", "--model", "opus"],
    "agent": "claude",
    "login_shell": {"path": "/bin/zsh", "prelude": "export TASK_ID=task_123"},
    "cwd": "/absolute/path/to/worktree",
    "size": {"cols": 120, "rows": 40}
  }
}
```

- `agent` is `claude`, `codex`, or `cursor`. `argv[0]` must be the corresponding
  executable, as a bare name or a path; Cursor uses `agent` (also accepts
  `cursor-agent`). Anything else is rejected with `launch.agent`.
- `login_shell.path` must be absolute. After recording the identity and adding
  the observer arguments to `argv`, Latch runs
  `[path, "-ilc", "<prelude>\nexec \"$@\"", "latch", argv...]`. The program and
  its arguments reach the shell as positional parameters, never as text.
  `prelude` is optional launcher-authored shell that runs first. Latch never
  interprets it.
- Require the `agent-launch` entry in `latch capabilities --json`
  `capabilities.extensions` before sending these fields. Older builds ignore
  them and would run the agent argv directly, with no identity.

### Create at most once: `--request-id`

A caller that retries after a crash, or after losing the report, can pass an
idempotency key so the retry never starts a second session:

```bash
latch create --manifest-file - --json --request-id 2f1c8a0e-6a1b-4f7e-9d2c-5b8e7a6c4d31 < manifest.json
```

- The id must be a hyphenated UUID; use a new one for each thing you mean to
  start, and the same one for every retry of it.
- Latch durably accepts the id before it launches anything, and the session's
  metadata records it (`source.request_id`) before the process starts. A repeat
  call with the same id and the same launch returns that session and launches
  nothing; the report is the usual one plus `"reused": true`. A call that
  started the session reports `"reused": false`. Without `--request-id` the
  report has no `reused` field and `create` behaves exactly as before.
- The id is bound to the launch: its directory, its program (by name, so an
  agent reinstalled at another path is still the same request) and everything
  else that changes what the program does (its arguments, which include the
  model and prompt, its `env`, `inherit_env`, `agent`, and `login_shell.prelude`).
  Terminal size and display fields do not bind it.
- When the id refuses the launch, nothing is started, the exit status is 65,
  and with `--json` standard output carries
  `{"protocolVersion": 2, "error": {"code": "...", "message": "..."}}`:
  - `request_id_conflict`: the id already names a different launch.
  - `request_id_session_removed`: the id already started a session
    (`error.sessionId`) that has since been removed; it is never started again.
  - `invalid_request_id`: the id is not a hyphenated UUID.
- Any other failure exits 1 (with `create_failed` under `--json`) and started
  nothing; the same id may be retried.
- `source.request_id` cannot be set from the manifest; only the flag sets it.
  Local ids and the phone's request ids are kept apart and never answer each
  other.
- Require the `create-request-id` entry in `latch capabilities --json`
  `capabilities.extensions` before passing the flag. An older build rejects it.

### Cursor chat

Launch Cursor with `latch run -- agent`, or use `"agent": "cursor"` and
`"argv": ["agent"]` in the launch manifest. Latch advertises the `cursor`
conversation connector to chat clients and injects a private native hook plugin
using `--plugin-dir`. This requires a Cursor CLI version with local plugin
support; live two-turn chat verified with `2026.09.23-86fc751`.

The connector reads only the transcript path reported by that session's hooks.
It projects user/assistant messages, observes tool activity and turn completion,
and submits chat messages only when the live Cursor composer is empty. Cursor
permission dialogs stay on the terminal surface. Existing sessions launched
without the observer must be relaunched for transcript-backed chat. User and
workspace Cursor settings are not modified.

References: [Cursor CLI parameters](https://cursor.com/docs/cli/reference/parameters),
[native hooks](https://cursor.com/docs/hooks), and
[plugin format](https://cursor.com/docs/reference/plugins).

A session's identity is fixed when it is created. Sessions started before a
launcher adopted this path keep no harness marker and must be recreated to
open in Chat.

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
setting. The same applies to focus: pass `--background` or `--foreground`
(0.2609181007.0 and later) rather than depending on `open.background`. The
JSON report echoes the result as `background`. `latch open` currently supports iTerm; Latch Desktop manages its own
terminal choices through its native UI.

Never read, write, or infer state from `~/.latch`. Do not use a private kernel
or tmux server as an integration API. Use the CLI's JSON output and process
exit status instead.

### Stopping from somewhere other than the Mac

`latch stop` is a CLI on the Mac that hosts the session, which is enough for a
desktop bridge running there. When the integration's process is somewhere
else — a hosted web app, a tunnel, or a phone — the same operation is served by
the gateway as `POST /v2/sessions/{id}/stop`, and `@latch/client` exposes it
as `stopSession`. It is the same graceful SIGTERM-then-SIGKILL stop, it answers
the same `StopReport` (`{ id, state, stopped }`), it requires the `control`
grant, and it is advertised as `endpoints.stopSession` in discovery. A
gateway that predates the route omits the key; the client refuses before
asking rather than mistaking a bare 404 for a session that is gone.

Stopping is not removing: the record and dead pane stay, and there is no
remote removal route. Erasing a session is a decision made at the Mac with
`latch remove`. Repeating a stop is safe, so a caller that never saw the
answer may simply ask again. The wire details, error codes, and the client's
contract are in [REMOTE_SDK.md](REMOTE_SDK.md).

## Remote and embedded clients

For a client that connects to a local or tunneled gateway, first query
`GET /v2/capabilities` and require protocol major 2. The terminal endpoint is
`WS /v2/sessions/{id}/terminal`, authenticated through the `latch.v2.*`
WebSocket subprotocol. It requires the `control` grant and takes the session's
only terminal surface. A client must supply a terminal size before the steal
commits and must not blindly reconnect after a `stolen` close: reconnecting
would take the session back from the person who just claimed it.

`POST /v2/sessions/{id}/stop` is the lifecycle verb the gateway serves beside
the terminal, so a remote client can end a session it can see without a shell
on the Mac; see the stop section above.

A client on the same Mac finds the gateway in one of two places. A standalone
`latch serve` binds `127.0.0.1:4610` and keeps its token in
`$LATCH_HOME/serve.token`. With Remote Access enabled, the Remote Link
supervisor runs the gateway instead, on an ephemeral loopback port: its address
is the `url` in
`$LATCH_HOME/remote-access/runtime/remote-link-gateway-ready.json` (schema
[`gateway-readiness.schema.json`](../schemas/remote-access/v2/gateway-readiness.schema.json))
and its token is in `remote-link-gateway.token` beside it. Only one gateway
holds the Conversation Hub at a time, so a client should try both and use the
one that serves the conversation endpoint. Overlord's runner depends on these
two file names to deliver answers; renaming them is a breaking change for it.

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
through the Overlord backend. The "End terminal session" action in a mission
panel is `latch stop SESSION --json` when Overlord's desktop bridge is on the
Mac and `stopSession` through the gateway when it is not; both answer the same
report, and Latch Mobile ends the same session the same way. The historical
design and the exact ownership boundary are retained in
[planning/OVERLORD_INTEGRATION.md](../planning/OVERLORD_INTEGRATION.md).
