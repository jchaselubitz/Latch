# Remote Link operations

Date: 8 September 2026. Owner: the sole Latch operator. This is the
operational reference for the deployed Remote Link services and the Mac and
phone endpoints: what is deployed, how to verify it, and the runbooks for
deployment, revocation, key rotation, incidents, and rollback. The design is
in [DECISION_REMOTE_ACCESS_TRANSPORT.md](DECISION_REMOTE_ACCESS_TRANSPORT.md);
the acceptance evidence is in
[REMOTE_ACCESS_FIELD_VERIFICATION.md](REMOTE_ACCESS_FIELD_VERIFICATION.md).

Secrets are never written here. Names, locations, and lifetimes are.

## 1. Deployed topology

| Component | Where | Identity |
| --- | --- | --- |
| Control plane | Railway project `latch`, service `Latch`, environment `production`, region `europe-west4`, root `services/control-plane`, source GitHub `jchaselubitz/Latch` branch `main` | `https://latch-production-7e52.up.railway.app`; `GET /health/ready` reports the git commit it was built from as `release` and the applied migrations |
| Relay | Same project, service `latch-relay`, root `services/relay` | `wss://latch-relay-production.up.railway.app/v1/connect`; `/health/live`, `/health/ready` |
| Database | Same project, service `latch-postgres` (PostgreSQL) | referenced by the control plane as `DATABASE_URL` |
| Mac endpoint | `~/.local/bin/{latch,latch-remote,latchd}` (coordinated signed payload) plus `/Applications/Latch.app` | `latch --version`, `latch remote-access status --json`, SHA-256 of the three binaries |
| Phone endpoint | `dev.cooperativ.latch.mobile` on the owner's iPhone, development-signed | build hash recorded in the field report |

Both services run **exactly one replica** with Railway app sleeping
**disabled**: a sleeping control plane adds its wake time to every cold open,
and a sleeping relay drops every waiting Mac socket. Health checks target
`/health/ready` on both. The relay's readiness answers 503 while draining so
the platform stops routing new admissions to a replica that is shutting down.

### Control-plane configuration

| Variable | Purpose | Lifetime / notes |
| --- | --- | --- |
| `DATABASE_URL`, `DATABASE_POOL_SIZE`, `DATABASE_SSL_REJECT_UNAUTHORIZED` | PostgreSQL | Railway reference |
| `MIGRATE_ON_BOOT` | apply `migrations/*.sql` at boot under an advisory lock | `true` |
| `OPERATOR_SECRET` | mints one-use owner invitations | rotate at will; only the operator holds it |
| `ADMISSION_PRIVATE_KEY_PEM`, `ADMISSION_KEY_ID`, `ADMISSION_ISSUER` | signs relay admission and lease-extension claims (Ed25519, compact EdDSA) | key id `latch-remote-link-2026-09a`; issuer is the control-plane origin |
| `RELAY_URL` | the public `wss://` endpoint handed to endpoints | `wss://latch-relay-production.up.railway.app/v1/connect` |
| `RELAY_SERVICE_TOKEN` | the relay's credential for ticket redemption | shared with the relay |
| `RELAY_INVALIDATION_SECRET` | the outbox worker's credential for `POST /private/v1/invalidate` on the relay | shared with the relay |
| `TRUST_PROXY` | use the edge's `X-Forwarded-For` for per-IP admission budgets | `true` on Railway |
| `REMOTE_ADMISSION_RATE_*`, `RATE_LIMIT_PER_MINUTE`, `MAX_DEVICES_PER_ACCOUNT`, `ATTENTION_RATE_PER_HOST` | bounds | defaults documented in `.env.example` |
| `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_PRIVATE_KEY_PEM`, `APNS_TOPIC`, `APNS_ENVIRONMENT` | attention notifications | optional; `sandbox` for the development-signed phone build |

