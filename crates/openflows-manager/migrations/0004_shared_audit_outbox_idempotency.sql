-- WP-01 migration 0004: Shared audit, outbox, idempotency, and ownership FK.
--
-- Covers the shared persistence rules from README.md:
--   * audit_events with org, actor, action, resource, result, request id, time
--   * outbox_events with org, event_type, allowlisted payload, attempts, lease
--   * idempotency_keys with a unique (actor, org, route, key) constraint
-- plus the deferred organization-owner membership FK required by
-- 01-user-management.md so an organization and its creator's membership can be
-- created in the same transaction.

-- Append-only audit trail. Never stored credentials or raw upstream bodies.
CREATE TABLE audit_events (
    id                UUID PRIMARY KEY,
    organization_id   UUID REFERENCES organizations(id),
    actor_type        TEXT NOT NULL,
    actor_id          UUID,
    action            TEXT NOT NULL,
    resource_type     TEXT,
    resource_id       UUID,
    result            TEXT NOT NULL CHECK (result IN ('success', 'denied', 'failure')),
    request_id        TEXT,
    occurred_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Durable outbox for side effects, written in the same transaction as the
-- state change that triggers them. Allowlisted payload is validated by the
-- application before insert.
CREATE TABLE outbox_events (
    id                UUID PRIMARY KEY,
    organization_id   UUID REFERENCES organizations(id),
    event_type        TEXT NOT NULL,
    payload           JSONB,
    attempts          INTEGER NOT NULL DEFAULT 0,
    lease_owner       TEXT,
    lease_expires_at  TIMESTAMPTZ,
    delivered_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotency records. A unique constraint on (actor_id, organization_id, route,
-- key) makes retries idempotent and conflicting reuse detectable. The request
-- hash distinguishes a repeated identical body from a different body reusing
-- the same key. response_reference lets a retry return the original result.
CREATE TABLE idempotency_keys (
    id                  UUID PRIMARY KEY,
    actor_id            UUID NOT NULL,
    organization_id     UUID NOT NULL REFERENCES organizations(id),
    route               TEXT NOT NULL,
    key                 TEXT NOT NULL,
    request_hash        TEXT NOT NULL,
    response_reference  TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at          TIMESTAMPTZ NOT NULL,
    UNIQUE (actor_id, organization_id, route, key)
);

CREATE INDEX idx_audit_events_org ON audit_events (organization_id, occurred_at DESC);
CREATE INDEX idx_outbox_lease ON outbox_events (lease_expires_at) WHERE delivered_at IS NULL;
CREATE INDEX idx_idempotency_expiry ON idempotency_keys (expires_at);

-- Enforce that an organization's owner is an active member of that
-- organization. Deferred so the organization and its creator's membership can
-- be inserted in the same transaction (deferred constraints are checked at
-- commit time).
ALTER TABLE organizations
    ADD CONSTRAINT organizations_owner_is_member
    FOREIGN KEY (id, owner_user_id)
    REFERENCES memberships (organization_id, user_id)
    DEFERRABLE INITIALLY DEFERRED;
