# Latch control plane

An independent TypeScript service that gives paired Latch devices an account,
a directory, and enough short-lived connectivity metadata to reach each other.
It is deployed separately from every endpoint and from every other Latch component,
and it holds no terminal content and no gateway credentials.

## What it owns

- **Accounts and devices** — account registration, device enrolment with a
  pinned Noise static public key, device key rotation, and revocation.
- **Paired-device directory** — pairings a host device declares after the
  local QR confirmation on the unlocked Mac, plus the permission
  (`observe`/`interact`/`control`) attached to each.
- **Presence** — a device publishes short-lived connection candidates
  (90 seconds by default). A paired peer can read them; nobody else can.
- **Rendezvous** — a paired device swaps candidates with a currently-present
  peer and learns the peer's pinned identity key so it can verify the static
  key during the Noise handshake.
- **Cloudflare Realtime TURN** — after paired-device authorization, issuance
  of short-lived ICE configuration for that requesting device only.
- **STUN discovery** — device-authenticated public STUN configuration for
  server-reflexive candidate gathering, deliberately available even when
  relay fallback is disabled.

## What it deliberately does not own

Terminal bytes, transcripts, scrollback, session names, prompt answers, device
private keys, endpoint session keys, Cloudflare TURN API token, and the Latch gateway bearer token. None
of these has a column, a request field, or a log line here. The relay ticket is
not a gateway credential and cannot decrypt application traffic: end-to-end
encryption is established directly between the two endpoints, and this service
never participates in it. See
[`docs/REMOTE_ACCESS_THREAT_MODEL.md`](../../docs/REMOTE_ACCESS_THREAT_MODEL.md);
`src/privacy.test.ts` is the executable form of that boundary.

The Cloudflare API token stays server-side. TURN passwords are returned only in
the one-time credential response and are never stored; only Cloudflare-issued
usernames are retained until expiry so a device, pairing, or account switch can
revoke them immediately.

## API

All bodies are JSON, and unknown properties are rejected with `400` so nothing
can be smuggled into storage. Credentials are bearer tokens
(`Authorization: Bearer <token>`) and are returned exactly once at creation;
only a SHA-256 digest is stored.

| Method | Path | Credential | Purpose |
| --- | --- | --- | --- |
| `GET` | `/health/live` | none | Process liveness. |
| `GET` | `/health/ready` | none | Storage reachable and migrations applied. |
| `POST` | `/v1/accounts` | none | Register an account; returns `accountToken`. |
| `GET` | `/v1/account` | account | Account summary. |
| `PATCH` | `/v1/account` | account | Toggle `relayEnabled` (the relay kill switch). |
| `GET` | `/v1/account/events` | account | Coarse access events. |
| `POST` | `/v1/devices` | account | Enrol a device; returns `deviceToken`. |
| `GET` | `/v1/devices` | device | The caller's paired-device directory. |
| `POST` | `/v1/devices/:id/rotate-key` | device | Replace a device public key in place. |
| `POST` | `/v1/devices/:id/revoke` | account or device | Revoke a device. |
| `GET` | `/v1/devices/:id` | device | A device reads its own record and its host. |
| `POST` | `/v1/pairings/requests` | host device | Register a pairing code the Mac is displaying. |
| `POST` | `/v1/pairings/:pairingId/confirm` | scanned secret | Enrol a phone from a scanned QR code. |
| `POST` | `/v1/pairings` | host device | Mirror a locally approved pairing. |
| `GET` | `/v1/pairings` | device | Active pairings for the caller. |
| `DELETE` | `/v1/pairings/:peerId` | device | Unpair from either side. |
| `POST` | `/v1/presence` | device | Publish short-lived candidates. |
| `DELETE` | `/v1/presence` | device | Withdraw presence. |
| `GET` | `/v1/presence/:id` | device | Read a paired peer's presence. |
| `POST` | `/v1/rendezvous` | device | Offer candidates to a present peer. |
| `GET` | `/v1/rendezvous` | device | Collect and consume inbound offers. `?wait=<0-25>` holds the request until an offer arrives or the wait elapses. |
| `GET` | `/v1/ice-servers` | device | Get STUN-only ICE servers for direct candidate gathering. |
| `POST` | `/v1/turn-credentials` | device | Issue short-lived Cloudflare ICE servers for an active paired peer. |

