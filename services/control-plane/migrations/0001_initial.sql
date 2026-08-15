-- Latch control plane, initial schema.
--
-- Every column here is account membership, opaque device identity, a public
-- key, revocation state, or short-lived connectivity metadata. There is
-- deliberately no column that can hold terminal bytes, a transcript, a session
-- name, a prompt answer, or a Latch gateway token.

CREATE TABLE accounts (
    id            TEXT PRIMARY KEY,
    label         TEXT        NOT NULL,
    token_digest  TEXT        NOT NULL,
    relay_enabled BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE devices (
    id             TEXT PRIMARY KEY,
    account_id     TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name           TEXT        NOT NULL,
    platform       TEXT        NOT NULL,
    role           TEXT        NOT NULL CHECK (role IN ('host', 'client')),
    -- Hex-encoded Noise static public key pinned during local pairing.
    public_key     TEXT        NOT NULL,
    key_generation INTEGER     NOT NULL DEFAULT 1,
    token_digest   TEXT        NOT NULL,
    revoked_at     TIMESTAMPTZ,
    last_seen_at   TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX devices_account_id_idx ON devices (account_id);

-- A pairing mirrors a grant the host device already approved locally. The
-- control plane can never manufacture one on its own.
CREATE TABLE pairings (
    account_id       TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    host_device_id   TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    client_device_id TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    permission       TEXT        NOT NULL CHECK (permission IN ('observe', 'interact', 'control')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at       TIMESTAMPTZ,
    PRIMARY KEY (host_device_id, client_device_id),
    CHECK (host_device_id <> client_device_id)
);

CREATE INDEX pairings_client_device_id_idx ON pairings (client_device_id);

-- Presence is short lived by construction: rows carry an absolute expiry and
-- are purged by the sweeper as well as filtered on read.
CREATE TABLE presence (
    device_id  TEXT PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
    account_id TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    candidates JSONB       NOT NULL,
    expires_at BIGINT      NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX presence_expires_at_idx ON presence (expires_at);

CREATE TABLE rendezvous_offers (
    id                  TEXT PRIMARY KEY,
    account_id          TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    requester_device_id TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    target_device_id    TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    request_id          TEXT        NOT NULL,
    candidates          JSONB       NOT NULL,
    expires_at          BIGINT      NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX rendezvous_offers_target_idx ON rendezvous_offers (target_device_id, expires_at);

-- Only the digest of the relay authentication secret is retained, so a
-- database leak cannot admit an endpoint to a live relay slot.
CREATE TABLE relay_tickets (
    relay_id            TEXT PRIMARY KEY,
    account_id          TEXT        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    host_device_id      TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    client_device_id    TEXT        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    secret_digest       TEXT        NOT NULL,
    expires_at          BIGINT      NOT NULL,
    admitted_device_ids JSONB       NOT NULL DEFAULT '[]'::JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX relay_tickets_expires_at_idx ON relay_tickets (expires_at);

-- Coarse audit trail: timestamp, action, opaque ids, and a result. No
-- addresses, names, keys, or payloads.
CREATE TABLE access_events (
    id         BIGSERIAL PRIMARY KEY,
    account_id TEXT,
    device_id  TEXT,
    action     TEXT        NOT NULL,
    result     TEXT        NOT NULL CHECK (result IN ('allowed', 'denied')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX access_events_account_idx ON access_events (account_id, id DESC);