Retired variables that must not exist after cutover: `CLOUDFLARE_TURN_API_TOKEN`,
`CLOUDFLARE_TURN_CREDENTIAL_TTL_SECONDS`, `CLOUDFLARE_TURN_KEY_ID`,
`PRESENCE_TTL_SECONDS`, `RELAY_TICKET_TTL_SECONDS`, `RENDEZVOUS_TTL_SECONDS`.
Migration `0007_retire_ice_signaling.sql` drops the tables they fed.

### Relay configuration

| Variable | Purpose |
| --- | --- |
| `ADMISSION_ISSUER`, `ADMISSION_KEY_ID`, `ADMISSION_PUBLIC_KEY_PEM` | verifies claims signed by the control plane |
| `ADMISSION_PREVIOUS_KEY_ID`, `ADMISSION_PREVIOUS_PUBLIC_KEY_PEM` | optional; the one previous key accepted during a rotation overlap |
| `CONTROL_PLANE_URL`, `RELAY_SERVICE_TOKEN` | ticket redemption at `POST /private/v1/relay/redemptions` |
| `RELAY_INVALIDATION_SECRET` | authenticates room invalidation from the control plane |
| `PORT` | Railway-provided; the relay serves plain WebSocket behind the platform TLS edge |

Lifetimes that the runbooks depend on: owner invitation 10 minutes,
enrollment 5 minutes, admission claim 60 seconds and single use, room lease
10 minutes renewed by a signed extension forwarded over the WSS control
channel, relay WebSocket ping every 15 seconds, Noise keepalive every 15
seconds with the peer declared dead after 45 seconds of silence, attention
events 10 minutes.

## 2. Verification checks after any deployment

Run these from a Mac shell; none needs a device key.

```sh
CP=https://latch-production-7e52.up.railway.app
RELAY=https://latch-relay-production.up.railway.app

# Control plane is up, on the expected commit, with migrations 0005-0007 applied.
curl -fsS $CP/health/ready | python3 -m json.tool

# Relay is live and accepting.
curl -fsS $RELAY/health/live; curl -fsS $RELAY/health/ready

# Authorization-header passthrough on the WebSocket upgrade. A missing header
# is refused with 401; a well-formed but unverifiable token is refused with
# 403. Seeing 403 proves the header reached the relay intact through the edge.
curl -s -o /dev/null -w '%{http_code}\n' -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==' $RELAY/v1/connect   # 401
curl -s -o /dev/null -w '%{http_code}\n' -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==' \
  -H 'Authorization: Bearer eyJhbGciOiJFZERTQSJ9.e30.AA' $RELAY/v1/connect                          # 403

# Address family published by the relay hostname. No AAAA means an IPv6-only
# phone reaches the relay through its carrier's NAT64, and the field report
# must say so rather than claiming native IPv6 client-to-relay.
dig +short A latch-relay-production.up.railway.app
dig +short AAAA latch-relay-production.up.railway.app

# TLS chain validates with the system trust store (the endpoints use
# platform validation; a private CA would fail here too).
curl -sSI $RELAY/health/live >/dev/null && echo tls-ok
```

Proxy idle limit: hold one admitted socket (a Mac helper waiting for its
peer is exactly that) for at least 15 minutes and confirm the relay's ping
keeps it open; `latch remote-access audit --json` on the Mac shows no
`link_closed` during the hold. Record the measured hold time in the field
report; the relay heartbeat is what keeps the edge from idling the socket.

Replica and sleeping checks: Railway service settings for both services show
`numReplicas: 1` (`railway.json` in each service root pins it) and app
sleeping off. The Railway MCP `get-service-config` output records
`multiRegionConfig.<region>.numReplicas` and the CLI `railway service`
settings show sleeping; check both after any settings change.

## 3. Runbook: deploy

1. Control plane: push `main`. `.github/workflows/control-plane.yml` runs
   typecheck, tests (in-memory and PostgreSQL 16 service container), build,
   and `railway up`; Railway's GitHub integration deploys the same commit.
   Migrations apply at boot (`MIGRATE_ON_BOOT=true`), forward only, under an
   advisory lock, refusing a changed already-applied file.
