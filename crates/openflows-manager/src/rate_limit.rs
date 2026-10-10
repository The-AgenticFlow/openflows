//! Independent rate limiting for authentication entry points.
//!
//! Start, code verification (device approve), and polling are bounded
//! independently so an attacker cannot exhaust one endpoint to starve another.
//! Counters are windowed and stored in PostgreSQL so they survive restarts and
//! are shared across the process.

use crate::error::ManagerError;
use sqlx::PgPool;

/// Rate-limit scopes, each bounded independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitScope {
    /// `POST /auth/github/start`.
    GithubStart,
    /// `POST /auth/cli/start`.
    DeviceStart,
    /// `POST /auth/cli/approve` (code verification).
    Verify,
    /// `POST /auth/cli/token` (polling).
    Poll,
    /// `POST /invitations/accept` and other token-verification entry points.
    Invitation,
    Refresh,
}

impl LimitScope {
    fn as_str(&self) -> &'static str {
        match self {
            LimitScope::GithubStart => "start",
            LimitScope::DeviceStart => "device_start",
            LimitScope::Verify => "verify",
            LimitScope::Poll => "poll",
            LimitScope::Invitation => "invitation",
            LimitScope::Refresh => "refresh",
        }
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    pool: PgPool,
}

// Fixed minute boundaries use the database clock across all manager instances.

impl RateLimiter {
    pub fn new(pool: PgPool) -> Self {
        RateLimiter { pool }
    }

    /// Check-and-increment an attempt for `bucket_key` in `scope`. Returns
    /// `true` when within `limit` (allowed), `false` when over (rate-limited).
    ///
    /// This is not a precise sliding window; a coarse fixed window is sufficient
    /// for abuse bounding and is cheap to maintain.
    pub async fn allow(
        &self,
        bucket_key: &str,
        scope: LimitScope,
        limit: u32,
    ) -> Result<bool, ManagerError> {
        let count: i32 = sqlx::query_scalar(
            "INSERT INTO rate_limit_ledger (bucket_key, scope, window_start, count)
             VALUES ($1, $2, date_trunc('minute', clock_timestamp()), 1)
             ON CONFLICT (bucket_key, scope, window_start) DO UPDATE
               SET count = GREATEST(rate_limit_ledger.count, LEAST(rate_limit_ledger.count::bigint + 1, $3::integer)),
                   updated_at = now()
             RETURNING count",
        )
        .bind(crate::auth::crypto::hash_token(bucket_key))
        .bind(scope.as_str())
        .bind(
            i32::try_from(limit)
                .unwrap_or(i32::MAX - 1)
                .saturating_add(1),
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        // Opportunistic bounded cleanup avoids an ever-growing minute ledger.
        if rand::random::<u8>() < 4 {
            sqlx::query("DELETE FROM rate_limit_ledger WHERE ctid IN
                (SELECT ctid FROM rate_limit_ledger WHERE window_start < clock_timestamp() - interval '1 hour'
                 ORDER BY window_start LIMIT 1000)")
                .execute(&self.pool).await.map_err(ManagerError::from)?;
        }
        Ok(i64::from(count) <= i64::from(limit))
    }
}

/// Set by the server from the socket peer, never from client-supplied forwarding headers.
pub fn peer_bucket(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-openflows-peer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown-peer")
        .to_string()
}
