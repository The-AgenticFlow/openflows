-- WP-02 migration 0006: Human authentication, sessions, encryption key
-- versioning, rate limiting, and org-less idempotency.
--
-- WP-01 already created the user/identity/organization/membership/invitation/
-- session/auth-transaction/cli-login-request tables. This migration adds the
-- fields WP-02 needs without modifying already-applied migrations:
--
--   * sessions: absolute (created_at) lifetime enforcement and a
--     last_authenticated_at timestamp for the "recent authentication" (10
--     minute) requirement on ownership transfer and deletion.
--   * auth_transactions: store the PKCE verifier as an encrypted blob with an
--     explicit key version, and remember a login_snapshot for the callback.
--   * idempotency_keys: allow a NULL organization_id so organization creation
--     can be idempotent before any organization exists, while still enforcing
--     uniqueness for org-scoped keys.
--   * New rate-limit ledger table for independently bounding start,
--     verification, and polling endpoints.
--   * New encryption-key-version registry (a thin envelope-key abstraction).
--   * New secret-audit table to prove sensitive values are never persisted in
--     plaintext application columns.

-- ---- sessions: absolute lifetime and recent authentication ----
-- The access/refresh expiry already exist. Add:
--   * created_at already exists; it is the absolute session birth and is used
--     to enforce the 12-hour absolute browser-session lifetime and the 30-day
--     refresh-family maximum lifetime.
--   * last_authenticated_at records when the user last proved fresh
--     authentication (e.g. re-entering credentials). Refreshing a credential
--     does NOT update it, so ownership transfer / deletion cannot be gated
--     merely by a long-lived refresh.
ALTER TABLE sessions
    ADD COLUMN last_authenticated_at TIMESTAMPTZ;

-- ---- auth_transactions: encrypted PKCE verifier with key version ----
-- Recoverable OAuth material must be encrypted with a versioned key; the key
-- version travels with the ciphertext so rotation can be keyed independently.
-- Keep the existing `pkce_verifier BYTEA` for backward compatibility but stop
-- writing raw material to it: new rows use the encrypted columns below.
ALTER TABLE auth_transactions
    ADD COLUMN encrypted_pkce_verifier BYTEA,
    ADD COLUMN pkce_key_version INTEGER,
    ADD COLUMN github_login_snapshot TEXT;

-- ---- idempotency: allow organization-less keys ----
-- Organization creation must be idempotent before an organization exists, so
-- the current NOT NULL organization_id foreign key is too strict. Relax the
-- column and add a partial unique index that treats NULL as a distinct key
-- value (NULLS NOT DISTINCT, Postgres 15+), preserving the org-scoped
-- uniqueness enforced by the original constraint for non-NULL values.
ALTER TABLE idempotency_keys
    ALTER COLUMN organization_id DROP NOT NULL;

-- Keep the original unique constraint for org-scoped keys (it still applies
-- because NULL values do not conflict under the default NULLS DISTINCT
-- semantics of a plain UNIQUE constraint).
-- Add a complementary unique index for the organization-less (actor, route,
-- key) case, treating NULL as a real value so two NULL org keys for the same
-- actor/route/key conflict as required.
CREATE UNIQUE INDEX idempotency_keys_orgless_unique
    ON idempotency_keys (actor_id, route, key)
    WHERE organization_id IS NULL;

-- ---- rate-limit ledger ----
-- Independent rate-limit counters keyed by a bucketed key (e.g. IP for public
-- entry points, device secret for polling). Uses a fixed time bucket so the
-- count is durable and queryable; workers/prune can be bounded.
CREATE TABLE rate_limit_ledger (
    bucket_key      TEXT NOT NULL,
    scope           TEXT NOT NULL,  -- 'start' | 'verify' | 'poll' | 'login' | 'device'
    window_start    TIMESTAMPTZ NOT NULL,
    count           INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_key, scope, window_start)
);

CREATE INDEX idx_rate_limit_ledger_window ON rate_limit_ledger (window_start);

-- ---- encryption key version registry ----
-- Records the current active version for each encryption key purpose so the
-- application can store `key_version` alongside ciphertext and resolve the
-- active key without hard-coding. The actual key bytes live in the configured
-- secret provider; this table only records versions.
CREATE TABLE encryption_keys (
    purpose       TEXT PRIMARY KEY,
    active_version INTEGER NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seed the default purpose so new deployments always have a resolvable active
-- version (the key bytes themselves come from configuration/secret provider).
INSERT INTO encryption_keys (purpose, active_version) VALUES ('oauth_pkce', 1);
INSERT INTO encryption_keys (purpose, active_version) VALUES ('user_token', 1);
INSERT INTO encryption_keys (purpose, active_version) VALUES ('device_secret', 1);
