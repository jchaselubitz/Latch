# Remote access in Latch Desktop

This documents the desktop half of remote access: what the app controls, what
it deliberately does not touch, and how the "the helper never exposes `latch
serve` publicly" property is verified.

## Process boundary

Three processes, with a one-way trust flow:

```
Latch Desktop  ──spawns──▶  latch remote-access lan-serve  ──spawns──▶  latch serve
(user control)              (authenticated transport,                  (plaintext /v1
                             permission enforcement,                    gateway, loopback
                             audit trail)                               only)
```

The app spawns exactly one remote-access process: the helper. It never spawns
`latch serve`, never reads or holds the gateway bearer token, and never learns
the gateway address. `RemoteAccessSupervisor.arguments(bind:)` is the only
place the helper's argument vector is built, and it refuses `serve`,
`--allow-remote`, and `--token-file` outright rather than filtering them.

The helper owns the gateway. It mints a fresh 32-byte bearer token before every
launch, starts `latch serve --bind 127.0.0.1:0`, and reads back the port from an
owner-only readiness file. `GATEWAY_BIND` in `crates/latch/src/cli/remote_access.rs`
is a constant, not a parameter, so no caller can widen it; the helper also
re-checks that the address it got back is loopback and aborts if it is not.

## Lifecycle

Remote access is off until the user turns it on in Settings → Remote Access.

- **Enable** runs `latch remote-access enable`, which creates the Mac device
  identity on first use (private key in the login Keychain under
  `co.cooperativ.latch.remote-access`; `identity.json` holds public metadata
  only), then starts the helper and waits for it to advertise a listener.
- **Restore** happens only when the CLI already reports remote access as on.
  The app never re-enables it on the user's behalf.
- **Supervise** restarts a helper that dies, with a capped 1/2/5/10/30-second
  backoff so a broken CLI cannot become a spin loop. Each failure is surfaced
  in Settings rather than retried silently.
- **Disable** stops the helper first, then runs `latch remote-access disable`,
  which cancels pending pairing material and removes the gateway credential and
  readiness files. Quitting the app terminates every helper it started, and the
  helper takes its supervised gateway down with it.

## What the app reads

`latch remote-access status --json` is the app's only lifecycle read. It
reports the switches, the Mac's opaque device id and *public* identity key,
device counts, and the helper's LAN listener address. It never reports the
gateway address, the gateway token, or any private key, and reading it never
creates an identity or starts anything — so the app can poll it while remote
access is off.

The listener address comes from `runtime/lan-ready.json`, which the helper
writes when it binds and removes when it stops — on Ctrl-C and on SIGTERM,
which is what the app sends. The document also carries the helper's pid, and a
reader discards it when that process is gone, so neither a hard kill nor a
crash can leave the app claiming a listener that nothing is serving.

## Permissions

Devices hold `observe`, `interact`, or `control` as a strict ladder. Enforcement
is in the helper, which resolves the permission required by the incoming request
line and refuses before the request ever reaches `/v1`. The app mirrors the same
ladder in `DevicePermission.permits(_:)` purely so the UI cannot offer an action
the device would be refused for. Changing a grant or revoking a device takes
effect immediately, including for a connection that is already open (the helper
re-checks device state every 250 ms).

## Pairing

Settings renders the CLI's pairing material verbatim: the same camelCase JSON
`latch remote-access pair create --json` emits, shown both as a QR code and as
selectable text. The desktop invents no format of its own, so the phone's
`PairingPayload` parser stays the single definition of the wire shape. The QR
code is generated in-process from a string already in memory; nothing is
written to disk and no service is contacted, and the app never persists the
secret.

## Audit

Security and connection events come from the helper's bounded local audit trail
(at most 1,024 events or 512 KiB; timestamp, coarse event, opaque device id,
result — no names, addresses, keys, or session content). Settings shows security
events and connection events as two separate lists. Diagnostics export writes the
content-free bundle locally; nothing is uploaded.

## Verification

Swift, in `apps/LatchDesktop/Tests/LatchDesktopTests/RemoteAccessTests.swift`:

- the helper launch vector is `remote-access lan-serve` and contains no
  `serve`, `--allow-remote`, `--token-file`, or loopback address;
- the helper bind is an ephemeral port and every loopback form
  (`127.0.0.1`, `127.x`, `localhost`, `::1`) is refused;
- the permission ladder matches the gateway contract;
- status/devices/audit/pairing decode the CLI's JSON contracts, including the
  identityless disabled Mac;
- pairing material is handed to the phone as the CLI's own camelCase JSON —
  the shape `PairingPayload` on the phone parses — and carries only the
  one-time secret and the public identity.

Rust, in `crates/latch/src/cli/remote_access.rs`:

- `supervised_gateway_is_always_launched_on_an_ephemeral_loopback_port`
  asserts the gateway argument vector;
- `the_helper_never_advertises_the_gateway_and_status_tracks_the_listener`
  asserts a loopback LAN listener is refused, that the readiness document names
  no loopback address, token, or key, that a document whose helper is gone is
  not reported, and that disabling retracts it;
- `status_never_creates_an_identity_or_reveals_a_private_key`.

Existing coverage in `serve_refuses_non_loopback_bind_without_allow_remote`
remains the backstop for the gateway itself.
