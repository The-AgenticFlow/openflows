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
pub const MAX_AUTOMATIC_ATTEMPTS: i32 = 8;

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
    claim_kind(pool, owner, None).await
}

/// Claim only operations of `kind` when provided. Specialized workers must use
/// this filter so they cannot consume another worker's operation.
pub async fn claim_kind(
    pool: &PgPool,
    owner: &str,
    kind: Option<&str>,
) -> Result<Option<LeaseGuard>, ManagerError> {
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
                 WHERE (state IN ('queued', 'running')
                        OR (state = 'retryable_failed' AND retry_at IS NOT NULL))
                   AND (lease_expires_at IS NULL OR lease_expires_at < now())
                   AND (retry_at IS NULL OR retry_at <= now())
                   AND ($3::text IS NULL OR kind = $3)
                 ORDER BY created_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
          )
          RETURNING id, organization_id",
    )
    .bind(owner)
    .bind(token)
    .bind(kind)
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

/// Claim one operation from a set of kinds while reserving the organization's
/// provisioning slot in the same database transaction. The advisory lock is
/// scoped to the organization, so separate Manager instances cannot both pass
/// the quota check before either one records its lease. Tenant deletion is a
/// finalizer: after normal attempts are exhausted, an unresolved deletion is
/// eligible again after five minutes while its tenant is still `deleting`.
pub async fn claim_kinds_with_org_slot(
    pool: &PgPool,
    owner: &str,
    kinds: &[&str],
) -> Result<Option<LeaseGuard>, ManagerError> {
    if kinds.is_empty() {
        return Ok(None);
    }

    let candidates = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
        "SELECT id, organization_id
           FROM (
             SELECT DISTINCT ON (organization_id) id, organization_id, created_at
               FROM operations
              WHERE (state IN ('queued', 'running')
                 OR (state = 'retryable_failed' AND retry_at IS NOT NULL)
                 OR (kind = 'tenant.delete' AND state = 'retryable_failed'
                     AND retry_at IS NULL AND updated_at < now() - interval '5 minutes'
                     AND EXISTS (SELECT 1 FROM tenants t WHERE t.id = operations.resource_id
                                 AND t.organization_id = operations.organization_id
                                 AND t.desired_state = 'deleting')))
                AND (lease_expires_at IS NULL OR lease_expires_at < now())
                AND (retry_at IS NULL OR retry_at <= now())
                AND kind = ANY($1::text[])
              ORDER BY organization_id,
                       CASE WHEN kind = 'tenant.delete' THEN 0 ELSE 1 END,
                       created_at
           ) candidates
          ORDER BY created_at",
    )
    .bind(kinds)
    .fetch_all(pool)
    .await
    .map_err(ManagerError::from)?;

    for (operation_id, organization_id) in candidates {
        let token = uuid::Uuid::new_v4();
        let mut tx = pool.begin().await.map_err(ManagerError::from)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(organization_id)
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;

        let candidate = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
            "SELECT id, organization_id
               FROM operations
              WHERE id = $1
                AND (state IN ('queued', 'running')
                     OR (state = 'retryable_failed' AND retry_at IS NOT NULL)
                     OR (kind = 'tenant.delete' AND state = 'retryable_failed'
                         AND retry_at IS NULL AND updated_at < now() - interval '5 minutes'
                         AND EXISTS (SELECT 1 FROM tenants t WHERE t.id = operations.resource_id
                                     AND t.organization_id = operations.organization_id
                                     AND t.desired_state = 'deleting')))
                AND (lease_expires_at IS NULL OR lease_expires_at < now())
                AND (retry_at IS NULL OR retry_at <= now())
                AND kind = ANY($2::text[])
              FOR UPDATE",
        )
        .bind(operation_id)
        .bind(kinds)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        let Some((operation_id, organization_id)) = candidate else {
            tx.commit().await.map_err(ManagerError::from)?;
            continue;
        };

        let max: i32 = sqlx::query_scalar(
            "SELECT max_concurrent_provisioning_ops
               FROM tenant_quotas
              WHERE organization_id = $1
              FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?
        .unwrap_or(1);
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM operations
              WHERE organization_id = $1
                AND id <> $2
                AND state = 'running'
                AND lease_expires_at > clock_timestamp()
                AND kind = ANY($3::text[])",
        )
        .bind(organization_id)
        .bind(operation_id)
        .bind(kinds)
        .fetch_one(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        if active >= i64::from(max) {
            tx.commit().await.map_err(ManagerError::from)?;
            continue;
        }

        sqlx::query(
            "UPDATE operations
                SET lease_owner = $1,
                    lease_expires_at = clock_timestamp() + interval '30 seconds',
                    lease_token = $2,
                    attempt_count = attempt_count + 1,
                    state = 'running',
                    updated_at = now()
              WHERE id = $3",
        )
        .bind(owner)
        .bind(token)
        .bind(operation_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        tx.commit().await.map_err(ManagerError::from)?;

        return Ok(Some(LeaseGuard {
            operation_id: OperationId::from_uuid(operation_id),
            organization_id: OrganizationId::from_uuid(organization_id),
            owner: owner.to_string(),
            token,
            pool: pool.clone(),
        }));
    }

    Ok(None)
}

