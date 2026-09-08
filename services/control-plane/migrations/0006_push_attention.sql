-- APNs registration and attention-event deduplication. A registration holds
-- only the opaque push token; an attention event holds opaque ids and a time.
-- Neither can carry a session id, name, prompt, path, output, or approval
-- content: the notification payload is a fixed generic sentence.

CREATE TABLE push_registrations (
    device_id TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    push_token TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE attention_events (
    host_device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    client_device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (host_device_id, event_id)
);
CREATE INDEX attention_events_created_idx ON attention_events(created_at);
