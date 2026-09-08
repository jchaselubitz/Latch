# Latch documentation

Latch keeps a terminal process alive independently of the window that happens
to display it. These guides describe the supported product surface; the other
files in this directory are design records, security reviews, and field notes.

Start here:

- [Getting started](GETTING_STARTED.md) — install Latch, create a first
  session, and make a terminal profile persistent.
- [CLI reference](CLI.md) — session lifecycle commands, JSON output, updates,
  and the local gateway.
- [Desktop](DESKTOP.md) — the macOS companion, terminal preferences, updates,
  and paired remote access.
- [Integrations](INTEGRATIONS.md) — create Latch sessions from another
  product, including the Overlord provider boundary.

More focused documentation:

- [iTerm setup](ITERM_SETUP.md) and [SSH setup](SSH_SETUP.md)
- [Remote access in Latch Desktop](REMOTE_ACCESS_DESKTOP.md) and the
  [remote-access threat model](REMOTE_ACCESS_THREAT_MODEL.md)
- [Secure relay replacement](PLAN_REMOTE_RELAY_REPLACEMENT.md) — implementation
  record and remaining resilience/deployment objectives for Overlord coo:952
- [Mobile session creation](FEATURE_MOBILE_SESSION_CREATION.md), its
  [implementation plan](PLAN_MOBILE_SESSION_CREATION.md), and the
  [field check](FIELD_CHECK_MOBILE_SESSION_CREATION.md) that is still to run
- [TypeScript gateway integration](REMOTE_SDK.md)
- [architecture rules](ARCHITECTURE_RULES.md) and
  [CLI release process](CLI_RELEASES.md)
