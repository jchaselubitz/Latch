# coo:618 objective 4 — session management commands

Implemented `list`, `inspect`, `stop`, `rename`, `resize`, `prune`, `doctor`,
`config`, and `capabilities` with stable `--json` forms.

## What landed

- `crates/latch/src/cli/manage.rs`: full implementations deriving state from
  socket + `exit.json`, sorting list by idle, stop via `session.update{stopping}`
  (connection held until the worker observes it), rename with ingest-time
  sanitization into `meta.json`, resize via attach+resize, prune for
  exited/lost, doctor findings for real problems only, flat `config.toml`
  read/write, capabilities matching the Overlord discovery schema.
- `crates/latch/src/worker/meta.rs`: `update()` for atomic meta rewrite on rename.
- `crates/latch/src/main.rs`: wired every management subcommand to the library,
  with human and `--json` output.

## Done criteria

- Management-command tests green (`cli_json`, `cli_list`, focused e2e stop).
- `--json` schemas match the typed reports the suite asserts.
- `cargo clippy -p latch --all-targets -- -D warnings` and `cargo fmt` clean.
- Boundaries check passes.

## Deferred (objective 5 / other tickets)

- Nesting refusal, iTerm setup doc, hand verification of M1 exit criteria.
- Attachment-registry / resize-authority pin enforcement still `todo!` in M1b;
  `resize --pin` reports the requested pin flag after applying the size.
