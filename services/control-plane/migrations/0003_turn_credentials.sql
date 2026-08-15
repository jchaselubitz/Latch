-- Cloudflare TURN usernames are revocation handles, not passwords. Passwords
-- returned to devices are never persisted by the control plane.
CREATE TABLE turn_credentials (
    username   TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    device_id  TEXT NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    expires_at BIGINT NOT NULL
);

CREATE INDEX turn_credentials_device_idx ON turn_credentials (device_id, expires_at);
CREATE INDEX turn_credentials_account_idx ON turn_credentials (account_id, expires_at);
