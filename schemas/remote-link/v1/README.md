# Remote Link v1

This directory is the canonical endpoint/relay/helper contract for Latch's
encrypted remote link. The relay admission claim is deliberately opaque: it
contains a random room, role, purpose, generation, expiry, and resource limits,
but no account/device identity, grant, gateway address, or application data.

Noise prologues are canonical binary values defined by the Rust transport
core, not JSON. Enrollment additionally mixes the 32-byte QR-only enrollment
secret into that prologue. It is never submitted to the control plane or relay.

Run `scripts/check-remote-link-contract.sh` to validate the schema fixtures and
ensure the copies embedded in the Apple client remain current.
