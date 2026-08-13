# Fixtures

Language-neutral evidence retained across engine implementations.

```text
fixtures/
  harness/    # schema-first normalized events and raw Claude Code records
  protocol/   # legacy v1 worker-protocol corpus; inactive after the tmux swap
  vt/         # recorded PTY streams from real Claude Code and Codex sessions
```

The protocol corpus documents the retired worker and remains useful when
reading the archived implementation at `archive/latch-term-v1`.

The VT recordings must remain in the active tree. They capture terminal
behavior from real harness sessions under conditions that cannot be reproduced
reliably later. Future harness connectors and presentation work may use the raw
streams even though tmux now owns the live screen model.

Each VT case contains:

```text
vt/<case>/
  input.bin      # recorded terminal bytes
  meta.json      # command, terminal, geometry, and resize points
  expected.json  # historical normalized-screen assertions
```

Re-recording is not routine fixture maintenance. Add a new recording when a new
condition must be captured; do not overwrite an old real-session stream merely
because the archived renderer no longer runs in the workspace.

The harness schemas are the public observation contract. Rust connector types
and TypeScript consumer types are generated from them with
`scripts/generate-harness-types.py`. Each Claude Code case keeps the raw JSONL
record next to the expected normalized NDJSON stream so transcript changes can
be reproduced without depending on a particular implementation language. The
version-named cases are captured from live harness releases rather than
constructed records. Imported Overlord hook fixtures remain byte-for-byte
language-neutral inputs and guard the permission sidecar and secret-reduction
behavior during connector migration.
