-- WP-05 migration 0011: Coder adapter, template releases, and org readiness.
--
-- Extends the WP-01 provisioning schema (template_releases,
-- template_release_roles, organization_template_versions,
-- organization_provisioners) with the release policy and organization
-- readiness state the Coder adapter needs. All changes are forward-only and
-- additive; nothing here drops or renames existing data.

-- ---------------------------------------------------------------------------
-- Platform operators: the restricted set of identities authorized to approve,
-- promote, or retire platform template releases. An ordinary customer
-- organization admin is never a platform operator; membership in this table is
-- provisioned by the platform operator token, not by any customer role.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_operators (
    user_id        UUID PRIMARY KEY REFERENCES users(id),
    created_by     UUID,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Organization readiness reconciliation state. `status` on organizations
-- already transitions provisioning -> ready. These columns capture the
-- per-resource readiness facts a worker reconciles so a not-ready organization
-- can report *why* without leaking upstream details, and so re-running the
-- reconciliation cannot silently re-adopt stale resources.
-- ---------------------------------------------------------------------------
ALTER TABLE organizations
    ADD COLUMN provisioner_status TEXT
        CHECK (provisioner_status IN ('provisioning', 'ready', 'unavailable')),
    ADD COLUMN model_capability_ok BOOLEAN,
    ADD COLUMN last_readiness_error TEXT;

ALTER TABLE template_releases
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- ---------------------------------------------------------------------------
-- Release publication policy. template_releases.status already models
-- staged/approved/retired. Add a per-organization publication record for the
-- *approved* release an organization is reconciled against, so promotion never
-- silently upgrades existing pins and the worker always re-publishes the exact
-- approved release by digest.
-- ---------------------------------------------------------------------------
CREATE TABLE organization_release_bindings (
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    release_id       UUID NOT NULL REFERENCES template_releases(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, release_id)
);

-- Index active (non-terminal) operations so the org-provision worker and
-- startup recovery can find claimable work without scanning the whole table.
CREATE INDEX idx_operations_kind_state
    ON operations (kind, state)
    WHERE state IN ('queued', 'running', 'retryable_failed');
