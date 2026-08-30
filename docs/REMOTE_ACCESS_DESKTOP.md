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
re-checks device state every 250 ms) — and this includes a *downgrade*, not
only a revocation: the check compares the live grant against what the
connected route requires, so dropping a device from `control` to `interact`
closes its terminal stream while leaving a lesser-permission connection open.

The device row in Settings → Remote Access renders this as two separate
decisions rather than one severity dropdown: a base-access picker
(Observe/Interact) and an explicit "Allow terminal" switch mapped to `control`.
The switch remembers what the device held underneath it, so granting the
terminal and taking it away again returns the device to Interact or Observe as
it was, not to a default. Desktop-approved pairings begin with terminal access
allowed (`control`); a direct CLI `pair confirm` still defaults to `interact`
unless its caller explicitly supplies `--permission control`. A grant writes
the local device store first — that is what the helper enforces — and is
mirrored to the control plane's pairing row afterward; a mirror failure is
reported but never rolls the local grant back.

## Keeping the Mac reachable

Two things this app does are specifically about the Mac being reachable while
nobody is at the keyboard, and one hard constraint neither of them removes.

- **Sleep prevention.** While Remote Access is on and at least one phone is
  currently connected, the app holds an `IOPMAssertion`
  (`kIOPMAssertionTypePreventUserIdleSystemSleep`, in `SleepAssertion.swift`)
  so the Mac does not idle-sleep out from under an open terminal. The
  assertion is released the moment the last phone disconnects or Remote Access
  is turned off — it is not held simply because the feature is enabled.
- **Never relay / Tailscale.** The `neverRelay` switch backs the existing
  `latch remote-access relay disable` refusal with a matching filter on
  presence: when set, the Mac publishes only host candidates (dropping
  server-reflexive ones), and the phone treats a presence host candidate —
  including a Tailscale `utun` address, `100.x` or the `fd7a:` ULA form — as a
  plain TCP Noise target with no ICE gathering required. This is what makes a
  tailnet a usable path with the relay disabled entirely: reachability comes
  from Tailscale's own routing, not from STUN/TURN.
- **The constraint neither of these removes: Latch Desktop must be running and
  the Mac must be awake.** There is no server component independent of this
  app — the helper it supervises is what holds the gateway and the ICE
  responder, and a slept or shut-down Mac, or a quit desktop app, presents no
  presence, answers no rendezvous offer, and accepts no LAN connection. The
  phone's control-plane 409 `target_offline` case is surfaced as "Your Mac is
  asleep or Latch is not running" for exactly this reason — it is a statement
  about this constraint, not a generic network error. Sleep prevention narrows
  the window (a Mac with an active phone connection will not idle-sleep on its
  own) but does not eliminate it: a lid close, a manual sleep, a reboot, or
  quitting the app all still take the Mac offline for remote access.

## Pairing

Settings renders the CLI's pairing material in the CLI's own camelCase JSON,
shown both as a QR code and as selectable text. The desktop invents no format
of its own, so the phone's `PairingPayload` parser stays the single definition
of the wire shape. The QR code is generated in-process from a string already in
memory; nothing is written to disk, and the app never persists the secret.

### The desktop is the host adapter

The CLI has no HTTP client — the every-window startup budget rules one out — so
it cannot know a control-plane address or register anything with one. The
material it emits therefore names no address, and a phone that scans it has
nowhere to present the secret: that is the "this pairing code does not say where
to enroll" failure. `ControlPlaneHost` closes the gap from this side.

When a control-plane address is set in Remote Access settings, creating a code:

1. enrolls this Mac once as a `host` device (`POST /v1/accounts`, then
   `POST /v1/devices` with the CLI's public identity), keeping the account and
   device tokens in the Keychain under `co.cooperativ.latch.control-plane`. A
   locally rotated Mac key is carried over with
   `POST /v1/devices/:id/rotate-key` rather than re-enrolling, which would
   strand this Mac's pairings. Changing the address forgets the credentials,
   because a token issued by one deployment names nothing in another;
2. registers the displayed code with `POST /v1/pairings/requests`, sending the
   pairing identifier and `sha256("latch/v1/pairing " + secret)` — never the
   secret, so a control-plane breach cannot answer a scan on this Mac's behalf.
   `expiresAt` is left to the service, which applies the same five-minute
   ceiling without a clock disagreement rejecting the request;
3. attaches `controlPlane` and `macName` to the material before it is rendered.

Registration failing fails the code rather than displaying an unusable one. A
Mac with no address configured still shows a code and says plainly that it
carries no address; the phone can then be given the address by hand.

#### The name this Mac enrolls under

The control plane stores labels from a fixed set — letters, digits, spaces, and
`. _ ' ( ) -`, up to 64 code points — and answers anything else with a 400.
macOS names a Mac "Jake’s MacBook Pro" with the typographic apostrophe, so the
default name of an ordinary Mac is outside that set and the first call of the
first pairing is refused. `ControlPlaneLabel.enrollable` reduces the name before
it is sent, folding typographic punctuation to its ASCII equivalent first so the
name arrives as "Jake's MacBook Pro" rather than losing the character to a
space. The phone does the same to its own name for the same reason.

#### Credentials the control plane no longer knows

A redeployed or reset service has no row for the device this Mac enrolled as,
and it issues no replacement, so the Keychain would otherwise keep a credential
that can only ever be refused — with no way to clear it from the UI. A 401 or
404 from the registration or key-rotation call is therefore treated as a stale
enrollment: the stored credentials are dropped and this Mac enrolls again, once.
That restores no authorization on its own, because the new host device holds no
pairings until a phone scans a code for it.

### Failures are raised where the button is

Creating a code enrolls this Mac and registers the code, both network round
trips, and a failure produces no code at all. The button shows a progress
indicator for as long as that takes, and a failure is raised as an alert rather
than left to the error row at the end of the form, which is below the fold in a
window this size. Without either, a refused pairing is indistinguishable from a
button that does nothing.

### Completing the pairing locally

The control plane holds the directory; this Mac holds the authorization. While
the sheet is open the app polls `GET /v1/devices` for a client device that was
not there before, then runs `latch remote-access pair confirm` with the phone's
public key so the local device store — the thing the helper actually checks —
has the phone in it. The phrase the CLI returns is shown for the person to
compare against the phone's screen, which is what rules out a substituted key.

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
  one-time secret, the public identity, and the public control-plane address.

Swift, in `apps/LatchDesktop/Tests/LatchDesktopTests/ControlPlaneHostTests.swift`:

- the registered pairing carries the domain-separated digest and never the
  secret, pinned against the vector `credentials.ts` produces;
- a code is addressed only after registration succeeds, and pairing without a
  configured control plane is refused rather than guessed;
- this Mac enrolls once, rotates in place when its key changes, and never
  reuses credentials issued by a different deployment;
- a Mac named the way macOS names one enrolls rather than being refused, and a
  name is reduced to the label set the service accepts;
- credentials the control plane no longer knows are replaced once, while an
  ordinary refusal is reported rather than answered by re-enrolling;
- only live client devices are reported as paired.

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
