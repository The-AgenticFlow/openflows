-- Keyset paging for the organization operation history
-- (`GET /organizations/{org}/operations`) orders by (created_at DESC, id DESC)
-- within one organization. This index lets Postgres read just the requested
-- page instead of sorting the organization's whole history, which grows with
-- every worker action.
CREATE INDEX IF NOT EXISTS operations_org_history_idx
    ON operations (organization_id, created_at DESC, id DESC);
