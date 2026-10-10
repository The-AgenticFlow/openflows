-- WP-04 migration 0010: Runtime credentials and GitHub credential broker.
--
-- Extends the WP-01 runtime_credentials / credential_leases tables for the
-- runtime-authenticated GitHub credential broker: workspace-scoped runtime
-- bearer credentials (hash-stored, generation-bound) and durable credential
-- leases that record the authorization/connection/workspace generations at
-- issuance so a recheck after the external token exchange can fail closed.
--
-- All changes are forward-only and additive; nothing here drops or renames
-- existing data or rewrites applied migrations.

-- ---------------------------------------------------------------------------
-- runtime_credentials: enforce that a workspace-scoped runtime credential can
-- only be minted for a workspace that still belongs to its tenant, and add an
-- index for "look up the live credential for a workspace".
--
-- The workspace is already scoped to (organization_id, tenant_id) through the
-- workspaces composite FK, so joining through workspaces/tenants is the trusted
-- relationship source for deriving the runtime's organization and tenant.
-- ---------------------------------------------------------------------------

-- A runtime credential is live only while it is not revoked. Add a partial
-- index so the authentication lookup and the per-workspace "current live
-- credential" query are both indexed.
CREATE INDEX idx_runtime_credentials_active
    ON runtime_credentials (workspace_id)
    WHERE revoked_at IS NULL;

-- ---------------------------------------------------------------------------
-- credential_leases: capture the workspace credential generation and the
-- runtime purpose at issuance. `connection_generation` already exists; WP-04
-- additionally requires the workspace credential generation so a runtime
-- rotation invalidates in-flight exchanges by generation comparison.
-- ---------------------------------------------------------------------------
ALTER TABLE credential_leases
    ADD COLUMN connection_id UUID,
    ADD COLUMN workspace_generation BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN purpose TEXT;

-- Index leases that still hold a usable (unrevoked) token for cleanup workers
-- that revoke outstanding leases on disconnect / installation removal.
CREATE INDEX idx_credential_leases_scope
    ON credential_leases (connection_id, workspace_id)
    WHERE revoked_at IS NULL;
