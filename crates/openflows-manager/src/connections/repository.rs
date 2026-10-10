//! Repository for GitHub connection lifecycle persistence: connection attempts,
//! immutable installation bindings, and synchronized repositories.
//!
//! Every method that writes a connection or repository is organization-scoped.
//! The final binding transaction locks the organization and membership rows so
//! concurrent revocation cannot race the authoritative write, and the unique
//! `(app_id, installation_id)` constraint enforces immutability at the database
//! level.

use crate::error::ManagerError;
use crate::id::{ConnectionId, OrganizationId, UserId};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// The lifecycle status of a connection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Pending,
    Verified,
    Consumed,
    Expired,
}

impl AttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttemptStatus::Pending => "pending",
            AttemptStatus::Verified => "verified",
            AttemptStatus::Consumed => "consumed",
            AttemptStatus::Expired => "expired",
        }
    }
}

/// The flow type of a connection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowType {
    /// Only the GitHub user OAuth proof is required.
    UserOauth,
    /// Only the installation-setup proof is required.
    InstallationSetup,
    /// Both proofs are required (default).
    Both,
}

impl FlowType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlowType::UserOauth => "user_oauth",
            FlowType::InstallationSetup => "installation_setup",
            FlowType::Both => "both",
        }
    }
}

/// A GitHub connection attempt row.
#[derive(Debug, Clone)]
pub struct AttemptRow {
    pub id: uuid::Uuid,
    pub organization_id: OrganizationId,
    pub initiated_by: UserId,
    pub flow_type: String,
    pub oauth_state_hash: Option<String>,
    pub setup_state_hash: Option<String>,
    pub oauth_code_verifier_ref: Option<Vec<u8>>,
    pub oauth_github_user_id: Option<i64>,
    pub candidate_installation: Option<i64>,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub oauth_consumed_at: Option<DateTime<Utc>>,
    pub setup_consumed_at: Option<DateTime<Utc>>,
    pub consumed_at: Option<DateTime<Utc>>,
}

impl AttemptRow {
    pub fn oauth_consumed(&self) -> bool {
        self.oauth_consumed_at.is_some()
    }
    pub fn setup_consumed(&self) -> bool {
        self.setup_consumed_at.is_some()
    }
    /// Whether both required proofs are present and the attempt can bind.
    pub fn proofs_complete(&self) -> bool {
        self.oauth_consumed() && self.setup_consumed()
    }
}

/// A connection row.
#[derive(Debug, Clone)]
pub struct ConnectionRow {
    pub id: ConnectionId,
    pub organization_id: OrganizationId,
    pub app_id: i64,
    pub installation_id: i64,
    pub github_account_id: i64,
    pub account_type: String,
    pub account_login: Option<String>,
    pub status: String,
    pub access_generation: i64,
    pub connected_by: Option<UserId>,
    pub verified_at: Option<DateTime<Utc>>,
    pub last_reconciled_at: Option<DateTime<Utc>>,
    pub access_revoked_reason: Option<String>,
}

/// A repository row.
#[derive(Debug, Clone)]
pub struct RepositoryRow {
    pub organization_id: OrganizationId,
    pub connection_id: ConnectionId,
    pub github_repository_id: i64,
    pub full_name: String,
    pub accessible: bool,
    pub last_seen_at: DateTime<Utc>,
}

/// The connection lifecycle repository.
#[derive(Clone)]
pub struct ConnectionRepository {
    pool: PgPool,
}