Errors are `{ "error": "<machine code>", "reason": "<sentence>" }`. A pairing
confirmation distinguishes unknown/consumed/expired (`404`), a mismatched
secret (`403`), and an already-enrolled identity (`409 already_paired`),
because the recovery differs: show a new code, or stop trusting the code.

The QR flow keeps the Mac authoritative. The Mac generates the pairing id and
one-time secret locally and sends only a domain-separated SHA-256 digest of
that secret; the phone proves it was in front of the unlocked Mac by presenting
the secret itself. A pairing request is single-use, expires after five minutes,
and is capped at eight pending per host — the same limits the local Rust
implementation enforces.

Revocation is immediate rather than eventual: a revoked device stops
authenticating on its next request, and its presence, pending offers, and
issued TURN usernames are revoked at Cloudflare in the same request path.

## Local development

```bash
npm install
npm run typecheck
npm test

cp .env.example .env      # point DATABASE_URL at a local PostgreSQL
npm run build && npm start
```

The suite is hermetic: it runs the real HTTP server against an in-memory
store. The PostgreSQL contract test runs only when `TEST_DATABASE_URL` names a
throwaway database, because it drops and recreates the `public` schema:

```bash
TEST_DATABASE_URL=postgres://... npm test
```

## Migrations

Forward-only SQL files in `migrations/`, applied in filename order inside one
transaction each and guarded by a PostgreSQL advisory lock so concurrent
replicas cannot race. Applied files are checksummed: editing a migration that
already ran is a boot error, not a silent divergence. They run on boot
(`MIGRATE_ON_BOOT`, default on) and can also be applied standalone with
`npm run migrate`.

## Deployment

Railway builds this directory (`services/control-plane` as the service root)
and health checks `/health/ready`, so a deploy that cannot reach PostgreSQL or
finish migrations never takes traffic. Pushes to `main` deploy through
Railway's GitHub integration; `.github/workflows/control-plane.yml` runs the
typecheck, build, and tests on every push and pull request that touches this
directory.

### Railway resources

The `latch` Railway project (environment `production`) holds:

| Service | Purpose |
| --- | --- |
| `Latch` | This control plane. Source: GitHub `jchaselubitz/Latch`, branch `main`, root directory `services/control-plane`, health check `/health/ready`. |
| `latch-postgres` | PostgreSQL 16 with a persistent volume at `/var/lib/postgresql/data` and a TCP proxy for administrative access. |

`DATABASE_URL` on the control-plane service is a reference to
`${{latch-postgres.DATABASE_URL}}`, so it resolves over Railway's private
network and the database is never reached over the public internet by the
service itself. Cloudflare TURN is deliberately called only by this backend;
it refuses credential issuance until both Cloudflare variables are set.

Configuration is environment-only and validated at boot — see
[`.env.example`](.env.example) for the full list.

### Railway variables and security

Set these as sealed Railway variables on the `Latch` control-plane service:

| Variable | Value |
| --- | --- |
| `CLOUDFLARE_TURN_KEY_ID` | Cloudflare TURN key ID (not its secret). |
| `CLOUDFLARE_TURN_API_TOKEN` | API token restricted to generating and revoking credentials for that TURN key. |
| `CLOUDFLARE_TURN_TTL_SECONDS` | `120` (or another short session window). |

Never put the API token in a mobile app, endpoint configuration, logs, or a
repository. Use a separate Cloudflare TURN key and API token per environment;
rotate the API token and TURN key together if either is exposed. Clients should
perform ICE restart/credential refresh for long-lived sessions.
