# Latch opaque relay

This is a separate deployable from `services/control-plane`. It admits two
outbound WebSocket connections into one opaque room and forwards bounded
binary Noise records unchanged. It has no database and receives no account or
device identifiers, public/private identity keys, grants, gateway tokens,
paths, session names, or application plaintext.

Production runs plain WebSocket behind the hosting TLS edge on `PORT`. Set
`TLS_CERT_PATH` and `TLS_KEY_PATH` only for direct-TLS integration or a host
without edge termination. Endpoint clients use platform certificate
validation and put admissions only in the upgrade `Authorization` header.

Required secrets/configuration: `ADMISSION_ISSUER`, `ADMISSION_KEY_ID`,
`ADMISSION_PUBLIC_KEY_PEM`, `CONTROL_PLANE_URL`, `RELAY_SERVICE_TOKEN`, and
`RELAY_INVALIDATION_SECRET`. During an admission-key rotation, set
`ADMISSION_PREVIOUS_KEY_ID` and `ADMISSION_PREVIOUS_PUBLIC_KEY_PEM` together
for the bounded overlap and remove them afterwards; the relay refuses to
start with only one of the pair. Configure exactly one replica and disable
scale-to-zero/app sleeping. Verify upgrade Authorization passthrough and that
the edge idle timeout exceeds the 15-second heartbeat before staging review.

Production runs as the Railway service `latch-relay` (project `latch`, root
`services/relay`, `railway.json` pins one replica and the readiness check) at
`wss://latch-relay-production.up.railway.app/v1/connect`. Deployment,
verification, rotation, incident, and rollback procedures are in
`docs/REMOTE_LINK_OPERATIONS.md`.

The relay closes both peers on role replacement, peer loss, lease expiry,
invalid control traffic, or the 8 MiB backpressure bound. `/health/ready`
refuses admission while draining; neither health endpoint enumerates rooms.