2. Relay: the `latch-relay` service builds `services/relay` from the same
   branch (`npm run build`, `npm start`). Variables listed above must exist
   before the first boot; `main.ts` refuses to start without them.
3. Set or rotate variables with `skipDeploys` and redeploy once, so a partial
   variable set never boots.
4. Run section 2. A control-plane boot log line `listening` carries
   `relayConfigured: true` when the admission key and service token are both
   present, and `apnsConfigured` when APNs is.
5. Endpoints: install the coordinated Mac payload (`scripts/install-cli.sh`
   or a local `scripts/release-cli.sh` archive) and the notarized Desktop
   app, then the phone build. Never install an unsigned `latch-remote` or
   `latch`: the Keychain grants the Mac identity only to the code signature
   the item trusts, and an unsigned helper blocks on the Keychain prompt
   before it starts its gateway (seen during cutover diagnosis). Desktop supervises `latch-remote`; quit and
   relaunch Desktop after replacing the binaries so the helper it supervises
   is the new one. Existing `latchd` sessions keep running on the already
   loaded daemon image.
6. Take a Railway PostgreSQL backup before any deploy that includes a
   migration; migration `0007` is destructive by design.

Owner invitation (needed once per Mac, and after `forget enrollment`):

```sh
curl -fsS -X POST $CP/v1/operator/owner-invitations \
  -H "Authorization: Bearer $OPERATOR_SECRET" -H 'content-type: application/json' \
  -d '{"ttlSeconds":600}'
```

Paste the returned `invitation` into Settings → Remote Access → "One-use
owner invitation"; Desktop exchanges it once and stores only the resulting
account and host tokens in the Keychain.

## 4. Runbook: revocation

Revocation is Mac-owned; the directory mirrors it and the relay is told.

- **One phone, immediately:** Desktop → Remote Access → Revoke, or
  `latch remote-access revoke <deviceId>`. The local record flips to
  revoked, every active stream for that key closes on the next 250 ms check,
  the control plane sets `revoked_at`, bumps the grant revision, and queues
  the room in `relay_revocation_outbox`; the worker delivers
  `POST /private/v1/invalidate` to the relay within about one second and the
  relay closes both roles. `latch remote-access audit --json` shows
  `device_revoked`; the phone shows "Pairing required" and stops retrying.
- **Downgrade instead of revoke:** `latch remote-access grant <deviceId>
  observe|interact|control`. Streams above the new grant close; the rest
  continue; the grant revision increments so a stale admission cannot
  restore the old permission.
