//! Durable operations with exclusive worker leasing.
//!
//! Operations track multi-step provisioning work. Workers claim a lease on an
//! operation with `FOR UPDATE SKIP LOCKED` and a lease owner + expiry; only the
//! lease owner may heartbeat or complete the operation. When a lease expires it
//! can be reclaimed by another worker, and a stale worker that lost the lease
//! cannot complete or overwrite the reclaimed operation because every
//! completion checks the owner, unexpired lease, and unique per-claim token.

use crate::error::ManagerError;
use crate::id::{OperationId, OrganizationId};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};

/// Default operation lease and heartbeat intervals.
pub const LEASE_DURATION: Duration = Duration::seconds(30);
pub const HEARTBEAT_INTERVAL: Duration = Duration::seconds(10);

/// The lifecycle states of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    RetryableFailed,
}

impl OperationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationState::Queued => "queued",
            OperationState::Running => "running",
            OperationState::Succeeded => "succeeded",
            OperationState::Failed => "failed",
            OperationState::Cancelled => "cancelled",
            OperationState::RetryableFailed => "retryable_failed",
        }
    }
}

/// A handle to an operation leased by one worker.
///
/// The handle carries the lease owner and enforces ownership on every
/// subsequent write, so a worker whose lease was reclaimed becomes a no-op
/// rather than overwriting the reclaiming worker's progress.
#[derive(Debug, Clone)]
pub struct LeaseGuard {
    pub operation_id: OperationId,
    pub organization_id: OrganizationId,
    owner: String,
    token: uuid::Uuid,
    pool: PgPool,
}

/// New-operation input.
#[derive(Debug, Clone)]
pub struct NewOperation {
    pub organization_id: OrganizationId,
    pub resource_type: String,
    pub resource_id: Option<uuid::Uuid>,
    pub kind: String,
    pub idempotency_ref: Option<String>,
}

/// Read a single operation, scoped to its organization.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OperationRecord {
    pub id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub resource_type: String,
    pub resource_id: Option<uuid::Uuid>,
    pub kind: String,
    pub idempotency_ref: Option<String>,
    pub state: String,
    pub current_step: Option<String>,
    pub attempt_count: i32,
    pub retry_at: Option<DateTime<Utc>>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    pub sanitized_result: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a new operation.
pub async fn create(pool: &PgPool, op: &NewOperation) -> Result<OperationId, ManagerError> {
    let id = OperationId::new();
    sqlx::query(
        "INSERT INTO operations
            (id, organization_id, resource_type, resource_id, kind, idempotency_ref, state, attempt_count)
         VALUES ($1, $2, $3, $4, $5, $6, 'queued', 0)",
    )
    .bind(id.0)
    .bind(op.organization_id.0)
    .bind(&op.resource_type)
    .bind(op.resource_id)
    .bind(&op.kind)
    .bind(&op.idempotency_ref)
    .execute(pool)
    .await
    .map_err(ManagerError::from)?;
    Ok(id)
}

/// Insert a new operation inside an existing transaction (used with
/// provisioning state changes).
pub async fn create_in_tx(
    tx: &mut PgConnection,
    op: &NewOperation,
) -> Result<OperationId, ManagerError> {
    let id = OperationId::new();
    sqlx::query(
        "INSERT INTO operations
            (id, organization_id, resource_type, resource_id, kind, idempotency_ref, state, attempt_count)
         VALUES ($1, $2, $3, $4, $5, $6, 'queued', 0)",
    )
    .bind(id.0)
    .bind(op.organization_id.0)
    .bind(&op.resource_type)
    .bind(op.resource_id)
    .bind(&op.kind)
    .bind(&op.idempotency_ref)
    .execute(&mut *tx)
    .await
    .map_err(ManagerError::from)?;
    Ok(id)
}

/// Claim an available operation for `owner`.
///
/// An operation is claimable when it is not terminal (`queued`, `running`, or
/// `retryable_failed`) and its lease is unset or expired (including an expired
/// `retry_at`). This makes an operation with an expired lease recoverable by
/// another worker even if its state is still `running` from a dead worker. The
/// claim is atomic (`FOR UPDATE SKIP LOCKED`) so concurrent workers never
/// receive the same operation. Returns `None` when no operation is available.
pub async fn claim(pool: &PgPool, owner: &str) -> Result<Option<LeaseGuard>, ManagerError> {
    let token = uuid::Uuid::new_v4();
    let row = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
        "UPDATE operations
            SET lease_owner = $1,
                lease_expires_at = clock_timestamp() + interval '30 seconds',
                lease_token = $2,
                attempt_count = attempt_count + 1,
                state = 'running',
                updated_at = now()
          WHERE id = (
                SELECT id
                  FROM operations
                 WHERE state IN ('queued', 'running', 'retryable_failed')
                   AND (lease_expires_at IS NULL OR lease_expires_at < now())
                   AND (retry_at IS NULL OR retry_at <= now())
                 ORDER BY created_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
          )
          RETURNING id, organization_id",
    )
    .bind(owner)
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(ManagerError::from)?;

    Ok(row.map(|(id, org)| LeaseGuard {
        operation_id: OperationId::from_uuid(id),
        organization_id: OrganizationId::from_uuid(org),
        owner: owner.to_string(),
        token,
        pool: pool.clone(),
    }))
}

