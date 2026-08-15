-- QR pairing requests.
--
-- A host device registers a pairing it is displaying on an unlocked Mac. The
-- service stores only a domain-separated digest of the one-time secret, never
-- the secret itself, and the row is single-use: it is consumed on the first
-- successful confirmation and expires five minutes after it was created.

CREATE TABLE pairing_requests (
    pairing_id     TEXT PRIMARY KEY,
    account_id     TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    host_device_id TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    secret_digest  TEXT        NOT NULL,
    -- The human-readable confirmation words shown on both screens. They are
    -- not a credential and prove nothing on their own.
    phrase         TEXT,
    permission     TEXT        NOT NULL CHECK (permission IN ('observe', 'interact', 'control')),
    expires_at     BIGINT      NOT NULL,
    consumed_at    TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX pairing_requests_host_idx ON pairing_requests (host_device_id);
CREATE INDEX pairing_requests_expires_at_idx ON pairing_requests (expires_at);
