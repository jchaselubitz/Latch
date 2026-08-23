# Integrating Latch with Overlord

> **The previous Overlord conversation integration is retired.**
>
> `latch events`, `latch send`, session-level interaction capabilities, the v1
> gateway, event cursors, and the harness event ledger no longer exist. There is
> no adapter, alias, migration, or compatibility mode for them.

## Supported boundary

Latch remains a persistent-terminal launch provider. Overlord may use the
public CLI to create, list, inspect, attach to, and stop a session. Latch owns
the PTY and terminal attachment lifecycle; Overlord owns mission, objective,
worktree, and agent protocol lifecycle. Neither product imports the other's
implementation or reads the other's private state.

```text
Overlord: resolve command, cwd, environment, mission context
Latch:    create persistent PTY session and expose terminal attachment
Agent:    ovld protocol attach/update/deliver
```

Terminal fallback remains `latch attach`. Overlord must not proxy terminal bytes
through its backend.

## The provider and the viewer are two choices, not one

Latch is an **execution/session provider** (`ExecutionProviderKind` `latch`).
iTerm, Terminal, and Termius are **viewers** (`TerminalViewerKind`). The two
axes are stored, resolved, and reported separately, and must not be collapsed
into a single terminal list:

- `provider = latch, viewer = none` is a valid, complete choice: the agent runs
  headless and nobody is looking at it yet.
- Session creation can succeed while viewer launch fails. The first is fatal to
  a launch; the second is cosmetic.
- Changing the viewer must never recreate the provider session.

A session has exactly one human surface, and `latch attach` **steals** it. A
second viewer therefore moves the existing session's terminal rather than
starting a second one, so changing where an agent is shown never restarts the
agent. `packages/core/service/terminal-profile-types.test.ts` in the Overlord
resource holds the regression tests for these invariants.

The UI may present a composed preset such as "Persistent with Latch, open in
iTerm", but what is persisted stays two-dimensional.

## Future conversation client

An Overlord conversation integration has not been rebuilt. If one is added, it
must be a native v2 Hub client rather than an independent transcript observer:

- authenticate and open `WS /v2/sessions/{id}/conversation`;
- supply generation, revision, and operation epoch at upgrade time;
- accept the server-first snapshot or retained mutation stream;
- page history through the socket;
- send `send_message` and `resolve_request` only with an `interact` device
  grant, accepting `accepted`, `refused`, and `ambiguous` outcomes.

The Hub is the sole owner of conversation projection, ordering, pending request
state, connector observation, and action durability. An Overlord client must
not parse Claude or Codex transcripts, synthesize events, maintain a second
cursor ledger, or bypass the Hub with PTY input.

Until that client exists, no Latch conversation data is available to Overlord.
