//! Execute database mutations and their idempotency result in one transaction.
//!
//! The callback must use its supplied connection for all writes, and enqueue
//! external side effects through the outbox. A failure or cancellation rolls
//! back both the mutation and key, allowing a subsequent request to retry.

use crate::error::ManagerError;
use crate::id::{OrganizationId, UserId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use std::{future::Future, pin::Pin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    New { response_reference: String },
    Replay { response_reference: String },
}

#[derive(Debug, Clone)]
pub struct IdempotencyRequest {
    pub actor_id: UserId,
    pub organization_id: OrganizationId,
    pub route: String,
    pub key: String,
    pub request_hash: String,
}

impl IdempotencyRequest {
    pub fn hash_request<T: Serialize>(body: &T) -> Result<String, serde_json::Error> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(body)?)))
    }
}

/// An organization-less idempotency request, used for organization creation
/// where no organization exists yet. Keyed by `(actor_id, route, key)` with a
/// NULL organization id.
#[derive(Debug, Clone)]
pub struct OrglessIdempotencyRequest {
    pub actor_id: UserId,
    pub route: String,
    pub key: String,
    pub request_hash: String,
}

pub type MutationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<String, ManagerError>> + Send + 'a>>;

#[derive(Clone)]
pub struct IdempotencyService {
    pool: PgPool,
}

impl IdempotencyService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Read a completed, unexpired result before doing external work.
    /// Callers must check current authorization before replaying it.
    pub async fn replay(&self, req: &IdempotencyRequest) -> Result<Option<String>, ManagerError> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT request_hash, response_reference FROM idempotency_keys
             WHERE actor_id=$1 AND organization_id=$2 AND route=$3 AND key=$4
               AND expires_at > clock_timestamp()",
        )
        .bind(req.actor_id.0)
        .bind(req.organization_id.0)
        .bind(&req.route)
        .bind(&req.key)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((hash, _)) if hash != req.request_hash => Err(ManagerError::Conflict(
                "idempotency key reused with a different request body".into(),
            )),
            Some((_, reference)) => Ok(reference.filter(|r| !r.trim().is_empty())),
            None => Ok(None),
        }
    }

    /// Commit an org-scoped mutation and its nonempty result reference
    /// together. Concurrent identical requests wait on the unique key and then
    /// replay the committed result. No unfinished key is committed.
    pub async fn execute<F>(
        &self,
        req: &IdempotencyRequest,
        mutation: F,
    ) -> Result<IdempotencyOutcome, ManagerError>
    where
        F: for<'a> FnOnce(&'a mut PgConnection) -> MutationFuture<'a> + Send,
    {
        self.execute_inner(
            req.actor_id,
            Some(req.organization_id),
            &req.route,
            &req.key,
            &req.request_hash,
            mutation,
        )
        .await
    }

    /// Commit an organization-less mutation (organization creation). Keyed by
    /// `(actor_id, route, key)` with a NULL organization id.
    pub async fn execute_orgless<F>(
        &self,
        req: &OrglessIdempotencyRequest,
        mutation: F,
    ) -> Result<IdempotencyOutcome, ManagerError>
    where
        F: for<'a> FnOnce(&'a mut PgConnection) -> MutationFuture<'a> + Send,
    {
        self.execute_inner(
            req.actor_id,
            None,
            &req.route,
            &req.key,
            &req.request_hash,
            mutation,
        )
        .await
    }

    async fn execute_inner<F>(
        &self,
        actor_id: UserId,
        organization_id: Option<OrganizationId>,
        route: &str,
        key: &str,
        request_hash: &str,
        mutation: F,
    ) -> Result<IdempotencyOutcome, ManagerError>
    where
        F: for<'a> FnOnce(&'a mut PgConnection) -> MutationFuture<'a> + Send,
    {
        if key.trim().is_empty() || key.len() > 255 {
            return Err(ManagerError::InvalidInput("invalid idempotency key".into()));
        }
        let mut tx = self.pool.begin().await?;
        // Lock before the foreign-key insert takes KEY SHARE. Otherwise two
        // different keys can deadlock when their mutations request FOR UPDATE.
        if let Some(org_id) = organization_id {
            sqlx::query("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
                .bind(org_id.0)
                .fetch_optional(&mut *tx)
                .await?;
        }
        // Only an expired record can be replaced. The upsert locks conflicts
        // and re-checks expiry after acquiring the row, including on retries.
        let id = uuid::Uuid::new_v4();
        let inserted = match organization_id {
            Some(org_id) => {
                sqlx::query(
                    "INSERT INTO idempotency_keys
                        (id, actor_id, organization_id, route, key, request_hash, expires_at)
                     VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp() + interval '7 days')
                     ON CONFLICT (actor_id, organization_id, route, key) DO UPDATE
                     SET id = EXCLUDED.id, request_hash = EXCLUDED.request_hash,
                         response_reference = NULL, created_at = clock_timestamp(),
                         expires_at = EXCLUDED.expires_at
                     WHERE idempotency_keys.expires_at <= clock_timestamp()",
                )
                .bind(id)
                .bind(actor_id.0)
                .bind(org_id.0)
                .bind(route)
                .bind(key)
                .bind(request_hash)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    == 1
            }
            None => {
                // Organization-less (org creation): the partial unique index is
                // on (actor_id, route, key) WHERE organization_id IS NULL.
                sqlx::query(
                    "INSERT INTO idempotency_keys
                        (id, actor_id, organization_id, route, key, request_hash, expires_at)
                     VALUES ($1, $2, NULL, $3, $4, $5, clock_timestamp() + interval '7 days')
                     ON CONFLICT (actor_id, route, key) WHERE organization_id IS NULL DO UPDATE
                     SET id = EXCLUDED.id, request_hash = EXCLUDED.request_hash,
                         response_reference = NULL, created_at = clock_timestamp(),
                         expires_at = EXCLUDED.expires_at
                     WHERE idempotency_keys.expires_at <= clock_timestamp()",
                )
                .bind(id)
                .bind(actor_id.0)
                .bind(route)
                .bind(key)
                .bind(request_hash)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    == 1
            }
        };

        if !inserted {
            let (stored_hash, reference): (String, Option<String>) = sqlx::query_as(
                "SELECT request_hash, response_reference FROM idempotency_keys
                 WHERE actor_id = $1 AND organization_id IS NOT DISTINCT FROM $2
                   AND route = $3 AND key = $4
                 FOR UPDATE",
            )
            .bind(actor_id.0)
            .bind(organization_id.map(|o| o.0))
            .bind(route)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
            if stored_hash != request_hash {
                return Err(ManagerError::Conflict(
                    "idempotency key reused with a different request body".into(),
                ));
            }
            let reference = reference.filter(|r| !r.trim().is_empty()).ok_or_else(|| {
                ManagerError::Conflict("legacy idempotency request has no completed result".into())
            })?;
            tx.commit().await?;
            return Ok(IdempotencyOutcome::Replay {
                response_reference: reference,
            });
        }

        let reference = mutation(&mut tx).await?;
        if reference.trim().is_empty() {
            return Err(ManagerError::InvalidInput(
                "empty idempotency result reference".into(),
            ));
        }
        sqlx::query("UPDATE idempotency_keys SET response_reference = $1 WHERE id = $2")
            .bind(&reference)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(IdempotencyOutcome::New {
            response_reference: reference,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_serializer_cannot_produce_a_request_hash() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("cannot serialize"))
            }
        }
        assert!(IdempotencyRequest::hash_request(&Invalid).is_err());
    }
}
