//! Central tenant quota enforcement (minimal infrastructure safeguards).
//!
//! Quotas are platform-configured ceilings stored per organization and enforced
//! centrally in the Manager, transactionally, so concurrent requests cannot
//! bypass them:
//!
//! - Maximum active tenants per organization (default 2).
//! - Maximum workers per tenant, in addition to its Nexus controller
//!   (default 2).
//! - Maximum concurrent provisioning operations per organization (default 1).
//!
//! Capacity is reserved consistently for queued/provisioning tenants and
//! workers, and released on failure or deletion. Provisioning work is *queued*
//! when the organization's execution slot is occupied (a concurrency limit,
//! not a resource-limit rejection); resource-limit violations (max tenants or
//! max workers reached) are returned as clear, structured errors. Only platform
//! operators can configure higher limits — these ceilings are operator startup
//! configuration, never a customer-facing endpoint.

use crate::config::TenantLimits;
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId};
use sqlx::{PgConnection, PgPool};

/// Structured resource-limit violation codes returned to callers.
pub mod code {
    /// The organization already has the maximum number of active tenants.
    pub const TENANT_LIMIT_REACHED: &str = "TENANT_LIMIT_REACHED";
    /// The tenant already has the maximum number of worker workspaces.
    pub const WORKER_LIMIT_REACHED: &str = "WORKER_LIMIT_REACHED";
}

/// A centralized, database-backed tenant quota service.
#[derive(Clone)]
pub struct TenantQuotaService {
    pool: PgPool,
    limits: TenantLimits,
}

