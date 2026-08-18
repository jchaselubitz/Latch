# coo:765 — Latch widget "request entity too large"

The duplicated error on the Overlord mission Latch card is presentation-only.
The Latch session, inspect/open/stop, and protocol attach/update/deliver keep
working. The widget POSTs a collected `latch events` snapshot to Overlord's
harness-event ingest; Express's 100 KiB JSON default rejects a streaming dump,
and the generic 500 handler repeats the body-parser message.

Fix lives in the Overlord sibling checkout (`ai/history/2026-08-18-coo-765-latch-widget-entity-too-large.md`):
raise the ingest body limit, batch the POST, and map 413 without duplicating
the message. No Latch CLI or event-contract change.
