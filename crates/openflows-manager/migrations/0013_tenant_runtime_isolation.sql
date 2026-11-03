-- Tenant runtime integration and isolation.
--
-- Extends the provisioning schema (tenants, workspaces, runtime_identities,
-- runtime_credentials, operations) with the tenant-runtime safeguards the
-- specification requires: per-organization quota ceilings (the minimal
-- infrastructure safeguards), tenant-scoped Redis credentials with immutable
-- namespaces, worker-capacity reservations, and tenant-scoped lifecycle-hook
-- signing keys. All changes are forward-only and additive; nothing here drops
-- or renames existing data.
--
-- Quotas are platform-configured ceilings stored per organization and populated
-- from the Manager's operator configuration at org creation. They are enforced
-- centrally and transactionally; there is no customer-facing endpoint to raise
-- them, so only a platform operator can configure higher limits.

-- ---------------------------------------------------------------------------
-- Per-organization tenant quota ceilings.
-- A row is created with the org so every org has an explicit, non-null ceiling
-- (missing ceilings must prevent hosted readiness, not silently allow any
-- number). `max_concurrent_provisioning_ops` gates how many tenant operations
-- may be actively executing for the org at once; further work is queued.
-- ---------------------------------------------------------------------------
CREATE TABLE tenant_quotas (
    organization_id               UUID PRIMARY KEY REFERENCES organizations(id),
    max_tenants                   INTEGER NOT NULL CHECK (max_tenants >= 1),
    max_workers_per_tenant        INTEGER NOT NULL CHECK (max_workers_per_tenant >= 1),
    max_concurrent_provisioning_ops INTEGER NOT NULL CHECK (max_concurrent_provisioning_ops >= 1),
    created_at                    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Tenant runtime reservations.
-- `reserved_worker_capacity` counts worker workspaces that are queued or
-- currently provisioning but not yet `running`, so concurrent tenant-creation
-- and worker requests reserve capacity consistently and release it on failure
-- or deletion. The Nexus controller occupies one fixed slot in addition to the
-- worker fleet; the per-org `max_workers_per_tenant` ceiling applies to worker
-- roles only.
-- ---------------------------------------------------------------------------
ALTER TABLE tenants
    ADD COLUMN reserved_worker_capacity INTEGER NOT NULL DEFAULT 0
        CHECK (reserved_worker_capacity >= 0);

-- Index active (non-terminal) tenant operations per org so the provisioning
-- slot gate (max one active tenant op per org) and startup recovery can find
-- work without scanning the whole table.
CREATE INDEX idx_tenant_operations_active
    ON operations (organization_id, kind, state)
    WHERE state IN ('queued', 'running', 'retryable_failed')
      AND kind IN ('tenant.provision', 'tenant.delete', 'tenant.clean');

-- ---------------------------------------------------------------------------
-- Tenant-scoped Redis credentials.
-- Each tenant has an immutable namespace (the tenant UUID) and an isolated
-- Redis credential. Only the credential hash is persisted; the raw password is
-- returned once through the secret provider and mounted into the workspace. The
-- service (not the runtime) enforces that a tenant's Redis client can only
-- reach its own immutable namespace.
-- ---------------------------------------------------------------------------
CREATE TABLE tenant_redis_credentials (
    id                UUID PRIMARY KEY,
    tenant_id         UUID NOT NULL UNIQUE REFERENCES tenants(id),
    organization_id   UUID NOT NULL REFERENCES organizations(id),
    username          TEXT NOT NULL UNIQUE,
    namespace         TEXT NOT NULL,
    credential_hash   TEXT NOT NULL,
    secret_ref        TEXT NOT NULL,
    generation        BIGINT NOT NULL DEFAULT 0,
    revoked_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, tenant_id, id),
    FOREIGN KEY (organization_id, tenant_id) REFERENCES tenants (organization_id, id)
);

-- ---------------------------------------------------------------------------
-- Tenant-scoped lifecycle-hook signing keys.
-- Coder hook verification moves to the Manager ingress; the global hook
-- signing secret is never distributed to customer runtimes. Each tenant has its
-- own hook signing key, stored only as a hash (verification uses the raw value
-- transiently, never persisted). Revolving the generation invalidates the
-- tenant's relay authentication without a workspace rebuild.
-- ---------------------------------------------------------------------------
CREATE TABLE tenant_hook_keys (
    id                UUID PRIMARY KEY,
    tenant_id         UUID NOT NULL UNIQUE REFERENCES tenants(id),
    organization_id   UUID NOT NULL REFERENCES organizations(id),
    key_hash          TEXT NOT NULL,
    generation        BIGINT NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at        TIMESTAMPTZ,
    UNIQUE (organization_id, tenant_id, id),
    FOREIGN KEY (organization_id, tenant_id) REFERENCES tenants (organization_id, id)
);