impl ConnectionRepository {
    pub fn new(pool: PgPool) -> Self {
        ConnectionRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ------------------------------------------------------------------
    // Attempts
    // ------------------------------------------------------------------

    /// Insert a new connection attempt. Only hashes of the state values are
    /// persisted; the raw states are returned once to the initiating browser.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_attempt(
        &self,
        id: uuid::Uuid,
        organization_id: OrganizationId,
        initiated_by: UserId,
        flow_type: FlowType,
        oauth_state_hash: Option<&str>,
        setup_state_hash: Option<&str>,
        oauth_verifier_ref: Option<&[u8]>,
        expires_minutes: i64,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        Self::create_attempt_in_tx(
            &mut tx,
            id,
            organization_id,
            initiated_by,
            flow_type,
            oauth_state_hash,
            setup_state_hash,
            oauth_verifier_ref,
            expires_minutes,
        )
        .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    /// Insert a new connection attempt inside an existing transaction (used by
    /// the idempotent connect flow).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_attempt_in_tx(
        tx: &mut PgConnection,
        id: uuid::Uuid,
        organization_id: OrganizationId,
        initiated_by: UserId,
        flow_type: FlowType,
        oauth_state_hash: Option<&str>,
        setup_state_hash: Option<&str>,
        oauth_verifier_ref: Option<&[u8]>,
        expires_minutes: i64,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO github_connect_attempts
                (id, organization_id, initiated_by, flow_type, oauth_state_hash,
                 setup_state_hash, oauth_code_verifier_ref, status, expires_at, state_hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending',
                     clock_timestamp() + make_interval(mins => $8::bigint::integer), $1::text)",
        )
        .bind(id)
        .bind(organization_id.0)
        .bind(initiated_by.0)
        .bind(flow_type.as_str())
        .bind(oauth_state_hash)
        .bind(setup_state_hash)
        .bind(oauth_verifier_ref)
        .bind(expires_minutes)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Resolve an attempt by its OAuth state hash (must be pending/verified and
    /// unexpired). Used by the OAuth callback.
    pub async fn attempt_by_oauth_state(
        &self,
        state_hash: &str,
    ) -> Result<Option<AttemptRow>, ManagerError> {
        self.fetch_attempt(
            "SELECT id, organization_id, initiated_by, flow_type, oauth_state_hash,
                    setup_state_hash, oauth_code_verifier_ref, oauth_github_user_id,
                    candidate_installation, status, expires_at, oauth_consumed_at,
                    setup_consumed_at, consumed_at
               FROM github_connect_attempts
              WHERE oauth_state_hash = $1 AND status IN ('pending','verified')
                AND expires_at > clock_timestamp()",
            state_hash,
        )
        .await
    }

    /// Resolve an attempt by its setup state hash (must be pending/verified and
    /// unexpired). Used by the installation-setup callback.
    pub async fn attempt_by_setup_state(
        &self,
        state_hash: &str,
    ) -> Result<Option<AttemptRow>, ManagerError> {
        self.fetch_attempt(
            "SELECT id, organization_id, initiated_by, flow_type, oauth_state_hash,
                    setup_state_hash, oauth_code_verifier_ref, oauth_github_user_id,
                    candidate_installation, status, expires_at, oauth_consumed_at,
                    setup_consumed_at, consumed_at
               FROM github_connect_attempts
              WHERE setup_state_hash = $1 AND status IN ('pending','verified')
                AND expires_at > clock_timestamp()",
            state_hash,
        )
        .await
    }

    async fn fetch_attempt(
        &self,
        query: &str,
        state_hash: &str,
    ) -> Result<Option<AttemptRow>, ManagerError> {
        let row = sqlx::query_as::<_, AttemptTuple>(query)
            .bind(state_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(ManagerError::from)?;
        Ok(row.map(AttemptRow::from))
    }

    /// Whether an attempt with this OAuth state exists (for replay reporting).
    pub async fn attempt_oauth_state_exists(&self, state_hash: &str) -> Result<bool, ManagerError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM github_connect_attempts WHERE oauth_state_hash = $1",
        )
        .bind(state_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.0 > 0)
    }

