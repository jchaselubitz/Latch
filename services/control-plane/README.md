# Latch Remote Link control plane

This service owns account/device identity, Mac-approved enrollment, the durable
grant directory, short-lived single-use relay admissions, renewable leases,
and the revocation outbox. It never accepts or forwards Remote Link frames,
gateway credentials, session identifiers, transcript content, paths, terminal
bytes, or device private keys.

The separately deployed relay receives signed room/role/expiry claims only.
Admission claims are short lived and redeemed once. Lease renewals produce a
signed extension that the endpoint forwards over the relay control channel.
Pairing revocation, device revocation, and the account relay switch invalidate
active rooms through the durable outbox and relay-private endpoint.

## Development

```sh
npm install
npm run typecheck
npm test
npm run build
```

PostgreSQL coverage is enabled with an explicitly disposable database:

```sh
TEST_DATABASE_URL=postgres://postgres:password@127.0.0.1:5432/latch_test npm test
```

The test suite drops and recreates the public schema. Never point
`TEST_DATABASE_URL` at a database containing data you need.

## API boundary

Owner bootstrap uses a one-time operator invitation. Device credentials are
returned once and only their digests are stored.

| Route | Caller | Purpose |
| --- | --- | --- |
| `POST /v1/operator/owner-invitations` | operator | Mint one owner invitation. |
| `POST /v1/accounts/claim` | invitation | Create the owner account. |
| `POST /v1/devices` | account | Register a host or controller identity. |
| `POST /v1/enrollments` | host | Open a bounded enrollment room. |
| `POST /v1/enrollments/:id/claim` | admission code | Register a provisional controller and issue its relay admission. |
| `POST /v1/enrollments/:id/complete` | host | Atomically commit the exact approved controller key and grant. |
| `GET /v1/remote-links` | device | Read current pinned peer key and grant revision. |
| `POST /v1/relay-admissions` | device | Mint a fresh single-use room admission. |
| `POST /v1/relay-admissions/redeem` | relay | Redeem once and begin the room lease. |
| `POST /v1/relay-leases/:id/renew` | device | Issue a signed lease-extension claim. |
| `GET/POST /v1/relay-revocations` | relay | Pull and acknowledge durable invalidations. |
| `PUT/DELETE /v1/push-registrations` | controller | Register or remove the phone's opaque APNs token. |
| `POST /v1/attention` | host | Ask for one generic attention alert to a paired phone, deduplicated by opaque event id. |

Enrollment also uses an independent 256-bit QR-only secret. That secret is
mixed into the Noise prologue by the endpoints and is never sent here.

## Configuration

Copy `.env.example`. Required secrets are:

- `OPERATOR_SECRET`
- `ADMISSION_PRIVATE_KEY_PEM` and `ADMISSION_KEY_ID`
- `RELAY_SERVICE_TOKEN`
- `RELAY_INVALIDATION_SECRET`

Attention notifications are optional and need `APNS_KEY_ID`, `APNS_TEAM_ID`,
and `APNS_PRIVATE_KEY_PEM` together, plus `APNS_TOPIC` (the iOS bundle id) and
`APNS_ENVIRONMENT` (`sandbox` for development-signed builds, `production` for
TestFlight/App Store signing). The payload is a fixed sentence; the service
stores only the opaque token and opaque event ids, removes the token when
Apple reports it invalid, and removes it on revoke or unpair. Without APNs
configuration the attention route answers `unconfigured` and nothing else
changes: notifications are best effort and never required for the phone's
own foreground refresh.

`RELAY_URL` must be the public `wss://` endpoint. In production, keep the
control plane and relay at one replica for this release, disable app sleeping
or scale-to-zero, and preserve the `Authorization` header on WebSocket upgrade.
Migrations apply at boot in filename order. `0005_remote_link.sql` and
`0006_push_attention.sql` add the Remote Link and attention state;
`0007_retire_ice_signaling.sql` is the coordinated-cutover migration: it
drops the retired `turn_credentials`, `rendezvous_offers`, `presence`,
`relay_tickets`, and `pairing_requests` tables and revokes every pairing and
phone identity that has no `remote_links` row, so "Pairing required" is the
honest state until the owner re-enrolls. Take a database backup before the
deploy that applies it. Operations are documented in
`docs/REMOTE_LINK_OPERATIONS.md`.
