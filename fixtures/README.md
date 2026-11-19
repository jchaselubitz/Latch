# Fixtures

Language-neutral evidence retained across engine implementations.

```text
fixtures/
  conversation/ # captured Claude source + current Hub projections; invented Codex corpus
  harness/      # raw Claude Code hook and transcript captures from live releases
  remote-link/  # Remote Link v1 contract examples and the device-name policy corpus
  vt/           # recorded PTY streams from real Claude Code and Codex sessions
```

The VT recordings must remain in the active tree. They capture terminal
behavior from real harness sessions under conditions that cannot be reproduced
reliably later, and they drive the `latch-term` fidelity suite and the mobile
emulator tests.

Each VT case contains:

```text
vt/<case>/
  input.bin      # recorded terminal bytes
  meta.json      # command, terminal, geometry, and resize points
  expected.json  # normalized-screen assertions
```

Re-recording is not routine fixture maintenance. Add a new recording when a new
condition must be captured; do not overwrite an old real-session stream.

The harness cases keep each raw Claude Code JSONL record next to the normalized
stream it once produced. No test reads them any more; the version-named cases
are retained because they are captures from live harness releases, and the
conversation fixtures cite them as their provenance.
