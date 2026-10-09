-- WP-01 migration 0002: GitHub integration domain.
--
-- Covers github_connections, github_repositories, github_connect_attempts,
-- webhook_deliveries, and credential_leases from
-- docs/implementation/centralized-deployment/02-github-app.md.
--
-- GitHub numeric IDs are stored as BIGINT (signed 64-bit). Opaque upstream
-- Coder IDs are TEXT. Every business table is scoped to an organization via a
-- composite (org, ...) unique constraint or a direct FK so a resource ID alone
-- can never authorize cross-organization access.

-- One GitHub App installation bound to an Openflows organization. A connection
-- cannot move between organizations through a callback or upsert.
CREATE TABLE github_connections (
    id                  UUID PRIMARY KEY,
    organization_id     UUID NOT NULL REFERENCES organizations(id),
    app_id              BIGINT NOT NULL,
    installation_id     BIGINT NOT NULL,
    github_account_id   BIGINT NOT NULL,
    account_type        TEXT NOT NULL CHECK (account_type IN ('User', 'Organization')),
    account_login       TEXT,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending', 'active', 'suspended', 'disconnected', 'deleted')),
    access_generation   BIGINT NOT NULL DEFAULT 0,
    connected_by        UUID REFERENCES users(id),
    verified_at         TIMESTAMPTZ,
    last_reconciled_at  TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- An installation belongs to exactly one organization.
    UNIQUE (app_id, installation_id),
    -- Unique per-organization connection id (composite org/id uniqueness).
    UNIQUE (organization_id, id)
);

-- Repositories exposed by a connected installation. Identity is the immutable
-- GitHub repository id. Composite FK (org, connection) prevents a repository
-- from being attached across organization boundaries.
CREATE TABLE github_repositories (
    organization_id       UUID NOT NULL REFERENCES organizations(id),
    connection_id         UUID NOT NULL REFERENCES github_connections(id),
    github_repository_id  BIGINT NOT NULL,
    full_name             TEXT NOT NULL,
    accessible            BOOLEAN NOT NULL DEFAULT true,
    last_seen_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (connection_id, github_repository_id),
    UNIQUE (organization_id, connection_id, github_repository_id)
);

-- Transient GitHub connect attempts with flow-specific state. Encrypted
-- user-credential reference lives outside plaintext application tables.
CREATE TABLE github_connect_attempts (
    id                      UUID PRIMARY KEY,
    organization_id         UUID NOT NULL REFERENCES organizations(id),
    initiated_by            UUID NOT NULL REFERENCES users(id),
    state_hash              TEXT NOT NULL,
    user_credential_ref     TEXT,
    candidate_installation  BIGINT,
    status                  TEXT NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending', 'verified', 'consumed', 'expired')),
    expires_at              TIMESTAMPTZ NOT NULL,
    consumed_at             TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (state_hash)
);

-- Verified webhook deliveries, deduplicated by delivery_id with a digest check.
CREATE TABLE webhook_deliveries (
    delivery_id       UUID PRIMARY KEY,
    organization_id   UUID REFERENCES organizations(id),
    event_type        TEXT NOT NULL,
    action            TEXT,
    installation_id   BIGINT,
    payload_digest    TEXT NOT NULL,
    payload           BYTEA,
    received_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at      TIMESTAMPTZ,
    attempts          INTEGER NOT NULL DEFAULT 0,
    lease_owner       TEXT,
    lease_expires_at  TIMESTAMPTZ
);

-- Credential leases record token fingerprints and encrypted references. The
-- actual token value is never stored as an ordinary application table value.
CREATE TABLE credential_leases (
    id                   UUID PRIMARY KEY,
    organization_id      UUID REFERENCES organizations(id),
    tenant_id            UUID,
    workspace_id         UUID,
    installation_id      BIGINT,
    repository_id        BIGINT,
    permission_profile_hash TEXT NOT NULL,
    connection_generation  BIGINT NOT NULL DEFAULT 0,
    token_fingerprint    TEXT NOT NULL,
    encrypted_ref        TEXT,
    expires_at           TIMESTAMPTZ NOT NULL,
    revoked_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_github_connections_org ON github_connections (organization_id);
CREATE INDEX idx_github_repositories_org ON github_repositories (organization_id);
CREATE INDEX idx_webhook_deliveries_lease ON webhook_deliveries (lease_expires_at) WHERE processed_at IS NULL;
CREATE INDEX idx_credential_leases_expiry ON credential_leases (expires_at);
