-- Each tenant gets a second Redis user for its controller (the Nexus
-- workspace): the tenant user's access plus the controller-only
-- `ctl:{namespace}:*` keys, which hold operator decisions (such as docs
-- approvals) that worker workspaces must not be able to rewrite. Its
-- username is derived (`{username}-ctl`); only the secret reference is
-- stored. NULL until the Manager next ensures the tenant's credentials.
ALTER TABLE tenant_redis_credentials
    ADD COLUMN IF NOT EXISTS controller_secret_ref TEXT;