- **Everything at once:** `POST /v1/accounts/relay {"enabled": false}`
  (Desktop's relay switch) invalidates every room for the account through
  the outbox and refuses new admissions until re-enabled. Local sessions are
  unaffected.
- **Lost or stolen Mac:** revoke every phone from the Mac if it is still
  reachable; otherwise rotate `OPERATOR_SECRET`, mint no invitations, and
  use the database to set `revoked_at` on the host device row; re-enroll a
  new host with a fresh invitation.
- **Verify:** the outbox row is acknowledged (`acknowledged_at` set), the
  relay log shows the room closed, and a phone admission for that pairing is
  refused with 403 at the relay (redemption denied).

Revocations are never undone by rollback (section 7).

## 5. Runbook: key and secret rotation

Rotation is credential hygiene, not compatibility: each step keeps exactly
one implementation.

**Admission signing key** (control plane private, relay public):

1. `openssl genpkey -algorithm ed25519 -out new.pem && openssl pkey -in new.pem -pubout -out new.pub`
2. Relay: set `ADMISSION_PREVIOUS_KEY_ID`/`ADMISSION_PREVIOUS_PUBLIC_KEY_PEM`
   to the current key and `ADMISSION_KEY_ID`/`ADMISSION_PUBLIC_KEY_PEM` to
   the new one; redeploy the relay. It now verifies both.
3. Control plane: set `ADMISSION_PRIVATE_KEY_PEM` and `ADMISSION_KEY_ID` to
   the new key; redeploy. New admissions and lease extensions use the new
   key immediately.
4. After 11 minutes (the 60-second admission lifetime plus the 10-minute
   lease horizon), remove the two `ADMISSION_PREVIOUS_*` variables from the
   relay and redeploy. Existing links are unaffected: the relay checks keys
   only at admission and extension.

**Relay service token / invalidation secret:** set the new value on the
relay first and redeploy, then on the control plane and redeploy. Between
the two deploys redemptions fail closed for a few seconds and endpoints
retry with backoff; queued invalidations stay durable in the outbox until
the relay accepts them.

**Operator secret:** set the new value; nothing else references it.

**APNs key:** create a new key in the Apple Developer portal, set
`APNS_KEY_ID` and `APNS_PRIVATE_KEY_PEM`, redeploy, then revoke the old key
in the portal. Device tokens are unaffected.

**Endpoint identities:** a phone key is rotated without re-pairing by
`latch remote-access rotate-device-key <deviceId> --public-key <hex>` after
the phone generated a new key and presented it over the authenticated link;
the Mac key rotates through Desktop, which calls the directory's host
rotation and bumps the key generation. Re-pairing is the fallback when a
key is suspected compromised.

## 6. Runbook: incidents

| Symptom | Likely cause | Action |
| --- | --- | --- |
| Phone shows "Mac unavailable through the relay" while the Mac is awake | The helper is not waiting in its room: Desktop not running, Remote Access off, relay unreachable from the Mac, or admission refused | `latch remote-access status --json`; Desktop shows the helper status line (`connecting`, `waiting_for_peer`, `ready`, `link_closed`, `offline`); check `curl $RELAY/health/ready`; Desktop re-admits the helper with backoff while the control plane is unreachable |
| Helper stuck in `waiting_for_peer` on a socket that is still ESTABLISHED | The network path silently stopped delivering, so no close frame can ever arrive; before the carrier had a silence bound the helper waited there forever | The helper now bounds relay silence at 45 s (the relay pings every 15 s), reports `offline` with reason `relay_silent`, and asks Desktop for a fresh admission. Seeing `relay_silent` in the helper status line is that recovery working, not a fault; repeated `relay_silent` on one network points at the path, not the Mac |
| Every admission fails at the relay with 403 | Wrong or rotated admission key, or the redemption call to the control plane failing (bad `RELAY_SERVICE_TOKEN`, control plane down) | Relay logs show `ticket redemption refused`; compare `ADMISSION_KEY_ID` on both services; restore the pair |
| Links drop every 10 minutes | Lease extension not reaching the relay | The endpoint forwards the signed extension over the control channel; check the control-plane `POST /v1/relay-leases/:id/renew` log and the relay's `lease expired` closes |
| Relay restarts or is redeployed | Platform restart, crash, or deploy | Both endpoints re-admit through the same gateway; local `latchd` sessions are never touched; measured 8 September: 10 of 10 restarts recovered on the phone in 1.2–1.8 s |
| Helper or gateway process dies | Crash, kill, or Desktop swap | Desktop relaunches the helper on a 1, 2, 5, 10, 30 s schedule that resets after the helper has been up for 60 s; a genuine crash loop therefore waits up to 30 s between attempts. The phone's LAN phase is bounded to 1.5 s, so a helper whose LAN listener moved costs at most that before the relay carries the reconnect |
| Control plane outage | Deploy failure or database outage | Existing links continue until their 10-minute lease cannot be extended; new admissions fail closed; nothing local is affected. Railway `environment-status` and `get-logs`; roll back the deployment to the previous successful snapshot if a deploy caused it |
| Revocation not taking effect at the relay | Outbox worker cannot reach the relay or wrong `RELAY_INVALIDATION_SECRET` | Rows stay pending in `relay_revocation_outbox`; the Mac has already closed the streams locally, so exposure is bounded to the relay room lifetime (10 minutes); fix the secret and the worker drains the queue |
| App sleeping got re-enabled or replicas scaled up | Settings drift | Set back to one replica, sleeping off; room affinity requires a single relay replica |
| Suspected key compromise (service) | Leaked variable | Rotate per section 5; invalidate all rooms with the relay switch; audit access events |
| Suspected endpoint compromise | Lost phone or Mac | Revoke per section 4; re-pair explicitly |
| Attention notifications stop | APNs key revoked, token invalidated, or environment mismatch | Control-plane log `apns` errors; Apple's `BadDeviceToken` removes the token and the phone re-registers on next foreground; `APNS_ENVIRONMENT` must match the build's signing |

Alerts to keep on: Railway deployment failure and crash notifications for
both services (project settings → notifications), a usage limit on the
workspace so relay egress cannot run unbounded (owner sets the value; see
section 8), and the health checks above. The control plane logs one JSON
line per request without device identifiers beyond opaque ids; the relay
logs aggregate counts and short-lived attempt ids only.

## 7. Runbook: rollback to the archived release

The archived coordinated release is `v0.2609070836.0` (GitHub release with
the signed CLI payload and `Latch-0.2609070836.0-macos.zip`) built from
`a2ab11dd44c8f68a887f6d276daa3af8b0ca7e97`, tagged
`remote-ice-baseline-2026-09`. Its control plane is the Railway deployment
`afda5e76-7bd3-4576-bf63-ad56659048b8` (snapshot
`15e8ab38-1ab2-409f-b36d-c1266ac8ec9c`). Rolling back is a deliberate
operator decision, not an automatic downgrade of a live link.

1. Remove the Remote Link endpoints first: quit Desktop, install the archived
   CLI payload (`scripts/install-cli.sh` pins the newest release; download the
   archived archive explicitly and verify its checksum), install the archived
   Desktop app, and reinstall the archived phone build.
2. Restore the control plane by redeploying the archived Railway snapshot and
   restoring the pre-cutover PostgreSQL backup (migration `0007` dropped the
   tables the archived service needs). Re-add the retired TURN variables
   from the owner-only backup at `~/.latch/remote-access/rollback/` (created
   during cutover, mode 0700; it holds variable names and values for the
   control plane only, never session secrets).
3. Grants are not resurrected: before restoring the backup, list the devices
   revoked since it was taken (`latch remote-access devices --json` and the
   control-plane `access_events`), and after the restore revoke each of them
   again through the archived service's revoke route. The Mac's local
   `devices.json` is shared by both versions, so local revocations survive
   the rollback on their own; re-pair anything that must work again.
4. Delete the `latch-relay` service or leave it stopped; it has no state.

## 8. Cost model and bandwidth

Relay cost is compute for one always-on replica plus relayed bytes. Every
byte a phone sends crosses the relay twice (in from one socket, out to the
other), so relay egress per link is roughly the sum of both endpoints'
application traffic. Idle cost per active link is bounded by the keepalives:
one empty authenticated Noise record (a few dozen bytes plus WebSocket
framing) every 15 seconds from each endpoint and a relay ping/pong pair
every 15 seconds per socket.

Measured workload bandwidth (relay bytes per minute, both directions) is
recorded in the field report once the soak has run; until then no monthly
figure is claimed. The measurement plan is: idle observation of one session,
conversation chat at a typical cadence, an interactive terminal, and a
high-output command, each for ten minutes with `latch remote-access audit`
and the relay's aggregate byte counters read before and after. Railway's own
pricing page is the source for compute and egress rates; this document does
not restate prices.

## 9. Evidence and identities

Installed identities are recorded in
[REMOTE_ACCESS_FIELD_VERIFICATION.md](REMOTE_ACCESS_FIELD_VERIFICATION.md)
with each run: the Mac payload version and SHA-256 of `latch`,
`latch-remote`, and `latchd`; the Desktop bundle version; the phone build's
executable and `LatchTransportFFI` hashes; the control-plane `release`
commit from `/health/ready`; and the relay deployment id. A result without
these identities is a recollection, not evidence.
