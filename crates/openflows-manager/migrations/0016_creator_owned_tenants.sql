-- Personal Coder accounts can own several tenants. Legacy tenants deliberately
-- remain unassigned until an operator performs an explicit ownership migration.
CREATE TABLE coder_user_links (
    user_id UUID PRIMARY KEY REFERENCES users(id),
    coder_url TEXT NOT NULL,
    coder_user_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (coder_url, coder_user_id)
);
CREATE TABLE coder_link_attempts (
    state_hash TEXT PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    browser_hash TEXT NOT NULL,
    verifier TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
ALTER TABLE tenants ADD COLUMN owner_user_id UUID REFERENCES users(id);
ALTER TABLE runtime_identities DROP CONSTRAINT runtime_identities_coder_owner_id_key;
