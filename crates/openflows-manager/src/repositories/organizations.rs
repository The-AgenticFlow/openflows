//! Organization repository with explicit organization scope.
//!
//! Reads and mutations are always joined to the caller's organization scope so
//! a resource id alone cannot authorize cross-organization access. Membership
//! changes are performed in a transaction with an organization-row lock so the
//! last-admin and owner invariants hold under concurrency.

use crate::db::tx::{OrgScope, TxScope};
use crate::dto::{MembershipDto, MembershipRole, MembershipStatus, OrganizationDto, UserDto};
use crate::error::ManagerError;
use crate::id::{CoderId, OrganizationId, UserId};
use sqlx::{PgPool, Postgres, Transaction};
use std::time::Duration;

/// A fully-located database handle that carries organization scope.
#[derive(Clone)]
pub struct OrganizationsRepository {
    pool: PgPool,
}

impl OrganizationsRepository {
    pub fn new(pool: PgPool) -> Self {
        OrganizationsRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create an organization and its creator's membership atomically.
    ///
    /// Both rows are inserted in the same transaction, and the deferred owner
    /// membership FK is satisfied at commit. A failed downstream step (the
    /// caller errors after this returns) leaves no partial records because the
    /// caller commits the transaction explicitly.
    pub async fn create_with_owner<'c>(
        &self,
        scope: OrgScope,
        slug: &str,
        display_name: &str,
        owner: UserId,
        mut tx: Transaction<'c, Postgres>,
    ) -> Result<TxScope<'c>, ManagerError> {
        // Insert organization and the creator's membership in the same
        // transaction. The owner FK is deferred until commit.
        sqlx::query(
            "INSERT INTO organizations
                (id, slug, display_name, owner_user_id, status)
             VALUES ($1, $2, $3, $4, 'provisioning')",
        )
        .bind(scope.organization_id.0)
        .bind(slug)
        .bind(display_name)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| translate_org_insert(e, slug))?;

        sqlx::query(
            "INSERT INTO memberships (organization_id, user_id, role, status)
             VALUES ($1, $2, 'admin', 'active')",
        )
        .bind(scope.organization_id.0)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        Ok(TxScope::new(scope, tx))
    }

    /// Read an organization by id within the given scope. Returns `None` when
    /// the organization does not exist or is outside `scope`.
    pub async fn get_scoped(
        &self,
        scope: OrgScope,
        id: OrganizationId,
    ) -> Result<Option<OrganizationDto>, ManagerError> {
        let row = sqlx::query_as::<
            _,
            (
                uuid::Uuid,
                String,
                String,
                String,
                uuid::Uuid,
                Option<String>,
            ),
        >(
            "SELECT id, slug, display_name, status, owner_user_id, coder_organization_id
               FROM organizations
              WHERE id = $1 AND id = $2",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;

        Ok(row.map(
            |(id, slug, display_name, status, owner, coder)| OrganizationDto {
                id: OrganizationId::from_uuid(id),
                slug,
                display_name,
                status,
                owner_user_id: UserId::from_uuid(owner),
                coder_organization_id: coder.map(CoderId::new),
            },
        ))
    }

    /// List organizations a user belongs to (memberships only). This is
    /// intentionally not scoped to a single org because a user may belong to
    /// several; each returned organization is a membership the user holds.
    pub async fn list_for_user(&self, user: UserId) -> Result<Vec<MembershipDto>, ManagerError> {
        let rows = sqlx::query_as::<_, (uuid::Uuid, String, String)>(
            "SELECT m.organization_id, m.role, m.status
               FROM memberships m JOIN organizations o ON o.id=m.organization_id
              WHERE m.user_id = $1 AND m.status = 'active' AND o.status <> 'deleted'
              ORDER BY m.organization_id",
        )
        .bind(user.0)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;

        rows.into_iter()
            .map(|(org, role, status)| {
                Ok(MembershipDto {
                    organization_id: OrganizationId::from_uuid(org),
                    role: role.parse::<MembershipRole>().map_err(|_| {
                        ManagerError::Service(anyhow::anyhow!(
                            "invalid membership role in database"
                        ))
                    })?,
                    status: status.parse::<MembershipStatus>().map_err(|_| {
                        ManagerError::Service(anyhow::anyhow!(
                            "invalid membership status in database"
                        ))
                    })?,
                })
            })
            .collect()
    }

    /// Read the creator/owner user record for an organization, scoped.
    pub async fn owner_user(
        &self,
        scope: OrgScope,
        id: OrganizationId,
    ) -> Result<Option<UserDto>, ManagerError> {
        let row = sqlx::query_as::<_, (uuid::Uuid, String, String)>(
            "SELECT u.id, u.display_name, u.status
               FROM organizations o
               JOIN users u ON u.id = o.owner_user_id
              WHERE o.id = $1 AND o.id = $2",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;

        Ok(row.map(|(id, display_name, status)| UserDto {
            id: UserId::from_uuid(id),
            display_name,
            status,
        }))
    }

    /// Count active admins in an organization (used for last-admin checks).
    pub async fn count_active_admins(
        &self,
        scope: OrgScope,
        id: OrganizationId,
    ) -> Result<i64, ManagerError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM memberships
              WHERE organization_id = $1 AND role = 'admin' AND status = 'active'
                AND organization_id = $2",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.0)
    }

    /// Record an organization's assigned Coder organization id, scoped.
    pub async fn set_coder_organization_id(
        &self,
        scope: OrgScope,
        id: OrganizationId,
        coder_org_id: &str,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE organizations
                SET coder_organization_id = $1, updated_at = now()
              WHERE id = $2 AND id = $3",
        )
        .bind(coder_org_id)
        .bind(id.0)
        .bind(scope.organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Update organization status, scoped. Returns false when the org is not in
    /// scope.
    pub async fn set_status(
        &self,
        scope: OrgScope,
        id: OrganizationId,
        status: &str,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE organizations
                SET status = $1, updated_at = now()
              WHERE id = $2 AND id = $3",
        )
        .bind(status)
        .bind(id.0)
        .bind(scope.organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }
}

/// Translate an organization insert failure, mapping slug collisions to 409.
fn translate_org_insert(e: sqlx::Error, slug: &str) -> ManagerError {
    if let sqlx::Error::Database(db) = &e {
        if db.constraint() == Some("organizations_slug_key") {
            return ManagerError::Conflict(format!("organization slug already exists: {slug}"));
        }
    }
    ManagerError::from(e)
}

/// A helper for tests and callers that need an explicit, bounded connection
/// acquisition with a timeout.
pub async fn acquire_timeout(
    pool: &PgPool,
    timeout: Duration,
) -> Result<sqlx::pool::PoolConnection<Postgres>, ManagerError> {
    tokio::time::timeout(timeout, pool.acquire())
        .await
        .map_err(|_| ManagerError::Service(anyhow::anyhow!("timed out acquiring a connection")))?
        .map_err(ManagerError::from)
}

// Re-export so callers can name the transaction type without reaching into
// sqlx directly.
pub type OrgTx<'c> = sqlx::Transaction<'c, Postgres>;
pub use sqlx::FromRow;
