//! Persistence for runtime credentials, credential leases, and the trusted
//! scope resolution used by the GitHub credential broker.
//!
//! Runtime scope is never caller-supplied: it is derived from the authenticated
//! workspace's trusted database relationships (workspace -> tenant ->
//! organization -> connection -> repository). Every query joins to these
//! relationships so a bare workspace, tenant, or repository id can never
//! authorize cross-boundary access.

use crate::error::ManagerError;
use crate::id::{ConnectionId, CredentialLeaseId, OrganizationId, TenantId, WorkspaceId};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// The audience value bound to workspace runtime credentials.
pub const WORKSPACE_AUDIENCE: &str = "openflows-workspace";
pub const WORKSPACE_REFRESH_AUDIENCE: &str = "openflows-workspace-refresh";

/// Result of atomically consuming a workspace bootstrap credential.
#[derive(Debug, Clone)]
pub struct ConsumedBootstrap {
    pub workspace_id: WorkspaceId,
    pub generation: i64,
    pub bootstrap_secret_ref: String,
    pub runtime_secret_ref: Option<String>,
}

/// A row describing the authenticated runtime and its derived scope. All
/// authorization-relevant state is captured in one trusted read.
#[derive(Debug, Clone)]
pub struct RuntimeScope {
    pub credential_id: uuid::Uuid,
    pub workspace_id: WorkspaceId,
    pub workspace_role: String,
    pub workspace_generation: i64,
    pub workspace_observed_state: String,
    pub workspace_desired_state: String,
    pub workspace_registered_at: Option<DateTime<Utc>>,
    pub tenant_id: TenantId,
    pub tenant_desired_state: String,
    pub tenant_observed_state: String,
    pub organization_id: OrganizationId,
    pub organization_status: String,
    pub connection_id: ConnectionId,
    pub connection_status: String,
    pub connection_generation: i64,
    pub installation_id: i64,
    pub repository_id: i64,
    pub repository_accessible: bool,
    pub credential_generation: i64,
    pub credential_expires_at: DateTime<Utc>,
    pub credential_revoked_at: Option<DateTime<Utc>>,
    pub credential_audience: String,
    pub credential_family_id: uuid::Uuid,
    pub credential_family_expires_at: DateTime<Utc>,
}

