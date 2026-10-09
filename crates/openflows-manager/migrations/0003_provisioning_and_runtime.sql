-- WP-01 migration 0003: Provisioning and runtime domain.
--
-- Covers template_releases, template_release_roles, organization_template_versions,
-- organization_provisioners, tenants, runtime_identities, workspaces,
-- runtime_credentials, operations, and operation_steps from
-- docs/implementation/centralized-deployment/03-template-provisioning.md.
--
-- Tenants and their dependent records are scoped to an organization. Composite
-- constraints keep a tenant's repository, runtime identity, and workspaces from
-- being attached across organization boundaries.

-- Operator-approved template releases. `version` and `manifest_sha256` are
-- immutable release identity.
CREATE TABLE template_releases (
    id                UUID PRIMARY KEY,
    version           TEXT NOT NULL,
    manifest_sha256   TEXT NOT NULL,
    artifact_uri      TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'staged'
                      CHECK (status IN ('staged', 'approved', 'retired')),
    approved_by       UUID,
    approved_at       TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (version),
    UNIQUE (manifest_sha256)
);

-- Per-role release archives. Composite PK (release_id, role).
CREATE TABLE template_release_roles (
    release_id              UUID NOT NULL REFERENCES template_releases(id),
    role                    TEXT NOT NULL CHECK (role IN ('nexus', 'forge', 'sentinel', 'vessel', 'lore')),
    archive_digest          TEXT NOT NULL,
    parameter_schema_version INTEGER NOT NULL,
    image_digest            TEXT,
    PRIMARY KEY (release_id, role)
);

-- A template version published into a specific organization for a role.
-- UNIQUE(org, release, role) prevents duplicate publication per org.
CREATE TABLE organization_template_versions (
    id                UUID PRIMARY KEY,
    organization_id   UUID NOT NULL REFERENCES organizations(id),
    release_id        UUID NOT NULL REFERENCES template_releases(id),
    role              TEXT NOT NULL CHECK (role IN ('nexus', 'forge', 'sentinel', 'vessel', 'lore')),
    coder_template_id TEXT,
    coder_version_id  TEXT,
    import_job_id     TEXT,
    status            TEXT NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending', 'importing', 'ready', 'failed')),
    error_code        TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, release_id, role)
);