    /// Fetch an attempt by id (used to re-read state after consuming a proof).
    pub async fn attempt_by_id(
        &self,
        attempt_id: uuid::Uuid,
    ) -> Result<Option<AttemptRow>, ManagerError> {
        let row = sqlx::query_as::<_, AttemptTuple>(
            "SELECT id, organization_id, initiated_by, flow_type, oauth_state_hash,
                    setup_state_hash, oauth_code_verifier_ref, oauth_github_user_id,
                    candidate_installation, status, expires_at, oauth_consumed_at,
                    setup_consumed_at, consumed_at
               FROM github_connect_attempts
              WHERE id = $1",
        )
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(AttemptRow::from))
    }

    /// Mark an attempt's OAuth proof consumed (inside a transaction), storing
    /// the encrypted user token (used later for authority verification when the
    /// setup proof arrives second) and the fetched GitHub user id. Returns
    /// false when it was already consumed or the attempt is no longer pending.
    pub async fn consume_oauth_proof_in_tx(
        tx: &mut PgConnection,
        attempt_id: uuid::Uuid,
        github_user_id: i64,
        verifier_ref: Option<&[u8]>,
        user_token_ref: Option<&[u8]>,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connect_attempts
                SET oauth_consumed_at = clock_timestamp(),
                    oauth_github_user_id = $2,
                    oauth_code_verifier_ref = COALESCE($3, oauth_code_verifier_ref),
                    user_credential_ref = COALESCE(encode($4::bytea, 'hex'), user_credential_ref)
              WHERE id = $1 AND oauth_consumed_at IS NULL
                AND status IN ('pending','verified') AND expires_at > clock_timestamp()",
        )
        .bind(attempt_id)
        .bind(github_user_id)
        .bind(verifier_ref)
        .bind(user_token_ref)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Read the encrypted user-token reference and the candidate installation
    /// for an attempt, used to complete the binding when the setup proof
    /// arrives after the OAuth proof.
    pub async fn attempt_credentials(
        &self,
        attempt_id: uuid::Uuid,
    ) -> Result<Option<(Option<Vec<u8>>, Option<i64>)>, ManagerError> {
        let row: Option<(Option<Vec<u8>>, Option<i64>)> = sqlx::query_as(
            "SELECT decode(user_credential_ref, 'hex'), candidate_installation
               FROM github_connect_attempts WHERE id = $1",
        )
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row)
    }

    /// Mark an attempt's setup proof consumed and record the candidate
    /// installation (inside a transaction). Returns false when it was already
    /// consumed or the attempt is no longer pending.
    pub async fn consume_setup_proof_in_tx(
        tx: &mut PgConnection,
        attempt_id: uuid::Uuid,
        installation_id: i64,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connect_attempts
                SET setup_consumed_at = clock_timestamp(),
                    candidate_installation = $2
              WHERE id = $1 AND setup_consumed_at IS NULL
                AND status IN ('pending','verified') AND expires_at > clock_timestamp()",
        )
        .bind(attempt_id)
        .bind(installation_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Transition an attempt to `verified` once both proofs are present (inside
    /// a transaction). No-op when it is no longer pending.
    pub async fn mark_verified_in_tx(
        tx: &mut PgConnection,
        attempt_id: uuid::Uuid,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connect_attempts
                SET status = 'verified'
              WHERE id = $1 AND status = 'pending'",
        )
        .bind(attempt_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Consume an attempt (inside a transaction), preventing replay. Returns
    /// whether a pending/verified attempt was actually consumed.
    pub async fn consume_attempt_in_tx(
        tx: &mut PgConnection,
        attempt_id: uuid::Uuid,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connect_attempts
                SET status = 'consumed', consumed_at = clock_timestamp(),
                    user_credential_ref = NULL, oauth_code_verifier_ref = NULL
              WHERE id = $1 AND status IN ('pending','verified')
                AND expires_at > clock_timestamp()",
        )
        .bind(attempt_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Expire all attempts past their deadline (background maintenance).
    pub async fn expire_attempts(&self) -> Result<u64, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connect_attempts
                SET status = 'expired'
              WHERE status IN ('pending','verified') AND expires_at <= clock_timestamp()",
        )
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected())
    }

    // ------------------------------------------------------------------
    // Connections
    // ------------------------------------------------------------------

    /// Look up a connection by the immutable installation identity. Returns the
    /// owning organization *without* revealing it to a caller (used to detect
    /// "already bound to another organization").
    pub async fn connection_by_installation(
        &self,
        app_id: i64,
        installation_id: i64,
    ) -> Result<Option<ConnectionRow>, ManagerError> {
        let row = sqlx::query_as::<_, ConnectionTuple>(
            "SELECT id, organization_id, app_id, installation_id, github_account_id,
                    account_type, account_login, status, access_generation, connected_by,
                    verified_at, last_reconciled_at, access_revoked_reason
               FROM github_connections
              WHERE app_id = $1 AND installation_id = $2",
        )
        .bind(app_id)
        .bind(installation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(ConnectionRow::from))
    }

    /// Fetch a connection scoped to an organization (None if it does not belong
    /// to the org).
    pub async fn get_connection_scoped(
        &self,
        organization_id: OrganizationId,
        id: ConnectionId,
    ) -> Result<Option<ConnectionRow>, ManagerError> {
        let row = sqlx::query_as::<_, ConnectionTuple>(
            "SELECT id, organization_id, app_id, installation_id, github_account_id,
                    account_type, account_login, status, access_generation, connected_by,
                    verified_at, last_reconciled_at, access_revoked_reason
               FROM github_connections
              WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.0)
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(ConnectionRow::from))
    }

    /// List connections for an organization (sanitized status/account only).
    pub async fn list_connections(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<ConnectionRow>, ManagerError> {
        let rows = sqlx::query_as::<_, ConnectionTuple>(
            "SELECT id, organization_id, app_id, installation_id, github_account_id,
                    account_type, account_login, status, access_generation, connected_by,
                    verified_at, last_reconciled_at, access_revoked_reason
               FROM github_connections
              WHERE organization_id = $1
              ORDER BY created_at",
        )
        .bind(organization_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(rows.into_iter().map(ConnectionRow::from).collect())
    }

    /// Insert a connection binding (inside the final binding transaction).
    /// Returns `Err(INSTALLATION_ALREADY_BOUND)` on a unique-constraint
    /// conflict so the caller can re-check idempotently without revealing the
    /// other organization.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_connection_in_tx(
        tx: &mut PgConnection,
        id: ConnectionId,
        organization_id: OrganizationId,
        app_id: i64,
        installation_id: i64,
        github_account_id: i64,
        account_type: &str,
        account_login: Option<&str>,
        connected_by: UserId,
    ) -> Result<(), ManagerError> {
        let result = sqlx::query(
            "INSERT INTO github_connections
                (id, organization_id, app_id, installation_id, github_account_id,
                 account_type, account_login, status, connected_by, verified_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', $8, clock_timestamp())",
        )
        .bind(id.0)
        .bind(organization_id.0)
        .bind(app_id)
        .bind(installation_id)
        .bind(github_account_id)
        .bind(account_type)
        .bind(account_login)
        .bind(connected_by.0)
        .execute(&mut *tx)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db))
                if db.constraint() == Some("github_connections_app_id_installation_id_key") =>
            {
                Err(ManagerError::api(
                    "INSTALLATION_ALREADY_BOUND",
                    "this GitHub installation is already connected to an organization",
                ))
            }
            Err(e) => Err(ManagerError::from(e)),
        }
    }

    /// Disconnect a connection: immediately disable local access (status +
    /// access-generation increment) so new runtime credential issuance stops
    /// before asynchronous cleanup.
    pub async fn disconnect_connection(
        &self,
        organization_id: OrganizationId,
        id: ConnectionId,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connections
                SET status = 'disconnected',
                    access_generation = access_generation + 1,
                    access_revoked_reason = 'disconnected',
                    updated_at = now()
              WHERE id = $1 AND organization_id = $2
                AND status IN ('active','suspended','pending')",
        )
        .bind(id.0)
        .bind(organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Conservatively revoke access for an access-removing event: set status and
    /// increment the access generation. `reason` explains why (suspended,
    /// deleted, repos removed).
    pub async fn revoke_access(
        &self,
        app_id: i64,
        installation_id: i64,
        status: &str,
        reason: &str,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connections
                SET status = $3,
                    access_generation = access_generation + 1,
                    access_revoked_reason = $4,
                    updated_at = now()
              WHERE app_id = $1 AND installation_id = $2
                AND status IN ('active','pending','suspended')",
        )
        .bind(app_id)
        .bind(installation_id)
        .bind(status)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Mark a connection active after successful reconciliation of fresh
    /// authoritative state. Refuses to reactivate when a removal reason is
    /// still recorded — the caller must have cleared it via reconciliation.
    pub async fn reactivate_connection(
        &self,
        app_id: i64,
        installation_id: i64,
        expected_generation: i64,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connections
                SET status = 'active',
                    access_revoked_reason = NULL,
                    last_reconciled_at = clock_timestamp(),
                    updated_at = now()
              WHERE app_id = $1 AND installation_id = $2
                AND status = 'suspended'
                AND access_generation = $3
                AND access_revoked_reason IS DISTINCT FROM 'disconnected'",
        )
        .bind(app_id)
        .bind(installation_id)
        .bind(expected_generation)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    /// Update `last_reconciled_at` for a connection.
    pub async fn touch_reconciled(
        &self,
        organization_id: OrganizationId,
        id: ConnectionId,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE github_connections SET last_reconciled_at = clock_timestamp(),
                    updated_at = now()
              WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.0)
        .bind(organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(affected.rows_affected() == 1)
    }

    // ------------------------------------------------------------------
    // Repositories
    // ------------------------------------------------------------------

    /// Upsert synchronized repositories by immutable GitHub id, updating the
    /// current owner/name when GitHub renames a repository. Never changes the
    /// connection or organization identity.
    pub async fn upsert_repositories(
        &self,
        organization_id: OrganizationId,
        connection_id: ConnectionId,
        repos: &[crate::connections::github_app::RepoRef],
    ) -> Result<(), ManagerError> {
        for repo in repos {
            sqlx::query(
                "INSERT INTO github_repositories
                    (organization_id, connection_id, github_repository_id, full_name, accessible, last_seen_at)
                 VALUES ($1, $2, $3, $4, true, clock_timestamp())
                 ON CONFLICT (connection_id, github_repository_id) DO UPDATE
                 SET full_name = EXCLUDED.full_name,
                     accessible = true,
                     last_seen_at = clock_timestamp(),
                     updated_at = now()",
            )
            .bind(organization_id.0)
            .bind(connection_id.0)
            .bind(repo.id)
            .bind(&repo.full_name)
            .execute(&self.pool)
            .await
            .map_err(ManagerError::from)?;
        }
        Ok(())
    }

    /// Mark repositories that are no longer present in the authoritative
    /// response as inaccessible (conservative removal). `present_ids` is the set
    /// of immutable ids currently visible.
    pub async fn mark_missing_inaccessible(
        &self,
        organization_id: OrganizationId,
        connection_id: ConnectionId,
        present_ids: &std::collections::BTreeSet<i64>,
    ) -> Result<(), ManagerError> {
        let ids: Vec<i64> = present_ids.iter().copied().collect();
        sqlx::query(
            "UPDATE github_repositories
                SET accessible = false, updated_at = now()
              WHERE organization_id = $1 AND connection_id = $2
                AND NOT (github_repository_id = ANY($3))",
        )
        .bind(organization_id.0)
        .bind(connection_id.0)
        .bind(ids)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// List accessible repositories for an organization across its connections.
    pub async fn list_repositories(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<RepositoryRow>, ManagerError> {
        let rows = sqlx::query_as::<_, RepositoryTuple>(
            "SELECT r.organization_id, r.connection_id, r.github_repository_id, r.full_name, r.accessible, r.last_seen_at
               FROM github_repositories r JOIN github_connections c
                 ON c.id = r.connection_id AND c.organization_id = r.organization_id
              WHERE r.organization_id = $1 AND r.accessible = true AND c.status = 'active'
              ORDER BY r.github_repository_id",
        )
        .bind(organization_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(rows.into_iter().map(RepositoryRow::from).collect())
    }
}

type AttemptTuple = (
    uuid::Uuid,
    uuid::Uuid,
    uuid::Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Option<i64>,
    Option<i64>,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

impl From<AttemptTuple> for AttemptRow {
    fn from(t: AttemptTuple) -> Self {
        AttemptRow {
            id: t.0,
            organization_id: OrganizationId::from_uuid(t.1),
            initiated_by: UserId::from_uuid(t.2),
            flow_type: t.3,
            oauth_state_hash: t.4,
            setup_state_hash: t.5,
            oauth_code_verifier_ref: t.6,
            oauth_github_user_id: t.7,
            candidate_installation: t.8,
            status: t.9,
            expires_at: t.10,
            oauth_consumed_at: t.11,
            setup_consumed_at: t.12,
            consumed_at: t.13,
        }
    }
}

type ConnectionTuple = (
    uuid::Uuid,
    uuid::Uuid,
    i64,
    i64,
    i64,
    String,
    Option<String>,
    String,
    i64,
    Option<uuid::Uuid>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<String>,
);

impl From<ConnectionTuple> for ConnectionRow {
    fn from(t: ConnectionTuple) -> Self {
        ConnectionRow {
            id: ConnectionId::from_uuid(t.0),
            organization_id: OrganizationId::from_uuid(t.1),
            app_id: t.2,
            installation_id: t.3,
            github_account_id: t.4,
            account_type: t.5,
            account_login: t.6,
            status: t.7,
            access_generation: t.8,
            connected_by: t.9.map(UserId::from_uuid),
            verified_at: t.10,
            last_reconciled_at: t.11,
            access_revoked_reason: t.12,
        }
    }
}

type RepositoryTuple = (uuid::Uuid, uuid::Uuid, i64, String, bool, DateTime<Utc>);

impl From<RepositoryTuple> for RepositoryRow {
    fn from(t: RepositoryTuple) -> Self {
        RepositoryRow {
            organization_id: OrganizationId::from_uuid(t.0),
            connection_id: ConnectionId::from_uuid(t.1),
            github_repository_id: t.2,
            full_name: t.3,
            accessible: t.4,
            last_seen_at: t.5,
        }
    }
}
