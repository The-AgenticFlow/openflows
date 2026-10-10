-- WP-03 migration 0008: GitHub App connection lifecycle.
--
-- Extends the WP-01 github_connections / github_connect_attempts /
-- webhook_deliveries tables for the full connection lifecycle: two-flow
-- connection attempts (user OAuth + installation setup), explicit webhook
-- processing state, and conservative access-revocation tracking so a stale
-- "added"/"unsuspended" event can never reactivate access by itself.
--
-- All changes are forward-only and additive; nothing here drops or renames
-- existing data.

-- ---------------------------------------------------------------------------
-- github_connect_attempts: two-flow model (OAuth + installation setup).
--
-- An attempt tracks up to two high-entropy states: an OAuth redirect state and
-- an installation-setup redirect state, each bound to the same server-side
-- attempt. Either may arrive first. The connection is only activated once both
-- proofs are present (OAuth identity proof + installation proof).
-- ---------------------------------------------------------------------------
ALTER TABLE github_connect_attempts
    ADD COLUMN flow_type TEXT NOT NULL DEFAULT 'both'
        CHECK (flow_type IN ('user_oauth', 'installation_setup', 'both')),
    ADD COLUMN oauth_state_hash TEXT,
    ADD COLUMN setup_state_hash TEXT,
    -- Encrypted PKCE verifier used to exchange the OAuth authorization code.
    ADD COLUMN oauth_code_verifier_ref BYTEA,
    -- The authenticated GitHub user id captured by the OAuth callback proof.
    ADD COLUMN oauth_github_user_id BIGINT,
    ADD COLUMN oauth_consumed_at TIMESTAMPTZ,
    ADD COLUMN setup_consumed_at TIMESTAMPTZ;

-- Only a hash of each state value is stored; each is unique so a callback can
-- resolve exactly one attempt, and replay of a consumed state is rejected.
CREATE UNIQUE INDEX github_connect_attempts_oauth_state_hash_key
    ON github_connect_attempts (oauth_state_hash)
    WHERE oauth_state_hash IS NOT NULL;

CREATE UNIQUE INDEX github_connect_attempts_setup_state_hash_key
    ON github_connect_attempts (setup_state_hash)
    WHERE setup_state_hash IS NOT NULL;

-- ---------------------------------------------------------------------------
-- github_connections: record why access was revoked so reconciliation can
-- require fresh authoritative state before reactivating. A connection whose
-- access was removed must not silently return to 'active' from a stale event.
-- ---------------------------------------------------------------------------
ALTER TABLE github_connections
    ADD COLUMN access_revoked_reason TEXT;

-- ---------------------------------------------------------------------------
-- webhook_deliveries: explicit, durable processing state and a reconciliation
-- index. `processing_state` tracks the durable pipeline; `processed_at`
-- timestamps completion. Unknown installations carry a NULL organization and
-- are recorded for reconciliation but never auto-bound.
-- ---------------------------------------------------------------------------
ALTER TABLE webhook_deliveries
    ADD COLUMN processing_state TEXT NOT NULL DEFAULT 'received'
        CHECK (processing_state IN ('received', 'processing', 'processed', 'failed')),
    ADD COLUMN digest_sha256 TEXT,
    ADD COLUMN reconciled_at TIMESTAMPTZ;

CREATE INDEX idx_webhook_deliveries_pending
    ON webhook_deliveries (received_at)
    WHERE processing_state <> 'processed';

CREATE INDEX idx_webhook_deliveries_installation
    ON webhook_deliveries (installation_id)
    WHERE installation_id IS NOT NULL;