impl TenantQuotaService {
    pub fn new(pool: PgPool, limits: TenantLimits) -> Self {
        TenantQuotaService { pool, limits }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn limits(&self) -> &TenantLimits {
        &self.limits
    }

    /// Ensure a per-organization quota row exists, inserting the operator
    /// defaults when absent. Called from the tenant-create transaction so every
    /// org has an explicit ceiling. Locks the org row to serialize with
    /// concurrent reservation checks.
    pub async fn ensure_org_row_in_tx(
        &self,
        conn: &mut PgConnection,
        org_id: OrganizationId,
    ) -> Result<(), ManagerError> {
        sqlx::query("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
            .bind(org_id.0)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| ManagerError::not_found("organization"))?;
        sqlx::query(
            "INSERT INTO tenant_quotas
                (organization_id, max_tenants, max_workers_per_tenant, max_concurrent_provisioning_ops)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (organization_id) DO NOTHING",
        )
        .bind(org_id.0)
        .bind(self.limits.max_tenants_per_org)
        .bind(self.limits.max_workers_per_tenant)
        .bind(self.limits.max_concurrent_provisioning_ops)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Check and reserve a tenant slot within `conn`. Locks the quota row for
    /// the organization and counts active (non-deleted, non-deleting) tenants;
    /// if the ceiling is already reached it returns a structured
    /// resource-limit error. The caller holds the tenant insert in the same
    /// transaction, so concurrent requests are serialized and cannot exceed the
    /// ceiling.
    pub async fn check_and_reserve_tenant_in_tx(
        &self,
        conn: &mut PgConnection,
        org_id: OrganizationId,
    ) -> Result<(), ManagerError> {
        let limits: Option<(i32,)> = sqlx::query_as(
            "SELECT max_tenants FROM tenant_quotas WHERE organization_id = $1 FOR UPDATE",
        )
        .bind(org_id.0)
        .fetch_optional(&mut *conn)
        .await?;
        let limits = limits.ok_or_else(|| ManagerError::not_found("organization"))?;

        let active: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM tenants
              WHERE organization_id = $1
                AND desired_state IN ('provisioning', 'running', 'paused')",
        )
        .bind(org_id.0)
        .fetch_one(&mut *conn)
        .await?;

        if active.0 >= i64::from(limits.0) {
            return Err(ManagerError::api(
                code::TENANT_LIMIT_REACHED,
                format!(
                    "this organization has reached its limit of {} active tenants",
                    limits.0
                ),
            ));
        }
        Ok(())
    }

    /// Reserve `additional` worker slots for a tenant within `conn`. Counts
    /// existing non-deleted worker workspaces plus the tenant's already-reserved
    /// worker capacity against the per-org worker ceiling. The Nexus controller
    /// is a fixed slot and is never counted against the worker fleet ceiling.
    pub async fn reserve_worker_capacity_in_tx(
        &self,
        conn: &mut PgConnection,
        org_id: OrganizationId,
        tenant_id: TenantId,
        additional: i32,
    ) -> Result<(), ManagerError> {
        let worker_limits: Option<(i32,)> = sqlx::query_as(
            "SELECT max_workers_per_tenant FROM tenant_quotas WHERE organization_id = $1 FOR UPDATE",
        )
        .bind(org_id.0)
        .fetch_optional(&mut *conn)
        .await?;
        let worker_limits = worker_limits.ok_or_else(|| ManagerError::not_found("organization"))?;

        let (existing, reserved): (i64, i32) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM workspaces
                  WHERE tenant_id = $2 AND role <> 'nexus'
                    AND desired_state <> 'deleted'),
                reserved_worker_capacity
             FROM tenants WHERE id = $2 AND organization_id = $1 FOR UPDATE",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_one(&mut *conn)
        .await?;

        // A reservation is accompanied by a workspace row in the same
        // transaction. Count the larger representation so queued work is not
        // charged twice while still covering reservations made before a row is
        // visible.
        let used = existing.max(i64::from(reserved)) + i64::from(additional);
        if used > i64::from(worker_limits.0) {
            return Err(ManagerError::api(
                code::WORKER_LIMIT_REACHED,
                format!(
                    "this tenant has reached its limit of {} workers",
                    worker_limits.0
                ),
            ));
        }

        sqlx::query(
            "UPDATE tenants SET reserved_worker_capacity = reserved_worker_capacity + $1,
                updated_at = now() WHERE id = $2 AND organization_id = $3",
        )
        .bind(additional)
        .bind(tenant_id.0)
        .bind(org_id.0)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Release previously reserved worker capacity on failure or deletion.
    pub async fn release_worker_capacity_in_tx(
        &self,
        conn: &mut PgConnection,
        org_id: OrganizationId,
        tenant_id: TenantId,
        amount: i32,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "UPDATE tenants SET reserved_worker_capacity = GREATEST(reserved_worker_capacity - $1, 0),
                updated_at = now() WHERE id = $2 AND organization_id = $3",
        )
        .bind(amount)
        .bind(tenant_id.0)
        .bind(org_id.0)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Whether the organization has an available provisioning-execution slot.
    /// This is a concurrency limit (not a resource-limit rejection): when the
    /// slot is occupied, provisioning work is queued and picked up later. The
    /// slot is the count of active tenant operations below the ceiling.
    pub async fn provisioning_slot_available(
        &self,
        org_id: OrganizationId,
    ) -> Result<bool, ManagerError> {
        self.provisioning_slot_available_for(org_id, None).await
    }

    pub async fn provisioning_slot_available_for(
        &self,
        org_id: OrganizationId,
        current_operation: Option<crate::id::OperationId>,
    ) -> Result<bool, ManagerError> {
        let max: (i32,) = sqlx::query_as(
            "SELECT max_concurrent_provisioning_ops FROM tenant_quotas WHERE organization_id = $1",
        )
        .bind(org_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(ManagerError::from)?;

        // Queued and retryable failures do not consume execution capacity.
        // Only a live lease represents a worker actively performing an
        // external provisioning operation.
        let active: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM operations
              WHERE organization_id = $1
                AND ($2::uuid IS NULL OR id <> $2)
                AND state = 'running'
                AND lease_expires_at > clock_timestamp()
                AND kind IN ('tenant.provision', 'tenant.delete', 'tenant.clean')",
        )
        .bind(org_id.0)
        .bind(current_operation.map(|id| id.0))
        .fetch_one(&self.pool)
        .await?;

        Ok(active.0 < i64::from(max.0))
    }
}