/// A credential lease row used to record and later revoke an issued token.
#[derive(Debug, Clone)]
pub struct CredentialLease {
    pub id: CredentialLeaseId,
    pub organization_id: OrganizationId,
    pub tenant_id: Option<TenantId>,
    pub workspace_id: Option<WorkspaceId>,
    pub installation_id: i64,
    pub repository_id: i64,
    pub permission_profile_hash: String,
    pub connection_generation: i64,
    pub workspace_generation: i64,
    pub purpose: Option<String>,
    pub token_fingerprint: String,
    pub encrypted_ref: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A SHA-256 hex fingerprint of a raw token, used for lease identity without
/// persisting the token as an ordinary value.
pub fn token_fingerprint(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// The runtime credential + scope repository.
#[derive(Clone)]
pub struct RuntimeRepository {
    pool: PgPool,
}

impl RuntimeRepository {
    pub fn new(pool: PgPool) -> Self {
        RuntimeRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ------------------------------------------------------------------
    // Runtime credential lifecycle
    // ------------------------------------------------------------------

    /// Store a freshly issued runtime credential (hash only). The raw secret is
    /// returned to the trusted caller exactly once and never persisted.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_credential(
        &self,
        id: uuid::Uuid,
        workspace_id: WorkspaceId,
        credential_hash: &str,
        audience: &str,
        expires_at: DateTime<Utc>,
        generation: i64,
        family_id: uuid::Uuid,
        family_expires_at: DateTime<Utc>,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO runtime_credentials
                (id, workspace_id, credential_hash, audience, expires_at, generation,
                 family_id, family_expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(workspace_id.0)
        .bind(credential_hash)
        .bind(audience)
        .bind(expires_at)
        .bind(generation)
        .bind(family_id)
        .bind(family_expires_at)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Persist a one-time, short-lived bootstrap credential. The raw value is
    /// held only by the secret-injection backend; PostgreSQL stores its hash.
    pub async fn insert_bootstrap(
        &self,
        id: uuid::Uuid,
        workspace_id: WorkspaceId,
        hash: &str,
        secret_ref: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE workspace_bootstrap_credentials
                SET revoked_at = clock_timestamp()
              WHERE workspace_id = $1 AND consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(workspace_id.0)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO workspace_bootstrap_credentials
                (id, workspace_id, credential_hash, secret_ref, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(workspace_id.0)
        .bind(hash)
        .bind(secret_ref)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        // The workspace row points at the stable mounted bundle, not the
        // short-lived bootstrap object. Provisioning updates that bundle in
        // place after this transaction commits.
        sqlx::query("UPDATE workspaces SET registered_at = NULL, updated_at = now() WHERE id = $1")
            .bind(workspace_id.0)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn live_bootstrap_ref(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<(String, DateTime<Utc>)>, ManagerError> {
        let row = sqlx::query_as(
            "SELECT secret_ref, expires_at
               FROM workspace_bootstrap_credentials
              WHERE workspace_id = $1 AND consumed_at IS NULL
                AND revoked_at IS NULL AND expires_at > clock_timestamp()",
        )
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Atomically consume a bootstrap value and mint its first access/refresh
    /// pair. A replay observes no live bootstrap row and fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn consume_bootstrap_and_insert_session(
        &self,
        bootstrap_hash: &str,
        access_id: uuid::Uuid,
        access_hash: &str,
        access_expires_at: DateTime<Utc>,
        refresh_id: uuid::Uuid,
        refresh_hash: &str,
        family_id: uuid::Uuid,
        family_expires_at: DateTime<Utc>,
    ) -> Result<Option<ConsumedBootstrap>, ManagerError> {
        let mut tx = self.pool.begin().await?;
        let row: Option<(uuid::Uuid, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT b.workspace_id, w.credential_generation, b.secret_ref, w.runtime_secret_ref
               FROM workspace_bootstrap_credentials b
               JOIN workspaces w ON w.id = b.workspace_id
               JOIN tenants t ON t.id = w.tenant_id AND t.organization_id = w.organization_id
              WHERE b.credential_hash = $1 AND b.consumed_at IS NULL
                AND b.revoked_at IS NULL AND b.expires_at > clock_timestamp()
                AND w.desired_state NOT IN ('deleting', 'deleted')
                AND t.desired_state NOT IN ('deleting', 'deleted')
              FOR UPDATE OF b, w",
        )
        .bind(bootstrap_hash)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((workspace_id, generation, bootstrap_secret_ref, runtime_secret_ref)) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        sqlx::query(
            "UPDATE workspace_bootstrap_credentials SET consumed_at = clock_timestamp()
              WHERE credential_hash = $1 AND consumed_at IS NULL",
        )
        .bind(bootstrap_hash)
        .execute(&mut *tx)
        .await?;
        for (id, hash, audience, expires_at) in [
            (
                access_id,
                access_hash,
                WORKSPACE_AUDIENCE,
                access_expires_at,
            ),
            (
                refresh_id,
                refresh_hash,
                WORKSPACE_REFRESH_AUDIENCE,
                family_expires_at,
            ),
        ] {
            sqlx::query(
                "INSERT INTO runtime_credentials
                    (id, workspace_id, credential_hash, audience, expires_at, generation,
                     family_id, family_expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(workspace_id)
            .bind(hash)
            .bind(audience)
            .bind(expires_at)
            .bind(generation)
            .bind(family_id)
            .bind(family_expires_at)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE workspaces SET registered_at = clock_timestamp(), updated_at = now()
              WHERE id = $1",
        )
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(ConsumedBootstrap {
            workspace_id: WorkspaceId::from_uuid(workspace_id),
            generation,
            bootstrap_secret_ref,
            runtime_secret_ref,
        }))
    }

    /// Revoke a specific runtime credential by id (set revoked_at). Returns
    /// false when it does not exist or is already revoked.
    pub async fn revoke_credential(&self, id: uuid::Uuid) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE runtime_credentials
                SET revoked_at = clock_timestamp()
              WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Revoke every live credential for a workspace (used on workspace shutdown
    /// / deletion and on full credential rotation).
    pub async fn revoke_workspace_credentials(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<u64, ManagerError> {
        let affected = sqlx::query(
            "UPDATE runtime_credentials
                SET revoked_at = clock_timestamp()
              WHERE workspace_id = $1 AND revoked_at IS NULL",
        )
        .bind(workspace_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected())
    }

    /// Bump the current credential generation for a workspace, invalidating all
    /// credentials minted under an older generation. Returns the new generation.
    pub async fn bump_workspace_generation(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<i64, ManagerError> {
        let row: (i64,) = sqlx::query_as(
            "UPDATE workspaces
                SET credential_generation = credential_generation + 1, updated_at = now()
              WHERE id = $1
             RETURNING credential_generation",
        )
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .ok_or_else(|| ManagerError::not_found("workspace"))?;
        Ok(row.0)
    }

    /// Atomically rotate the workspace generation, revoke old credentials and
    /// leases, and insert the replacement credential.
    #[allow(clippy::too_many_arguments)]
    pub async fn rotate_credential_pair(
        &self,
        access_id: uuid::Uuid,
        workspace_id: WorkspaceId,
        presented_refresh_hash: Option<&str>,
        access_hash: &str,
        access_expires_at: DateTime<Utc>,
        refresh_id: uuid::Uuid,
        refresh_hash: &str,
        family_id: uuid::Uuid,
        family_expires_at: DateTime<Utc>,
    ) -> Result<i64, ManagerError> {
        let mut tx = self.pool.begin().await?;
        let generation: i64 = sqlx::query_scalar(
            "UPDATE workspaces SET credential_generation = credential_generation + 1, updated_at = now()
             WHERE id = $1
               AND ($2::text IS NULL OR EXISTS (
                 SELECT 1 FROM runtime_credentials rc
                  WHERE rc.workspace_id = workspaces.id
                    AND rc.credential_hash = $2
                    AND rc.audience = 'openflows-workspace-refresh'
                    AND rc.revoked_at IS NULL
                    AND rc.expires_at > clock_timestamp()
                    AND rc.generation = workspaces.credential_generation
               ))
             RETURNING credential_generation",
        ).bind(workspace_id.0).bind(presented_refresh_hash).fetch_optional(&mut *tx).await?
            .ok_or_else(|| ManagerError::not_found("workspace"))?;
        sqlx::query("UPDATE runtime_credentials SET revoked_at = clock_timestamp() WHERE workspace_id = $1 AND revoked_at IS NULL")
            .bind(workspace_id.0).execute(&mut *tx).await?;
        sqlx::query("UPDATE credential_leases SET revoked_at = clock_timestamp(), encrypted_ref = NULL WHERE workspace_id = $1 AND revoked_at IS NULL")
            .bind(workspace_id.0).execute(&mut *tx).await?;
        for (id, hash, audience, expires_at) in [
            (
                access_id,
                access_hash,
                WORKSPACE_AUDIENCE,
                access_expires_at,
            ),
            (
                refresh_id,
                refresh_hash,
                WORKSPACE_REFRESH_AUDIENCE,
                family_expires_at,
            ),
        ] {
            sqlx::query("INSERT INTO runtime_credentials (id, workspace_id, credential_hash, audience, expires_at, generation, family_id, family_expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
                .bind(id).bind(workspace_id.0).bind(hash).bind(audience).bind(expires_at).bind(generation)
                .bind(family_id).bind(family_expires_at).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(generation)
    }

    /// Read the current credential generation for a workspace.
    pub async fn workspace_generation(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<i64>, ManagerError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT credential_generation FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.map(|r| r.0))
    }

    // ------------------------------------------------------------------
    // Runtime authentication and scope resolution
    // ------------------------------------------------------------------

    /// Resolve the authenticated runtime scope for a presented workspace bearer
    /// credential. The credential is matched by its hash, then the full trusted
    /// relationship chain (workspace -> tenant -> organization -> connection ->
    /// repository) is read in one query. Authorization state (audience, expiry,
    /// revocation, current workspace generation) is returned for the caller to
    /// validate; this method does not enforce policy.
    pub async fn resolve_runtime_scope(
        &self,
        credential_hash: &str,
    ) -> Result<Option<RuntimeScope>, ManagerError> {
        let row = sqlx::query_as::<_, RuntimeScopeRow>(
            "SELECT rc.id AS credential_id, rc.workspace_id, w.role AS workspace_role,
                    w.credential_generation AS workspace_generation,
                    w.observed_state AS workspace_observed_state,
                    w.desired_state AS workspace_desired_state,
                    w.registered_at AS workspace_registered_at,
                    t.id AS tenant_id, t.desired_state AS tenant_desired_state,
                    t.observed_state AS tenant_observed_state,
                    o.id AS organization_id, o.status AS organization_status,
                    c.id AS connection_id, c.status AS connection_status,
                    c.access_generation AS connection_generation,
                    c.installation_id, t.github_repository_id AS repository_id,
                    r.accessible AS repository_accessible,
                    rc.generation AS credential_generation, rc.expires_at AS credential_expires_at,
                    rc.revoked_at AS credential_revoked_at, rc.audience AS credential_audience,
                    rc.family_id AS credential_family_id,
                    rc.family_expires_at AS credential_family_expires_at
               FROM runtime_credentials rc
               JOIN workspaces w ON w.id = rc.workspace_id
               JOIN tenants t ON t.id = w.tenant_id AND t.organization_id = w.organization_id
               JOIN organizations o ON o.id = t.organization_id
               JOIN github_connections c ON c.id = t.connection_id AND c.organization_id = t.organization_id
               JOIN github_repositories r ON r.connection_id = c.id
                    AND r.organization_id = c.organization_id
                    AND r.github_repository_id = t.github_repository_id
              WHERE rc.credential_hash = $1",
        )
        .bind(credential_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(RuntimeScope::from))
    }

    /// Re-read the authoritative runtime scope for a workspace, used to recheck
    /// generations and resource state after an external token exchange. Never
    /// uses the (potentially stale) credential hash path.
    pub async fn resolve_workspace_scope(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<RuntimeScope>, ManagerError> {
        let row = sqlx::query_as::<_, WorkspaceScopeRow>(
            "SELECT w.id AS workspace_id, w.role AS workspace_role,
                    w.credential_generation AS workspace_generation,
                    w.observed_state AS workspace_observed_state,
                    w.desired_state AS workspace_desired_state,
                    w.registered_at AS workspace_registered_at,
                    t.id AS tenant_id, t.desired_state AS tenant_desired_state,
                    t.observed_state AS tenant_observed_state,
                    o.id AS organization_id, o.status AS organization_status,
                    c.id AS connection_id, c.status AS connection_status,
                    c.access_generation AS connection_generation,
                    c.installation_id, t.github_repository_id AS repository_id,
                    r.accessible AS repository_accessible
               FROM workspaces w
               JOIN tenants t ON t.id = w.tenant_id AND t.organization_id = w.organization_id
               JOIN organizations o ON o.id = t.organization_id
               JOIN github_connections c ON c.id = t.connection_id AND c.organization_id = t.organization_id
               JOIN github_repositories r ON r.connection_id = c.id
                    AND r.organization_id = c.organization_id
                    AND r.github_repository_id = t.github_repository_id
              WHERE w.id = $1",
        )
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(RuntimeScope::from))
    }

    /// Recheck the exact bearer credential after an external exchange.
    pub async fn credential_is_current(
        &self,
        credential_id: uuid::Uuid,
        workspace_id: WorkspaceId,
        generation: i64,
    ) -> Result<bool, ManagerError> {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM runtime_credentials rc
                JOIN workspaces w ON w.id = rc.workspace_id
                WHERE rc.id = $1 AND rc.workspace_id = $2
                  AND rc.revoked_at IS NULL AND rc.expires_at > clock_timestamp()
                  AND rc.generation = $3 AND w.credential_generation = $3
            )",
        )
        .bind(credential_id)
        .bind(workspace_id.0)
        .bind(generation)
        .fetch_one(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(valid)
    }

    // ------------------------------------------------------------------
    // Credential leases
    // ------------------------------------------------------------------

    /// Record a credential lease after a successful token exchange and after
    /// the authorization/generation recheck. The token value is stored only as
    /// an encrypted secret-provider reference when reuse/revocation requires it
    /// (i.e. always for runtime credentials, so cleanup can trace and discard it); the raw
    /// value never appears in an ordinary table.
    pub async fn insert_lease(
        &self,
        lease: &NewCredentialLease,
    ) -> Result<CredentialLeaseId, ManagerError> {
        let mut tx = self.pool.begin().await?;
        let id = Self::insert_lease_in_tx(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn record_authorized_lease(
        &self,
        scope: &RuntimeScope,
        lease: &NewCredentialLease,
        existing: Option<CredentialLeaseId>,
    ) -> Result<CredentialLeaseId, ManagerError> {
        let mut tx = self.pool.begin().await?;
        // Match lifecycle lock ordering. State and credential locks are retained
        // until the lease commits, serializing issuance against revocation.
        for (query, id) in [
            (
                "SELECT id FROM organizations WHERE id = $1 FOR UPDATE",
                scope.organization_id.0,
            ),
            (
                "SELECT id FROM github_connections WHERE id = $1 FOR UPDATE",
                scope.connection_id.0,
            ),
            (
                "SELECT id FROM tenants WHERE id = $1 FOR UPDATE",
                scope.tenant_id.0,
            ),
            (
                "SELECT id FROM workspaces WHERE id = $1 FOR UPDATE",
                scope.workspace_id.0,
            ),
            (
                "SELECT id FROM runtime_credentials WHERE id = $1 FOR UPDATE",
                scope.credential_id,
            ),
        ] {
            sqlx::query(query).bind(id).fetch_optional(&mut *tx).await?;
        }
        let valid: Option<i64> = sqlx::query_scalar("SELECT r.github_repository_id
            FROM organizations o
            JOIN tenants t ON t.organization_id = o.id
            JOIN workspaces w ON w.tenant_id = t.id AND w.organization_id = o.id
            JOIN runtime_credentials rc ON rc.workspace_id = w.id
            JOIN github_connections c ON c.id = t.connection_id AND c.organization_id = o.id
            JOIN github_repositories r ON r.connection_id = c.id AND r.organization_id = o.id AND r.github_repository_id = t.github_repository_id
            WHERE o.id = $1 AND t.id = $2 AND w.id = $3 AND rc.id = $4 AND c.id = $5
              AND r.github_repository_id = $6 AND c.installation_id = $7
              AND c.access_generation = $8 AND w.credential_generation = $9
              AND rc.generation = $9 AND rc.audience = 'openflows-workspace'
              AND rc.revoked_at IS NULL AND rc.expires_at > clock_timestamp()
              AND o.status IN ('ready','provisioning') AND c.status = 'active'
              AND t.desired_state IN ('running', 'provisioning')
              AND t.observed_state NOT IN ('deleted', 'deleting')
              AND w.desired_state IN ('running', 'provisioning')
              AND (
                    w.observed_state = 'running'
                    OR (
                        w.observed_state = 'provisioning'
                        AND w.desired_state = 'provisioning'
                        AND w.registered_at IS NOT NULL
                    )
              )
              AND w.role = $10 AND r.accessible
            FOR UPDATE OF r")
            .bind(scope.organization_id.0).bind(scope.tenant_id.0).bind(scope.workspace_id.0)
            .bind(scope.credential_id).bind(scope.connection_id.0).bind(scope.repository_id)
            .bind(scope.installation_id).bind(scope.connection_generation).bind(scope.workspace_generation)
            .bind(&scope.workspace_role).fetch_optional(&mut *tx).await?;
        if valid.is_none() {
            return Err(ManagerError::api(
                "AUTH_CHANGED",
                "runtime authorization changed",
            ));
        }
        if let Some(id) = existing {
            let valid: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM credential_leases WHERE id = $1 AND workspace_id = $2 AND connection_id = $3 AND revoked_at IS NULL AND expires_at > clock_timestamp() AND token_fingerprint = $4 FOR UPDATE")
                .bind(id.0).bind(scope.workspace_id.0).bind(scope.connection_id.0).bind(&lease.token_fingerprint)
                .fetch_optional(&mut *tx).await?;
            if valid.is_none() {
                return Err(ManagerError::api(
                    "AUTH_CHANGED",
                    "credential lease is no longer valid",
                ));
            }
            tx.commit().await?;
            return Ok(id);
        }
        let id = Self::insert_lease_in_tx(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn insert_lease_in_tx(
        tx: &mut sqlx::PgConnection,
        lease: &NewCredentialLease,
    ) -> Result<CredentialLeaseId, ManagerError> {
        let id = CredentialLeaseId::new();
        sqlx::query(
            "INSERT INTO credential_leases
                (id, organization_id, connection_id, tenant_id, workspace_id, installation_id,
                 repository_id, permission_profile_hash, connection_generation,
                 workspace_generation, purpose, token_fingerprint, encrypted_ref,
                 expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id.0)
        .bind(lease.organization_id.0)
        .bind(lease.connection_id.0)
        .bind(lease.tenant_id.map(|t| t.0))
        .bind(lease.workspace_id.map(|w| w.0))
        .bind(lease.installation_id)
        .bind(lease.repository_id)
        .bind(&lease.permission_profile_hash)
        .bind(lease.connection_generation)
        .bind(lease.workspace_generation)
        .bind(&lease.purpose)
        .bind(&lease.token_fingerprint)
        .bind(&lease.encrypted_ref)
        .bind(lease.expires_at)
        .execute(tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(id)
    }

    /// Recheck that a recorded lease's generations still match the current
    /// authoritative connection and workspace generations, and that it is not
    /// revoked. Used to fail closed after an external exchange.
    pub async fn lease_generations_current(
        &self,
        lease_id: CredentialLeaseId,
        expected_connection_generation: i64,
        expected_workspace_generation: i64,
    ) -> Result<bool, ManagerError> {
        let row: Option<(i64, i64, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT l.connection_generation, l.workspace_generation, l.revoked_at
               FROM credential_leases l
               JOIN github_connections c ON c.id = l.connection_id
               JOIN workspaces w ON w.id = l.workspace_id
              WHERE l.id = $1 AND c.access_generation = $2
                AND w.credential_generation = $3
                AND c.status = 'active' AND w.observed_state = 'running'
                AND w.desired_state IN ('running', 'provisioning')",
        )
        .bind(lease_id.0)
        .bind(expected_connection_generation)
        .bind(expected_workspace_generation)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(match row {
            Some((cg, wg, revoked)) => {
                revoked.is_none()
                    && cg == expected_connection_generation
                    && wg == expected_workspace_generation
            }
            None => false,
        })
    }

    /// Revoke and discard a lease (and its encrypted token reference) — used on
    /// disconnect, installation removal, workspace shutdown, and when a token
    /// must be discarded after an authorization change.
    pub async fn revoke_lease(&self, lease_id: CredentialLeaseId) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE credential_leases
                SET revoked_at = clock_timestamp(), encrypted_ref = NULL
              WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(lease_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Revoke every unrevoked lease scoped to a connection (disconnect /
    /// installation removal / repository removal). Returns the count revoked.
    pub async fn revoke_leases_for_connection(
        &self,
        organization_id: OrganizationId,
        connection_id: ConnectionId,
    ) -> Result<u64, ManagerError> {
        let affected = sqlx::query(
            "UPDATE credential_leases
                SET revoked_at = clock_timestamp(), encrypted_ref = NULL
              WHERE organization_id = $1 AND connection_id = $2 AND revoked_at IS NULL",
        )
        .bind(organization_id.0)
        .bind(connection_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected())
    }

    /// Revoke every unrevoked lease scoped to a workspace (workspace shutdown
    /// / deletion / runtime credential rotation).
    pub async fn revoke_leases_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<u64, ManagerError> {
        let affected = sqlx::query(
            "UPDATE credential_leases
                SET revoked_at = clock_timestamp(), encrypted_ref = NULL
              WHERE workspace_id = $1 AND revoked_at IS NULL",
        )
        .bind(workspace_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected())
    }
}

/// Input for inserting a credential lease.
pub struct NewCredentialLease {
    pub organization_id: OrganizationId,
    pub connection_id: ConnectionId,
    pub tenant_id: Option<TenantId>,
    pub workspace_id: Option<WorkspaceId>,
    pub installation_id: i64,
    pub repository_id: i64,
    pub permission_profile_hash: String,
    pub connection_generation: i64,
    pub workspace_generation: i64,
    pub purpose: String,
    pub token_fingerprint: String,
    pub encrypted_ref: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// The row shape returned by the credential-hash scope resolution.
#[derive(sqlx::FromRow)]
struct RuntimeScopeRow {
    credential_id: uuid::Uuid,
    workspace_id: uuid::Uuid,
    workspace_role: String,
    workspace_generation: i64,
    workspace_observed_state: String,
    workspace_desired_state: String,
    workspace_registered_at: Option<DateTime<Utc>>,
    tenant_id: uuid::Uuid,
    tenant_desired_state: String,
    tenant_observed_state: String,
    organization_id: uuid::Uuid,
    organization_status: String,
    connection_id: uuid::Uuid,
    connection_status: String,
    connection_generation: i64,
    installation_id: i64,
    repository_id: i64,
    repository_accessible: bool,
    credential_generation: i64,
    credential_expires_at: DateTime<Utc>,
    credential_revoked_at: Option<DateTime<Utc>>,
    credential_audience: String,
    credential_family_id: uuid::Uuid,
    credential_family_expires_at: DateTime<Utc>,
}

impl From<RuntimeScopeRow> for RuntimeScope {
    fn from(t: RuntimeScopeRow) -> Self {
        RuntimeScope {
            credential_id: t.credential_id,
            workspace_id: WorkspaceId::from_uuid(t.workspace_id),
            workspace_role: t.workspace_role,
            workspace_generation: t.workspace_generation,
            workspace_observed_state: t.workspace_observed_state,
            workspace_desired_state: t.workspace_desired_state,
            workspace_registered_at: t.workspace_registered_at,
            tenant_id: TenantId::from_uuid(t.tenant_id),
            tenant_desired_state: t.tenant_desired_state,
            tenant_observed_state: t.tenant_observed_state,
            organization_id: OrganizationId::from_uuid(t.organization_id),
            organization_status: t.organization_status,
            connection_id: ConnectionId::from_uuid(t.connection_id),
            connection_status: t.connection_status,
            connection_generation: t.connection_generation,
            installation_id: t.installation_id,
            repository_id: t.repository_id,
            repository_accessible: t.repository_accessible,
            credential_generation: t.credential_generation,
            credential_expires_at: t.credential_expires_at,
            credential_revoked_at: t.credential_revoked_at,
            credential_audience: t.credential_audience,
            credential_family_id: t.credential_family_id,
            credential_family_expires_at: t.credential_family_expires_at,
        }
    }
}

/// The row shape returned by the workspace scope resolution (no credential
/// fields).
#[derive(sqlx::FromRow)]
struct WorkspaceScopeRow {
    workspace_id: uuid::Uuid,
    workspace_role: String,
    workspace_generation: i64,
    workspace_observed_state: String,
    workspace_desired_state: String,
    workspace_registered_at: Option<DateTime<Utc>>,
    tenant_id: uuid::Uuid,
    tenant_desired_state: String,
    tenant_observed_state: String,
    organization_id: uuid::Uuid,
    organization_status: String,
    connection_id: uuid::Uuid,
    connection_status: String,
    connection_generation: i64,
    installation_id: i64,
    repository_id: i64,
    repository_accessible: bool,
}

impl From<WorkspaceScopeRow> for RuntimeScope {
    fn from(t: WorkspaceScopeRow) -> Self {
        RuntimeScope {
            credential_id: uuid::Uuid::nil(),
            workspace_id: WorkspaceId::from_uuid(t.workspace_id),
            workspace_role: t.workspace_role,
            workspace_generation: t.workspace_generation,
            workspace_observed_state: t.workspace_observed_state,
            workspace_desired_state: t.workspace_desired_state,
            workspace_registered_at: t.workspace_registered_at,
            tenant_id: TenantId::from_uuid(t.tenant_id),
            tenant_desired_state: t.tenant_desired_state,
            tenant_observed_state: t.tenant_observed_state,
            organization_id: OrganizationId::from_uuid(t.organization_id),
            organization_status: t.organization_status,
            connection_id: ConnectionId::from_uuid(t.connection_id),
            connection_status: t.connection_status,
            connection_generation: t.connection_generation,
            installation_id: t.installation_id,
            repository_id: t.repository_id,
            repository_accessible: t.repository_accessible,
            credential_generation: 0,
            credential_expires_at: DateTime::<Utc>::UNIX_EPOCH,
            credential_revoked_at: None,
            credential_audience: String::new(),
            credential_family_id: uuid::Uuid::nil(),
            credential_family_expires_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }
}
