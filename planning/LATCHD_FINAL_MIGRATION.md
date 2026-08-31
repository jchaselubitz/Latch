# Latchd final migration and rollback boundary

Date: 2026-08-31

Decision: the fallback window is closed. `latchd` is the sole session kernel
for current Latch releases.

## What was retired

- `latch-tmux` is absent from the release archive, direct installer, updater,
  repair checks, signing, notarization, version reporting, and doctor output.
- The pinned upstream manifest, checksum list, private patches, download/build
  script, raw-attach preflight, and tmux-only conformance and soak harnesses are
  deleted.
- `engine.rs` has one execution path. Create, attach, list, inspect, snapshot,
  history, resize, signal, remove, and Hub actions/events all use latchd's
  authenticated per-session socket. No kernel selector or subprocess adapter
  remains.
- CI and release verification exercise the real latchd binary and the
  three-member payload: `latch`, `latch-remote`, and `latchd`.

Historical design and incident records remain under `planning/`, `ai/history/`,
and `docs/DECISION_*`. They explain why the former kernel existed; they are not
current operator instructions.

## Installation, update, and repair contract

The archive manifest binds product version, target, and the exact three binary
names. Installer and updater validate the archive digest, manifest, signatures,
component versions, and latchd protocol before the first replacement. The
updater replaces helpers before the CLI and rolls back earlier sibling renames
if any later rename fails. A current CLI beside a missing or mixed-version
helper is repaired by reinstalling the complete payload.

Replacing the on-disk latchd executable does not affect a daemon that already
owns a session: the running process keeps its mapped image. New sessions use
the replacement.

## Runtime boundary

Every new session writes a protected `kernel.json` record naming `latchd`.
Current releases refuse a session directory with no record or an unsupported
kernel name and direct the operator to a pre-cutover release. They never guess,
rewrite the record, start another kernel, or attempt live migration.

Gateway and Desktop attachment use the same exclusive latchd surface as the
CLI. The Hub uses persistent control and event sockets for atomic submit, paste,
key, structured snapshot/history, reconnect resynchronization, and lifecycle
events. There is no capture polling or subprocess injection compatibility path.

## Rollback boundary

Rollback means installing the final dual-kernel release. That older CLI can
recover or close a legacy tmux-hosted session. Returning to it does not
terminate already-running latchd daemons because ownership resides in the
daemon processes, not the CLI image on disk.

There is deliberately no rollback selector in the current release.
Reintroducing one would require restoring the old binary, patch/build chain,
updater contract, and subprocess adapter as a coordinated historical release.

## Verification

- updater coverage validates three-member manifest rejection, missing and
  mixed-version repair, transactional rollback, and a running latchd process
  continuing across replacement;
- the real latch+latchd PTY suite covers session lifecycle, retained exits,
  byte-exact I/O, snapshot paint, detach/reattach and steals, geometry, daemon
  suspend/failure, gateway attachment, and persistent Hub control/events;
- boundaries, formatting, strict workspace clippy, all-target and doc tests,
  shell syntax, Desktop tests, and release-archive inspection form the final
  reproducible gate.

The final macOS run is green: architecture and both generated-contract checks;
formatting; workspace clippy with warnings denied; all workspace targets (126
Latch library tests, 14 real latch+latchd PTY/gateway/Hub cases, 22 latchd unit,
14 adversarial security, 18 real-daemon session cases, and every terminal,
transport, updater, and remote-access target); workspace doc tests; and 91
Desktop tests. A real optimized arm64 release archive was built and inspected:
its only members are `latch`, `latch-remote`, `latchd`, and
`latch-payload.json`; all component versions and the daemon protocol match.
Running that extracted CLI's doctor reports only `kernel: latchd` and the
matching `latchdVersion`.
