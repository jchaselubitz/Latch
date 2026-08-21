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