-- Organization-scoped Coder provisioners.
CREATE TABLE organization_provisioners (
    id                     UUID PRIMARY KEY,
    organization_id        UUID NOT NULL REFERENCES organizations(id),
    backend_resource_id    TEXT,
    coder_provisioner_ref  TEXT,
    secret_ref             TEXT,
    status                 TEXT NOT NULL DEFAULT 'provisioning'
                           CHECK (status IN ('provisioning', 'ready', 'unavailable')),
    last_heartbeat_at      TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenants: one repository automation environment per Openflows organization.
-- Unique live (org, repo) and (org, slug) constraints; a tenant is bound to a
-- repository through its connection within the same organization.
CREATE TABLE tenants (
    id                     UUID PRIMARY KEY,
    organization_id        UUID NOT NULL REFERENCES organizations(id),
    slug                   TEXT NOT NULL,
    connection_id          UUID NOT NULL REFERENCES github_connections(id),
    github_repository_id   BIGINT NOT NULL,
    pinned_release_id      UUID REFERENCES template_releases(id),
    fleet_size             INTEGER NOT NULL CHECK (fleet_size >= 1),
    desired_state          TEXT NOT NULL DEFAULT 'provisioning'
                           CHECK (desired_state IN ('provisioning', 'running', 'paused', 'deleting', 'deleted')),
    observed_state         TEXT NOT NULL DEFAULT 'provisioning'
                           CHECK (observed_state IN ('provisioning', 'running', 'paused', 'deleting', 'deleted', 'failed', 'access_blocked')),
    error_code             TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One live tenant per repository per organization.
    UNIQUE (organization_id, github_repository_id),
    -- One live tenant per slug per organization.
    UNIQUE (organization_id, slug),
    -- Composite uniqueness so dependents (e.g. workspaces) can reference a
    -- tenant within the same organization and be prevented from crossing org
    -- boundaries.
    UNIQUE (organization_id, id)
);

-- Runtime identity: the tenant's Coder machine owner. tenant_id is UNIQUE and
-- coder_owner_id is UNIQUE.
CREATE TABLE runtime_identities (
    id                     UUID PRIMARY KEY,
    tenant_id              UUID NOT NULL UNIQUE REFERENCES tenants(id),
    organization_id        UUID NOT NULL REFERENCES organizations(id),
    coder_owner_id         TEXT NOT NULL UNIQUE,
    status                 TEXT NOT NULL DEFAULT 'provisioning'
                           CHECK (status IN ('provisioning', 'ready', 'suspended', 'deleted')),
    credential_generation  BIGINT NOT NULL DEFAULT 0,
    secret_ref             TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Workspaces. Coder workspace ID is unique. A workspace references its tenant
-- within the same organization (composite org/tenant FK).
CREATE TABLE workspaces (
    id                      UUID PRIMARY KEY,
    organization_id         UUID NOT NULL REFERENCES organizations(id),
    tenant_id               UUID NOT NULL REFERENCES tenants(id),
    role                    TEXT NOT NULL CHECK (role IN ('nexus', 'forge', 'sentinel', 'vessel', 'lore')),
    slot                    INTEGER,
    ticket_id               TEXT,
    coder_workspace_id      TEXT UNIQUE,
    pinned_version_id       TEXT,
    desired_state           TEXT NOT NULL DEFAULT 'provisioning'
                            CHECK (desired_state IN ('provisioning', 'running', 'stopped', 'deleting', 'deleted')),
    observed_state          TEXT NOT NULL DEFAULT 'provisioning'
                            CHECK (observed_state IN ('provisioning', 'running', 'stopped', 'deleting', 'deleted', 'failed')),
    credential_generation   BIGINT NOT NULL DEFAULT 0,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A workspace must reference a tenant in the same organization. The
    -- composite FK guarantees organization scope at the database level.
    UNIQUE (organization_id, tenant_id, id),
    FOREIGN KEY (organization_id, tenant_id) REFERENCES tenants (organization_id, id)
);

-- Runtime credentials. Credential hash is unique; secret is returned only once.
CREATE TABLE runtime_credentials (
    id               UUID PRIMARY KEY,
    workspace_id     UUID NOT NULL REFERENCES workspaces(id),
    credential_hash  TEXT NOT NULL UNIQUE,
    audience         TEXT NOT NULL,
    expires_at       TIMESTAMPTZ NOT NULL,
    revoked_at       TIMESTAMPTZ,
    generation       BIGINT NOT NULL DEFAULT 0,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Durable operations with leasing. lease_owner + lease_expires_at drive
-- exclusive claiming by workers. state/current_step/attempts track progress.
CREATE TABLE operations (
    id                  UUID PRIMARY KEY,
    organization_id     UUID NOT NULL REFERENCES organizations(id),
    resource_type       TEXT NOT NULL,
    resource_id         UUID,
    kind                TEXT NOT NULL,
    idempotency_ref     TEXT,
    state               TEXT NOT NULL DEFAULT 'queued'
                        CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'retryable_failed')),
    current_step        TEXT,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    retry_at            TIMESTAMPTZ,
    lease_owner         TEXT,
    lease_expires_at    TIMESTAMPTZ,
    error_code          TEXT,
    sanitized_result    JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, id)
);

-- Individual steps of an operation. UNIQUE(operation, step) prevents duplicate
-- step execution.
CREATE TABLE operation_steps (
    operation_id    UUID NOT NULL REFERENCES operations(id),
    step_key        TEXT NOT NULL,
    request_digest  TEXT,
    upstream_id     TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    attempt         INTEGER NOT NULL DEFAULT 0,
    result          JSONB,
    PRIMARY KEY (operation_id, step_key)
);

CREATE INDEX idx_tenants_org ON tenants (organization_id);
CREATE INDEX idx_workspaces_tenant ON workspaces (tenant_id);
CREATE INDEX idx_operations_lease ON operations (lease_expires_at) WHERE state IN ('queued', 'running', 'retryable_failed');
CREATE INDEX idx_operations_org ON operations (organization_id);
CREATE INDEX idx_runtime_credentials_workspace ON runtime_credentials (workspace_id);
