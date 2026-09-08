-- Remote Link v1 admission and durable revocation state. The control plane may
-- hold endpoint identity metadata, but relay claims contain only opaque rooms.

CREATE TABLE owner_invitations (
    id TEXT PRIMARY KEY,
    secret_digest TEXT NOT NULL,
    expires_at BIGINT NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE remote_enrollments (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    host_device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    room_id TEXT NOT NULL UNIQUE,
    admission_digest TEXT NOT NULL,
    expires_at BIGINT NOT NULL,
    provisional_device_id TEXT,
    provisional_name TEXT,
    provisional_platform TEXT,
    provisional_public_key TEXT,
    provisional_token_digest TEXT,
    completed_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE remote_links (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    host_device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    client_device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    room_id TEXT NOT NULL UNIQUE,
    grant_revision BIGINT NOT NULL DEFAULT 1,
    host_generation BIGINT NOT NULL DEFAULT 0,
    controller_generation BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(host_device_id, client_device_id)
);

CREATE TABLE remote_admissions (
    id TEXT PRIMARY KEY,
    link_id TEXT REFERENCES remote_links(id) ON DELETE CASCADE,
    enrollment_id TEXT REFERENCES remote_enrollments(id) ON DELETE CASCADE,
    room_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('host', 'controller')),
    purpose TEXT NOT NULL CHECK(purpose IN ('enrollment', 'session')),
    generation BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    attempt_id TEXT,
    lease_id TEXT UNIQUE,
    lease_expires_at BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (
        (purpose = 'session' AND link_id IS NOT NULL AND enrollment_id IS NULL)
        OR
        (purpose = 'enrollment' AND link_id IS NULL AND enrollment_id IS NOT NULL)
    )
);
CREATE INDEX remote_admissions_expiry_idx ON remote_admissions(expires_at);

CREATE TABLE relay_revocation_outbox (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL,
    not_after BIGINT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    acknowledged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX relay_revocation_pending_idx ON relay_revocation_outbox(created_at)
    WHERE acknowledged_at IS NULL;
