-- Preserve the original migrations for databases already initialized by WP-01.
-- Each claim needs its own identity, even when a worker name is reused.
ALTER TABLE operations ADD COLUMN lease_token UUID;
ALTER TABLE outbox_events ADD COLUMN lease_token UUID;

ALTER TABLE github_repositories
    ADD CONSTRAINT github_repositories_scoped_connection
    FOREIGN KEY (organization_id, connection_id)
    REFERENCES github_connections (organization_id, id);
ALTER TABLE tenants
    ADD CONSTRAINT tenants_scoped_repository
    FOREIGN KEY (organization_id, connection_id, github_repository_id)
    REFERENCES github_repositories (organization_id, connection_id, github_repository_id);
ALTER TABLE runtime_identities
    ADD CONSTRAINT runtime_identities_scoped_tenant
    FOREIGN KEY (organization_id, tenant_id)
    REFERENCES tenants (organization_id, id);

-- Keep names reserved throughout deletion; release them only when both
-- desired and observed state confirm that deletion has completed.
ALTER TABLE tenants DROP CONSTRAINT tenants_organization_id_github_repository_id_key;
ALTER TABLE tenants DROP CONSTRAINT tenants_organization_id_slug_key;
CREATE UNIQUE INDEX tenants_organization_id_github_repository_id_key
    ON tenants (organization_id, github_repository_id)
    WHERE desired_state <> 'deleted' OR observed_state <> 'deleted';
CREATE UNIQUE INDEX tenants_organization_id_slug_key
    ON tenants (organization_id, slug)
    WHERE desired_state <> 'deleted' OR observed_state <> 'deleted';

-- NULL represents a singleton slot and must compare equal to another NULL.
CREATE UNIQUE INDEX workspaces_live_slot
    ON workspaces (tenant_id, role, slot) NULLS NOT DISTINCT
    WHERE desired_state <> 'deleted' OR observed_state <> 'deleted';
