//! Openflows CLI device-approval flow (an Openflows-managed flow, not a claim
//! to be a general OAuth authorization server).
//!
//! Flow (per 01-user-management.md §4):
//!   1. `start`: generate a 256-bit device secret and a short human code, store
//!      only their hashes, and return the raw values (device secret to the CLI,
//!      human code + verification URL to the user) once.
//!   2. `approve`: an authenticated browser user explicitly approves the
//!      displayed human code via POST with CSRF protection. A GET must never
//!      approve.
//!   3. `token`: the CLI polls with its device secret. Returns pending,
//!      expired, or a one-time credential delivery (the credential pair is
//!      delivered once and the request marked consumed).

use crate::auth::crypto::{hash_token, Secret};
use crate::auth::repository::SessionsRepository;
use crate::error::ManagerError;
use crate::id::UserId;
use sqlx::PgPool;

/// The verification code format: 8 characters with a hyphen in the middle
/// (e.g. `WDJB-MJHT`), matching GitHub's human-code style.
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// The short human code is 8 characters, split 4-4 by a hyphen.
pub fn format_user_code(code: &str) -> String {
    let mut out = String::with_capacity(9);
    for (i, c) in code.chars().enumerate() {
        if i == 4 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

/// A started device request, returned to the CLI exactly once.
#[derive(Clone)]
pub struct DeviceRequest {
    pub device_secret: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in_seconds: i64,
    pub polling_interval_seconds: i64,
}

#[derive(Clone)]
pub struct DeviceFlowService {
    pool: PgPool,
    sessions: SessionsRepository,
    public_url: String,
}

impl DeviceFlowService {
    pub fn new(pool: PgPool, sessions: SessionsRepository, public_url: String) -> Self {
        DeviceFlowService {
            pool,
            sessions,
            public_url,
        }
    }

    fn generate_human_code() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut code = String::with_capacity(8);
        for _ in 0..8 {
            let idx = rng.gen_range(0..USER_CODE_ALPHABET.len());
            code.push(USER_CODE_ALPHABET[idx] as char);
        }
        code
    }

    /// Start a device request. Stores hashes only; returns the raw values once.
    pub async fn start(&self) -> Result<DeviceRequest, ManagerError> {
        let device_secret = Secret::generate();
        let user_code = Self::generate_human_code();
        let id = uuid::Uuid::new_v4();

        sqlx::query(
            "INSERT INTO cli_login_requests
                (id, device_secret_hash, user_code_hash, status, expires_at)
             VALUES ($1, $2, $3, 'pending', clock_timestamp() + interval '10 minutes')",
        )
        .bind(id)
        .bind(device_secret.hash())
        .bind(hash_token(&user_code))
        .execute(&self.pool)
        .await
        .map_err(translate_device_insert)?;

        Ok(DeviceRequest {
            device_secret: device_secret.encode(),
            user_code: format_user_code(&user_code),
            verification_url: format!("{}/auth/cli/verify", self.public_url),
            expires_in_seconds: 600,
            polling_interval_seconds: 5,
        })
    }

    /// Approve a pending request by its human code. Called only from a POST
    /// handler after CSRF validation with an authenticated browser user. The
    /// human code is looked up by hash; rate limits bound verification attempts.
    /// Returns an error when the code is unknown, expired, or already handled.
    pub async fn approve(&self, user_code: &str, approver: UserId) -> Result<(), ManagerError> {
        self.approve_authenticated(user_code, approver, chrono::Utc::now())
            .await
    }

    pub async fn approve_authenticated(
        &self,
        user_code: &str,
        approver: UserId,
        authenticated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ManagerError> {
        // Normalize the hyphenated display form back to the 8-char code.
        let code = user_code.trim().to_ascii_uppercase().replace('-', "");
        let code_hash = hash_token(&code);

        // Atomically approve only a pending, unexpired request.
        let affected = sqlx::query(
            "UPDATE cli_login_requests
                SET status = 'approved', approved_user_id = $1, approved_authenticated_at = $3
              WHERE user_code_hash = $2 AND status = 'pending' AND expires_at > now()",
        )
        .bind(approver.0)
        .bind(&code_hash)
        .bind(authenticated_at)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();

        if affected == 0 {
            return Err(ManagerError::api(
                "DEVICE_CODE_INVALID",
                "verification code is invalid or expired",
            ));
        }
        Ok(())
    }

    /// Poll for a credential by device secret. One-time delivery.
    ///
    /// Returns:
    ///   `Poll::Pending`     - not yet approved.
    ///   `Poll::Expired`     - the request expired or was consumed before
    ///                          delivery.
    ///   `Poll::Denied`      - the request was rejected (reserved).
    ///   `Poll::Credentials` - the CLI session credential pair, delivered once.
    pub async fn poll(&self, device_secret: &str) -> Result<Poll, ManagerError> {
        let hash = hash_token(device_secret);
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;

        let row = sqlx::query_as::<_, (String, Option<uuid::Uuid>, bool, bool, Option<chrono::DateTime<chrono::Utc>>)>(
            "SELECT status, approved_user_id, expires_at <= clock_timestamp(),
                    COALESCE(last_poll_at > clock_timestamp() - interval '5 seconds', false), approved_authenticated_at
               FROM cli_login_requests
              WHERE device_secret_hash = $1
              FOR UPDATE",
        )
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        let Some((status, approved_user_id, expired, too_soon, authenticated_at)) = row else {
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok(Poll::Expired);
        };

        if expired {
            sqlx::query("UPDATE cli_login_requests SET status='expired' WHERE device_secret_hash=$1 AND status <> 'consumed'")
                .bind(&hash).execute(&mut *tx).await.map_err(ManagerError::from)?;
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok(Poll::Expired);
        }
        if status == "consumed" || status == "expired" {
            return Ok(Poll::Expired);
        }
        if too_soon {
            return Err(ManagerError::api(
                "RATE_LIMITED",
                "wait five seconds before polling again",
            )
            .retryable(true));
        }
        match status.as_str() {
            "pending" => {
                // Record the poll (rate-limit boundary) and keep waiting.
                sqlx::query(
                    "UPDATE cli_login_requests SET last_poll_at = now() WHERE device_secret_hash = $1",
                )
                .bind(&hash)
                .execute(&mut *tx)
                .await
                .map_err(ManagerError::from)?;
                tx.commit().await.map_err(ManagerError::from)?;
                Ok(Poll::Pending)
            }
            "approved" => {
                let (Some(approver), Some(authenticated_at)) = (approved_user_id, authenticated_at)
                else {
                    tx.commit().await.map_err(ManagerError::from)?;
                    return Ok(Poll::Expired);
                };
                // Atomically transition to consumed so only one poll delivers.
                let delivered = sqlx::query(
                    "UPDATE cli_login_requests
                        SET status = 'consumed'
                      WHERE device_secret_hash = $1 AND status = 'approved'",
                )
                .bind(&hash)
                .execute(&mut *tx)
                .await
                .map_err(ManagerError::from)?
                .rows_affected();

                if delivered == 0 {
                    // Another poll won the single delivery race.
                    tx.commit().await.map_err(ManagerError::from)?;
                    return Ok(Poll::Expired);
                }

                let creds = self
                    .sessions
                    .create_cli_session_in_tx(&mut tx, UserId::from_uuid(approver))
                    .await?;
                sqlx::query("UPDATE sessions SET last_authenticated_at=$1 WHERE id=$2")
                    .bind(authenticated_at)
                    .bind(creds.session_id.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(ManagerError::from)?;
                tx.commit().await.map_err(ManagerError::from)?;
                Ok(Poll::Credentials(creds))
            }
            "consumed" | "expired" => {
                tx.commit().await.map_err(ManagerError::from)?;
                Ok(Poll::Expired)
            }
            _ => {
                tx.commit().await.map_err(ManagerError::from)?;
                Ok(Poll::Expired)
            }
        }
    }

    /// Verify that a device request is pending (for the verification page).
    pub async fn is_pending_code(&self, user_code: &str) -> Result<bool, ManagerError> {
        let code = user_code.replace('-', "");
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT count(*) FROM cli_login_requests
              WHERE user_code_hash = $1 AND status = 'pending' AND expires_at > now()",
        )
        .bind(hash_token(&code))
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(|(c,)| c > 0).unwrap_or(false))
    }
}

fn translate_device_insert(e: sqlx::Error) -> ManagerError {
    ManagerError::from(e)
}

/// The result of polling the device flow for credentials.
#[derive(Debug, Clone)]
pub enum Poll {
    Pending,
    Expired,
    Denied,
    Credentials(crate::auth::repository::SessionCredentials),
}

impl std::fmt::Debug for DeviceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceRequest([REDACTED])")
    }
}