impl LeaseGuard {
    /// The worker that currently owns this lease.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Extend the lease, only if this worker still owns it.
    pub async fn heartbeat(&self) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE operations
                SET lease_expires_at = clock_timestamp() + interval '30 seconds', updated_at = now()
              WHERE lease_token = $1 AND id = $2 AND lease_owner = $3
                AND lease_expires_at > clock_timestamp()",
        )
        .bind(self.token)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Mark the operation succeeded. Only the current lease owner with a live
    /// lease can succeed it; a stale worker is rejected.
    pub async fn complete(&self, sanitized_result: Option<Value>) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE operations
                SET state = 'succeeded',
                    lease_owner = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    sanitized_result = $1,
                    error_code = NULL,
                    updated_at = now()
              WHERE id = $2 AND lease_owner = $3 AND lease_expires_at > clock_timestamp()
                AND lease_token = $4",
        )
        .bind(sanitized_result)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Mark the operation failed (terminal or retryable). Only the current
    /// lease owner may do so.
    pub async fn fail(
        &self,
        retryable: bool,
        error_code: Option<&str>,
    ) -> Result<bool, ManagerError> {
        let (state, retry_at) = if retryable {
            (
                OperationState::RetryableFailed.as_str(),
                Some(Utc::now() + Duration::seconds(1)),
            )
        } else {
            (OperationState::Failed.as_str(), None)
        };
        let affected = sqlx::query(
            "UPDATE operations
                SET state = $1,
                    retry_at = $2,
                    error_code = $3,
                    lease_owner = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    updated_at = now()
              WHERE id = $4 AND lease_owner = $5 AND lease_expires_at > clock_timestamp()
                AND lease_token = $6",
        )
        .bind(state)
        .bind(retry_at)
        .bind(error_code)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Update the current step, only if this worker still owns the lease.
    pub async fn set_step(&self, step: &str) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE operations
                SET current_step = $1, updated_at = now()
              WHERE id = $2 AND lease_owner = $3 AND lease_expires_at > clock_timestamp()
                AND lease_token = $4",
        )
        .bind(step)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }
}

/// Fetch an operation by id, requiring the org scope. Returns `None` when the
/// operation does not exist or does not belong to `organization_id`.
pub async fn get_scoped(
    pool: &PgPool,
    organization_id: OrganizationId,
    id: OperationId,
) -> Result<Option<OperationRecord>, ManagerError> {
    let row = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, organization_id, resource_type, resource_id, kind, idempotency_ref,
                state, current_step, attempt_count, retry_at, lease_owner, lease_expires_at,
                error_code, sanitized_result, created_at, updated_at
           FROM operations
          WHERE id = $1 AND organization_id = $2",
    )
    .bind(id.0)
    .bind(organization_id.0)
    .fetch_optional(pool)
    .await
    .map_err(ManagerError::from)?;
    Ok(row)
}

/// Fetch an operation only through the caller's current active membership.
pub async fn get_for_user(
    pool: &PgPool,
    id: OperationId,
    caller: crate::id::UserId,
) -> Result<Option<OperationRecord>, ManagerError> {
    let row = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, organization_id, resource_type, resource_id, kind, idempotency_ref,
                state, current_step, attempt_count, retry_at, lease_owner, lease_expires_at,
                error_code, sanitized_result, created_at, updated_at
           FROM operations
          WHERE id = $1 AND EXISTS (SELECT 1 FROM memberships m JOIN users u ON u.id=m.user_id
            JOIN organizations o ON o.id=m.organization_id
            WHERE m.organization_id=operations.organization_id AND m.user_id=$2
              AND m.status='active' AND u.status='active' AND o.status <> 'deleted')",
    )
    .bind(id.0)
    .bind(caller.0)
    .fetch_optional(pool)
    .await
    .map_err(ManagerError::from)?;
    Ok(row)
}

/// Whether an operation is still leased to `owner`.
pub async fn is_leased_to(
    pool: &PgPool,
    id: OperationId,
    owner: &str,
) -> Result<bool, ManagerError> {
    let row: (Option<String>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT lease_owner, lease_expires_at FROM operations WHERE id = $1")
            .bind(id.0)
            .fetch_one(pool)
            .await
            .map_err(ManagerError::from)?;
    Ok(row.0.as_deref() == Some(owner) && row.1.map(|e| e > Utc::now()).unwrap_or(false))
}
