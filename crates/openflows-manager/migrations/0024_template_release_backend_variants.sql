-- Stage 1 of #504: backend-specific template release variants and pinning.
--
-- Extends template_release_roles and organization_template_versions with
-- `backend` (defaulting existing rows to 'docker') and adds backend/release
-- metadata (architecture, image_repository, workspace_runtime_release) plus
-- tenant/workspace backend and release pinning columns.

ALTER TABLE template_release_roles
    ADD COLUMN IF NOT EXISTS backend TEXT NOT NULL DEFAULT 'docker'
        CHECK (backend IN ('docker', 'kubernetes')),
    ADD COLUMN IF NOT EXISTS architecture TEXT NOT NULL DEFAULT 'amd64'
        CHECK (architecture IN ('amd64', 'arm64')),
    ADD COLUMN IF NOT EXISTS image_repository TEXT,
    ADD COLUMN IF NOT EXISTS workspace_runtime_release TEXT;

ALTER TABLE template_release_roles
    DROP CONSTRAINT IF EXISTS template_release_roles_pkey;

ALTER TABLE template_release_roles
    ADD PRIMARY KEY (release_id, backend, role);

ALTER TABLE organization_template_versions
    ADD COLUMN IF NOT EXISTS backend TEXT NOT NULL DEFAULT 'docker'
        CHECK (backend IN ('docker', 'kubernetes'));

-- Drop the unnamed UNIQUE(organization_id, release_id, role) constraint
-- safely regardless of PostgreSQL's 63-byte identifier truncation.
DO $$
DECLARE
    constraint_name TEXT;
BEGIN
    SELECT c.conname INTO constraint_name
    FROM pg_constraint c
    JOIN pg_class t ON t.oid = c.conrelid
    WHERE t.relname = 'organization_template_versions'
      AND c.contype = 'u'
      AND (
          SELECT array_agg(a.attname::text ORDER BY k.ord)
          FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord)
          JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
      ) = ARRAY['organization_id', 'release_id', 'role'];

    IF constraint_name IS NOT NULL THEN
        EXECUTE format('ALTER TABLE organization_template_versions DROP CONSTRAINT %I', constraint_name);
    END IF;
END $$;

ALTER TABLE organization_template_versions
    ADD CONSTRAINT organization_template_versions_org_release_backend_role_key
        UNIQUE (organization_id, release_id, backend, role);

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS pinned_backend TEXT
        CHECK (pinned_backend IN ('docker', 'kubernetes'));

ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS pinned_backend TEXT
        CHECK (pinned_backend IN ('docker', 'kubernetes')),
    ADD COLUMN IF NOT EXISTS pinned_release_id UUID REFERENCES template_releases(id);

-- All workspaces and already-pinned/provisioned tenants created prior to
-- backend-specific release support belong to Docker deployments.
UPDATE workspaces
   SET pinned_backend = 'docker'
 WHERE pinned_backend IS NULL;

UPDATE tenants
   SET pinned_backend = 'docker'
 WHERE pinned_backend IS NULL
   AND (
       pinned_release_id IS NOT NULL
       OR EXISTS (SELECT 1 FROM workspaces w WHERE w.tenant_id = tenants.id)
   );
