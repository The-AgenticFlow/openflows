//! Tenant repository with explicit organization scope.
//!
//! Every tenant read and mutation is joined to the caller's organization scope,
//! so a bare tenant id cannot be used to read or modify another organization's
//! tenants. Pagination preserves organization scope and stable ordering.

use crate::db::tx::OrgScope;
use crate::dto::TenantDto;
use crate::error::ManagerError;
use crate::id::{GithubId, OrganizationId, TenantId};
use crate::pagination::{cursor_for, Cursor, Page, PageLimit};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// A row returned by the tenant list query.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TenantRow {
    pub id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub slug: String,
    pub github_repository_id: i64,
    pub desired_state: String,
    pub observed_state: String,
    pub fleet_size: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct TenantsRepository {
    pool: PgPool,
}

impl TenantsRepository {
    pub fn new(pool: PgPool) -> Self {
        TenantsRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create a tenant bound to a repository in the same organization. The
    /// connection and repository must belong to `scope` (enforced by the
    /// composite FK and the scoped lookups below). Returns a conflict when a
    /// live tenant already exists for the repository or slug.
    pub async fn create(
        &self,
        scope: OrgScope,
        slug: &str,
        connection_id: crate::id::ConnectionId,
        github_repository_id: GithubId,
        fleet_size: i32,
    ) -> Result<TenantId, ManagerError> {
        let mut tx = self.pool.begin().await?;
        let id = self
            .create_in_tx(
                &mut tx,
                scope,
                slug,
                connection_id,
                github_repository_id,
                fleet_size,
            )
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Use the caller's transaction so the tenant, operation, audit, outbox,
    /// and idempotency result commit or roll back together.
    pub async fn create_in_tx(
        &self,
        conn: &mut PgConnection,
        scope: OrgScope,
        slug: &str,
        connection_id: crate::id::ConnectionId,
        github_repository_id: GithubId,
        fleet_size: i32,
    ) -> Result<TenantId, ManagerError> {
        // Verify the connection and repository belong to this organization
        // before inserting, so a foreign repository id cannot be bound here.
        let owned = sqlx::query_as::<_, (i64,)>(
            "SELECT github_repository_id
               FROM github_repositories
              WHERE connection_id = $1 AND organization_id = $2
                AND github_repository_id = $3",
        )
        .bind(connection_id.0)
        .bind(scope.organization_id.0)
        .bind(github_repository_id.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(ManagerError::from)?;

        if owned.is_none() {
            return Err(ManagerError::not_found("repository"));
        }

        let id = TenantId::new();
        let result = sqlx::query(
            "INSERT INTO tenants
                (id, organization_id, slug, connection_id, github_repository_id, fleet_size, desired_state, observed_state)
             VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', 'provisioning')",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .bind(slug)
        .bind(connection_id.0)
        .bind(github_repository_id.0)
        .bind(fleet_size)
        .execute(&mut *conn)
        .await
        .map_err(|e| translate_tenant_insert(e, slug))?;

        if result.rows_affected() == 0 {
            return Err(ManagerError::Conflict("tenant not created".to_string()));
        }
        Ok(id)
    }

    /// Read a tenant by id within the given scope. Returns `None` when the
    /// tenant does not exist or belongs to a different organization.
    pub async fn get_scoped(
        &self,
        scope: OrgScope,
        id: TenantId,
    ) -> Result<Option<TenantRow>, ManagerError> {
        let row = sqlx::query_as::<_, TenantRow>(
            "SELECT id, organization_id, slug, github_repository_id, desired_state,
                    observed_state, fleet_size, created_at
               FROM tenants
              WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row)
    }

    /// List tenants within the given scope, paginated with stable ordering
    /// (`created_at, id`) and an opaque next cursor. Organization scope is part
    /// of the query, so pagination cannot leak another organization's tenants.
    pub async fn list_scoped(
        &self,
        scope: OrgScope,
        limit: PageLimit,
        cursor: Option<&Cursor>,
    ) -> Result<Page<TenantRow>, ManagerError> {
        // Keyset pagination: fetch limit+1 rows strictly after the cursor, then
        // trim and build the next cursor from the last retained row.
        let fetch = limit.get() as i64 + 1;
        let rows = match cursor {
            Some(c) => {
                let tiebreaker: uuid::Uuid = c
                    .tiebreaker
                    .parse()
                    .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))?;
                sqlx::query_as::<_, TenantRow>(
                    "SELECT id, organization_id, slug, github_repository_id, desired_state,
                            observed_state, fleet_size, created_at
                       FROM tenants
                      WHERE organization_id = $1
                        AND (created_at, id) > ($2, $3)
                      ORDER BY created_at ASC, id ASC
                      LIMIT $4",
                )
                .bind(scope.organization_id.0)
                .bind(c.created_at)
                .bind(tiebreaker)
                .bind(fetch)
                .fetch_all(&self.pool)
                .await
                .map_err(ManagerError::from)?
            }
            None => sqlx::query_as::<_, TenantRow>(
                "SELECT id, organization_id, slug, github_repository_id, desired_state,
                            observed_state, fleet_size, created_at
                       FROM tenants
                      WHERE organization_id = $1
                      ORDER BY created_at ASC, id ASC
                      LIMIT $2",
            )
            .bind(scope.organization_id.0)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
            .map_err(ManagerError::from)?,
        };

        Ok(Page::from_fetch(rows, limit, |r| {
            cursor_for(r.created_at, r.id)
        }))
    }

    /// Update a tenant's observed state, scoped. Returns false when the tenant
    /// is not in scope.
    pub async fn set_observed_state(
        &self,
        scope: OrgScope,
        id: TenantId,
        observed_state: &str,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE tenants
                SET observed_state = $1, updated_at = now()
              WHERE id = $2 AND organization_id = $3",
        )
        .bind(observed_state)
        .bind(id.0)
        .bind(scope.organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Convert a [`TenantRow`] into a public [`TenantDto`].
    pub fn to_dto(row: &TenantRow) -> TenantDto {
        TenantDto {
            id: TenantId::from_uuid(row.id),
            organization_id: OrganizationId::from_uuid(row.organization_id),
            slug: row.slug.clone(),
            github_repository_id: GithubId(row.github_repository_id),
            desired_state: row.desired_state.clone(),
            observed_state: row.observed_state.clone(),
            fleet_size: row.fleet_size,
        }
    }
}

/// Translate a tenant insert failure, mapping live-uniqueness collisions to
/// 409 with a sanitized message.
fn translate_tenant_insert(e: sqlx::Error, slug: &str) -> ManagerError {
    if let sqlx::Error::Database(db) = &e {
        match db.constraint() {
            Some("tenants_organization_id_github_repository_id_key") => {
                return ManagerError::Conflict(
                    "one live tenant already exists for this repository".to_string(),
                );
            }
            Some("tenants_organization_id_slug_key") => {
                return ManagerError::Conflict(format!(
                    "a tenant with slug '{slug}' already exists in this organization"
                ));
            }
            _ => {}
        }
    }
    ManagerError::from(e)
}
