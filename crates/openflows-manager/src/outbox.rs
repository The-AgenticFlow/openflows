//! Durable outbox for side effects.
//!
//! Outbox events are written in the same transaction as the state change that
//! triggers them and are then delivered by leased workers. Events carry an
//! organization scope, an allowlisted payload, an attempt counter, and lease
//! fields so a worker that dies mid-delivery can be recovered by another.

use crate::error::ManagerError;
use crate::id::{OrganizationId, OutboxEventId};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};

/// Default lease duration for an outbox delivery.
pub const LEASE_DURATION: Duration = Duration::seconds(30);

/// A claimed outbox event ready for delivery.
#[derive(Debug, Clone)]
pub struct OutboxClaim {
    pub id: OutboxEventId,
    pub organization_id: Option<OrganizationId>,
    pub event_type: String,
    pub payload: Option<Value>,
    pub attempts: i32,
    owner: String,
    token: uuid::Uuid,
}

/// Insert an outbox event inside a transaction so it commits with the state
/// change that produced it. A failed transaction must leave no orphaned outbox
/// write.
pub async fn insert_in_tx(
    tx: &mut PgConnection,
    organization_id: Option<OrganizationId>,
    event_type: &str,
    payload: Option<&Value>,
) -> Result<OutboxEventId, ManagerError> {
    let id = OutboxEventId::new();
    sqlx::query(
        "INSERT INTO outbox_events (id, organization_id, event_type, payload, attempts)
         VALUES ($1, $2, $3, $4, 0)",
    )
    .bind(id.0)
    .bind(organization_id.map(|o| o.0))
    .bind(event_type)
    .bind(payload)
    .execute(&mut *tx)
    .await
    .map_err(ManagerError::from)?;
    Ok(id)
}

/// Claim up to `limit` pending outbox events whose lease has expired (or never
/// been set), atomically assigning `owner` and a fresh lease. Uses
/// `FOR UPDATE SKIP LOCKED` so concurrent workers never claim the same row.
pub async fn claim(
    pool: &PgPool,
    owner: &str,
    limit: i32,
) -> Result<Vec<OutboxClaim>, ManagerError> {
    let token = uuid::Uuid::new_v4();
    let rows = sqlx::query_as::<_, (uuid::Uuid, Option<uuid::Uuid>, String, Option<Value>, i32)>(
        "UPDATE outbox_events
            SET lease_owner = $1,
                lease_expires_at = clock_timestamp() + interval '30 seconds',
                lease_token = $2,
                attempts = attempts + 1
          WHERE id IN (
                SELECT id
                  FROM outbox_events
                 WHERE delivered_at IS NULL
                   AND (lease_expires_at IS NULL OR lease_expires_at < now())
                 ORDER BY created_at
                 LIMIT $3
                 FOR UPDATE SKIP LOCKED
          )
          RETURNING id, organization_id, event_type, payload, attempts",
    )
    .bind(owner)
    .bind(token)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(ManagerError::from)?;

    Ok(rows
        .into_iter()
        .map(|(id, org, event_type, payload, attempts)| OutboxClaim {
            id: OutboxEventId::from_uuid(id),
            organization_id: org.map(OrganizationId::from_uuid),
            event_type,
            payload,
            attempts,
            owner: owner.to_string(),
            token,
        })
        .collect())
}

/// Mark a claimed event as delivered.
pub async fn mark_delivered(pool: &PgPool, claim: &OutboxClaim) -> Result<bool, ManagerError> {
    let result = sqlx::query("UPDATE outbox_events SET delivered_at = now(), lease_owner = NULL, lease_expires_at = NULL, lease_token = NULL
        WHERE id = $1 AND lease_owner = $2 AND lease_token = $3
          AND lease_expires_at > clock_timestamp() AND delivered_at IS NULL")
        .bind(claim.id.0)
        .bind(&claim.owner)
        .bind(claim.token)
        .execute(pool)
        .await
        .map_err(ManagerError::from)?;
    Ok(result.rows_affected() == 1)
}

/// Release a lease without delivering (e.g. transient failure), resetting the
/// lease so the event can be retried by another worker.
pub async fn release_lease(pool: &PgPool, claim: &OutboxClaim) -> Result<bool, ManagerError> {
    let result = sqlx::query(
        "UPDATE outbox_events
            SET lease_owner = NULL, lease_expires_at = NULL, lease_token = NULL
          WHERE id = $1 AND lease_owner = $2 AND lease_token = $3
            AND lease_expires_at > clock_timestamp() AND delivered_at IS NULL",
    )
    .bind(claim.id.0)
    .bind(&claim.owner)
    .bind(claim.token)
    .execute(pool)
    .await
    .map_err(ManagerError::from)?;
    Ok(result.rows_affected() == 1)
}

/// Count undelivered events, for diagnostics and readiness probes.
pub async fn pending_count(pool: &PgPool, owner: &str) -> Result<i64, ManagerError> {
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM outbox_events
          WHERE delivered_at IS NULL
            AND (lease_expires_at IS NULL OR lease_expires_at < now() OR lease_owner = $1)",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .map_err(ManagerError::from)?;
    Ok(row.0)
}

/// Whether an outbox event is currently leased to `owner`.
pub async fn is_leased_to(
    pool: &PgPool,
    id: OutboxEventId,
    owner: &str,
) -> Result<bool, ManagerError> {
    let row: (Option<String>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT lease_owner, lease_expires_at FROM outbox_events WHERE id = $1")
            .bind(id.0)
            .fetch_one(pool)
            .await
            .map_err(ManagerError::from)?;
    Ok(row.0.as_deref() == Some(owner) && row.1.map(|e| e > Utc::now()).unwrap_or(false))
}
