-- Workspace bootstrap, rotating runtime sessions, and durable hook delivery.
--
-- Bootstrap values and refresh tokens are hash-only in PostgreSQL. The raw
-- bootstrap value is written through the configured secret-injection backend
-- and is atomically consumed by POST /runtime/register.

ALTER TABLE workspaces
    ADD COLUMN runtime_secret_ref TEXT,
    ADD COLUMN branch TEXT,
    ADD COLUMN registered_at TIMESTAMPTZ,
    ADD COLUMN last_heartbeat_at TIMESTAMPTZ;

CREATE UNIQUE INDEX workspaces_one_live_role_slot
    ON workspaces (tenant_id, role, COALESCE(slot, -1))
    WHERE desired_state <> 'deleted';

CREATE TABLE workspace_bootstrap_credentials (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    credential_hash TEXT NOT NULL UNIQUE,
    secret_ref      TEXT NOT NULL UNIQUE,
    expires_at      TIMESTAMPTZ NOT NULL,
    consumed_at     TIMESTAMPTZ,
    revoked_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX workspace_bootstrap_one_live
    ON workspace_bootstrap_credentials (workspace_id)
    WHERE consumed_at IS NULL AND revoked_at IS NULL;

ALTER TABLE runtime_credentials
    ADD COLUMN family_id UUID,
    ADD COLUMN family_expires_at TIMESTAMPTZ;

UPDATE runtime_credentials
   SET family_id = id,
       family_expires_at = expires_at
 WHERE family_id IS NULL;

ALTER TABLE runtime_credentials
    ALTER COLUMN family_id SET NOT NULL,
    ALTER COLUMN family_expires_at SET NOT NULL;

ALTER TABLE tenant_hook_keys
    ADD COLUMN secret_ref TEXT,
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE tenant_hook_deliveries (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL REFERENCES tenants(id),
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    workspace_id     UUID NOT NULL REFERENCES workspaces(id),
    delivery_id      TEXT NOT NULL,
    payload          JSONB NOT NULL,
    deadline_at      TIMESTAMPTZ NOT NULL,
    state            TEXT NOT NULL DEFAULT 'pending'
                     CHECK (state IN ('pending', 'delivered', 'expired')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at     TIMESTAMPTZ,
    UNIQUE (organization_id, delivery_id),
    FOREIGN KEY (organization_id, tenant_id) REFERENCES tenants (organization_id, id),
    FOREIGN KEY (organization_id, tenant_id, workspace_id)
        REFERENCES workspaces (organization_id, tenant_id, id)
);

CREATE INDEX tenant_hook_deliveries_pending
    ON tenant_hook_deliveries (workspace_id, created_at)
    WHERE state = 'pending';

CREATE UNIQUE INDEX operations_runtime_idempotency
    ON operations (organization_id, kind, idempotency_ref)
    WHERE idempotency_ref IS NOT NULL
      AND kind IN ('tenant.workspace', 'workspace.stop', 'workspace.delete');