impl LeaseGuard {
    /// Lock the lease row inside a resource transaction. Callers must keep
    /// this transaction short and never perform external calls while holding it.
    pub async fn lock_in_tx(&self, tx: &mut PgConnection) -> Result<bool, ManagerError> {
        let id: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM operations WHERE id = $1 AND organization_id = $2
             AND lease_owner = $3 AND lease_token = $4 AND state = 'running'
             AND lease_expires_at > clock_timestamp() FOR UPDATE",
        )
        .bind(self.operation_id.0)
        .bind(self.organization_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .fetch_optional(tx)
        .await?;
        Ok(id.is_some())
    }

    /// Complete in the transaction that persists the final resource state.
    pub async fn complete_in_tx(
        &self,
        tx: &mut PgConnection,
        result: Value,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE operations SET state = 'succeeded',
            lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
            sanitized_result = $1, error_code = NULL, retry_at = NULL, updated_at = now()
            WHERE id = $2 AND lease_owner = $3 AND lease_token = $4
            AND state = 'running' AND lease_expires_at > clock_timestamp()",
        )
        .bind(result)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .execute(tx)
        .await?
        .rows_affected();
        Ok(affected == 1)
    }

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

    /// Return an operation to the queue without consuming an execution
    /// attempt. This is used when a per-organization concurrency slot is
    /// temporarily occupied by another live operation.
    pub async fn requeue(&self, waiting_step: &str) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE operations
                SET state = 'queued',
                    current_step = $1,
                    attempt_count = GREATEST(attempt_count - 1, 0),
                    retry_at = NULL,
                    lease_owner = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    updated_at = now()
              WHERE id = $2 AND lease_owner = $3 AND lease_token = $4
                AND state = 'running' AND lease_expires_at > clock_timestamp()",
        )
        .bind(waiting_step)
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
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
        let attempts: i32 = sqlx::query_scalar(
            "SELECT attempt_count FROM operations
              WHERE id = $1 AND lease_owner = $2 AND lease_token = $3",
        )
        .bind(self.operation_id.0)
        .bind(&self.owner)
        .bind(self.token)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(MAX_AUTOMATIC_ATTEMPTS);
        let (state, retry_at) = if retryable {
            let retry_at =
                (attempts < MAX_AUTOMATIC_ATTEMPTS).then(|| Utc::now() + retry_delay(attempts));
            (OperationState::RetryableFailed.as_str(), retry_at)
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

fn retry_delay(attempt: i32) -> Duration {
    let exponent = attempt.clamp(1, 6) as u32;
    Duration::seconds((1_i64 << exponent).min(60))
}

/// Requeue a retryable operation, an organization import failure created
/// before bounded import recovery, or a failed template promotion after its
/// operator has corrected the underlying Coder configuration.
/// A live lease cannot be stolen from an active worker; the adapter retains its
/// per-release import attempt limit even when the operation is manually retried.
pub async fn retry_in_tx(
    conn: &mut PgConnection,
    id: OperationId,
    organization_id: OrganizationId,
) -> Result<(), ManagerError> {
    let affected = sqlx::query(
        "UPDATE operations
            SET state = 'queued', retry_at = NULL, lease_owner = NULL,
                lease_token = NULL, lease_expires_at = NULL, error_code = NULL,
                attempt_count = 0, updated_at = now()
          WHERE id = $1 AND organization_id = $2
            AND (state = 'retryable_failed'
                 OR (state = 'failed' AND kind = 'org.provision'
                     AND error_code = 'IMPORT_FAILED')
                 OR (state = 'failed' AND kind = 'template.promote'))
            AND (lease_expires_at IS NULL OR lease_expires_at < clock_timestamp())",
    )
    .bind(id.0)
    .bind(organization_id.0)
    .execute(conn)
    .await?
    .rows_affected();
    if affected != 1 {
        return Err(ManagerError::Conflict(
            "operation is not available for retry".into(),
        ));
    }
    Ok(())
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
