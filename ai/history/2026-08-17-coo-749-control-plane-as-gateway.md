# coo:749.b8ff — Mobile `/v1/sessions` 404

## Cause

Settings was linked to `https://latch-production-….up.railway.app` (the
control plane) as if it were `latch serve`. Pairing with Jacob's MacBook Pro
had succeeded, but a saved manual gateway link wins over the paired route.

`GET /v1/capabilities` 404s on the control plane with
`{error: "not_found", reason: "no such resource"}`. Discovery treated any 404
as the pre-discovery gateway (sessions + terminal on). `GET /v1/sessions`
then 404s on a service that has no session API.

## Latch changes

- Treat that unmatched-route body as "not a gateway", not as legacy `latch serve`.
- Forget a saved control-plane URL so restore cannot keep blocking pairing.
- If a paired route already exists, fall back to it after rejecting the URL.
- Settings copy: do not paste the Mac Remote Access control-plane URL into
  the `latch serve` field.

## What to do on the phone

Unlink the Railway address if this build is not installed yet. Pairing is
enough; that URL is only for enrolling a phone, not for listing sessions.
