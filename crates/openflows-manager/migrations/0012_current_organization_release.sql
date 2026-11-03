-- Preserve publication history and represent the current selection separately.
CREATE TABLE organization_current_releases (
    organization_id UUID PRIMARY KEY REFERENCES organizations(id),
    release_id UUID NOT NULL REFERENCES template_releases(id)
);
INSERT INTO organization_current_releases (organization_id, release_id)
SELECT DISTINCT ON (organization_id) organization_id, release_id
FROM organization_release_bindings
ORDER BY organization_id, created_at DESC, release_id DESC;
