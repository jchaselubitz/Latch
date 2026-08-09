# coo:618 objective 1 — CLI and M1 end-to-end test suite

Wrote the CLI/M1 test suite before implementation.

## What landed

- Library stubs under `crates/latch/src/cli/` with `todo!()` bodies: create, attach, manage, nesting, term (raw-mode guard), and typed `--json` report shapes.
- Clap surface extended where the plan required it: `--name`/`--title` on `shell`/`run`, `--json` on `create`.
- Binary dispatch folded into `main.rs` so the library can own the `cli` module name.
- Integration tests: `cli_create`, `cli_attach`, `cli_json`, `cli_nesting`, `cli_list`, `cli_latency`, `cli_e2e`, plus harness helpers in `tests/support`.

## Done criteria

- `cargo test -p latch` builds and runs; new tests fail with `todo!` / "lands in M1", not compile errors.
- `cargo clippy -p latch --all-targets -- -D warnings` and `cargo fmt` clean.
- Boundaries check passes.

## Not done (later objectives)

Implementation of create, attach, management commands, nesting, and iTerm docs.
