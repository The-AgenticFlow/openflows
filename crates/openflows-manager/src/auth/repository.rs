//! Persistence for authentication: users, external identities, sessions,
//! refresh history, OAuth transactions, and CLI device requests.
//!
//! All secret-bearing values are stored as SHA-256 hashes; recoverable OAuth
//! material (PKCE verifier, retained user tokens) is stored encrypted with a
//! key version. Provider access tokens are discarded after ordinary login and
//! never persisted as plaintext application values.

use crate::error::ManagerError;
use crate::id::{SessionId, UserId};
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Human users and their external provider identities.
#[derive(Clone)]
pub struct UsersRepository {
    pool: PgPool,
}

impl UsersRepository {
    pub fn new(pool: PgPool) -> Self {
        UsersRepository { pool }
    }

    /// Look up a user by provider subject (GitHub numeric id). Returns the
    /// user id and status. `None` when the identity is unknown.
    pub async fn find_by_provider_subject(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<(UserId, String)>, ManagerError> {
        let row = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT u.id, u.status
               FROM identities i
               JOIN users u ON u.id = i.user_id
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(|(id, status)| (UserId::from_uuid(id), status)))
    }

    /// Atomically create a user and its provider identity, or return the
    /// existing user if the identity already exists. The login snapshot is
    /// stored for display only; identity matching uses `(provider, subject)`.
    pub async fn find_or_create_user(
        &self,
        pool: &PgPool,
        provider: &str,
        subject: &str,
        login_snapshot: &str,
        display_name: &str,
    ) -> Result<(UserId, bool), ManagerError> {
        let mut tx = pool.begin().await.map_err(ManagerError::from)?;
        // Serialize even when the identity row does not exist yet.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("identity:{provider}:{subject}"))
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM identities WHERE provider = $1 AND subject = $2 FOR UPDATE",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        if let Some(user_id) = existing {
            // Refresh the login snapshot (a snapshot, never identity).
            sqlx::query(
                "UPDATE identities SET login_snapshot = $1, updated_at = now()
                  WHERE provider = $2 AND subject = $3",
            )
            .bind(login_snapshot)
            .bind(provider)
            .bind(subject)
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok((UserId::from_uuid(user_id), false));
        }

        let user_id = UserId::new();
        let identity_id = crate::id::IdentityId::new();
        sqlx::query("INSERT INTO users (id, display_name, status) VALUES ($1, $2, 'active')")
            .bind(user_id.0)
            .bind(display_name)
            .execute(&mut *tx)
            .await
            .map_err(translate_user_insert)?;
        sqlx::query(
            "INSERT INTO identities (id, user_id, provider, subject, login_snapshot)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(identity_id.0)
        .bind(user_id.0)
        .bind(provider)
        .bind(subject)
        .bind(login_snapshot)
        .execute(&mut *tx)
        .await
        .map_err(translate_identity_insert)?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok((user_id, true))
    }

    /// Load a user's current status. Returns `None` when the user does not
    /// exist.
    pub async fn user_status(&self, user_id: UserId) -> Result<Option<String>, ManagerError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT status FROM users WHERE id = $1")
            .bind(user_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(ManagerError::from)?;
        Ok(row.map(|(s,)| s))
    }

    /// Load a user's display name and status. Returns `None` when the user does
    /// not exist.
    pub async fn user_dto(
        &self,
        user_id: UserId,
    ) -> Result<Option<crate::dto::UserDto>, ManagerError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT display_name, status FROM users WHERE id = $1")
                .bind(user_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.map(|(display_name, status)| crate::dto::UserDto {
            id: user_id,
            display_name,
            status,
        }))
    }

    /// Resolve the user's GitHub numeric subject id (as a string) for the
    /// "github" provider. Used to verify an invited user's exact identity on
    /// invitation acceptance. Returns `None` when the user has no GitHub
    /// identity (should not happen for a signed-in user).
    pub async fn github_subject(&self, user_id: UserId) -> Result<Option<i64>, ManagerError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT subject FROM identities WHERE user_id = $1 AND provider = 'github'",
        )
        .bind(user_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            Some((subject,)) => Ok(subject.parse::<i64>().ok()),
            None => Ok(None),
        }
    }
}

fn translate_user_insert(e: sqlx::Error) -> ManagerError {
    ManagerError::Service(anyhow::anyhow!("failed to create user: {e}"))
}

fn translate_identity_insert(_e: sqlx::Error) -> ManagerError {
    // A concurrent insert may have won the (provider, subject) race; the
    // caller should retry the lookup rather than treat it as a hard error.
    ManagerError::Conflict("identity already exists".to_string())
}

/// OAuth login transactions (state + encrypted PKCE verifier + cookie binding)
/// and CLI device-approval requests.
#[derive(Clone)]
pub struct AuthTransactionRepository {
    pool: PgPool,
}

impl AuthTransactionRepository {
    pub fn new(pool: PgPool) -> Self {
        AuthTransactionRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Insert a login OAuth transaction. The `state` is stored hashed; the
    /// PKCE verifier is stored encrypted with an explicit key version.
    pub async fn create_login(
        &self,
        state_hash: &str,
        encrypted_verifier: &[u8],
        verifier_version: i32,
    ) -> Result<Uuid, ManagerError> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO auth_transactions
                (id, purpose, state_hash, encrypted_pkce_verifier, pkce_key_version, expires_at)
             VALUES ($1, 'login', $2, $3, $4, clock_timestamp() + interval '10 minutes')",
        )
        .bind(id)
        .bind(state_hash)
        .bind(encrypted_verifier)
        .bind(verifier_version)
        .execute(&self.pool)
        .await
        .map_err(translate_state_insert)?;
        Ok(id)
    }

    /// Atomically consume a login transaction: validate it is unexpired and
    /// unconsumed, return its encrypted verifier, and mark it consumed in one
    /// step. `state_hash` binds the callback to the exact transaction issued at
    /// `/auth/github/start`. Returns `None` when unknown/expired/consumed.
    pub async fn consume_login(
        &self,
        state_hash: &str,
    ) -> Result<Option<(Uuid, Vec<u8>, i32)>, ManagerError> {
        let row = sqlx::query_as::<_, (Uuid, Option<Vec<u8>>, Option<i32>)>(
            "UPDATE auth_transactions
                SET consumed_at = now()
              WHERE state_hash = $1 AND purpose = 'login'
                AND consumed_at IS NULL AND expires_at > clock_timestamp()
              RETURNING id, encrypted_pkce_verifier, pkce_key_version",
        )
        .bind(state_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(|(id, verifier, version)| {
            (id, verifier.unwrap_or_default(), version.unwrap_or(1))
        }))
    }

    /// Whether a login transaction with this state hash exists and is not yet
    /// consumed (used to make callback replay return a consistent 401).
    pub async fn state_exists(&self, state_hash: &str) -> Result<bool, ManagerError> {
        let row: (i64,) =
            sqlx::query_as("SELECT count(*) FROM auth_transactions WHERE state_hash = $1")
                .bind(state_hash)
                .fetch_one(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.0 > 0)
    }
}

fn translate_state_insert(e: sqlx::Error) -> ManagerError {
    if let sqlx::Error::Database(db) = &e {
        if db.constraint() == Some("auth_transactions_state_hash_key") {
            return ManagerError::Conflict("OAuth transaction already exists".to_string());
        }
    }
    ManagerError::from(e)
}

/// Sessions, refresh rotation, and reuse detection.
#[derive(Clone)]
pub struct SessionsRepository {
    pool: PgPool,
}

/// A freshly-created session credential pair, returned to the client exactly
/// once. Only the hashes are persisted.
#[derive(Clone)]
pub struct SessionCredentials {
    pub session_id: SessionId,
    pub family_id: Uuid,
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
}

/// A resolved session identity for an authenticated call.
#[derive(Debug, Clone)]
pub struct SessionPrincipal {
    pub session_id: SessionId,
    pub user_id: UserId,
    pub kind: String,
    pub access_expires_at: DateTime<Utc>,
}

impl SessionsRepository {
    pub fn new(pool: PgPool) -> Self {
        SessionsRepository { pool }
    }

    /// Absolute lifetime constants.
    pub const ACCESS_LIFETIME: Duration = Duration::minutes(15);
    pub const BROWSER_LIFETIME: Duration = Duration::hours(12);
    pub const REFRESH_LIFETIME: Duration = Duration::days(30);
    pub const RECENT_AUTH_WINDOW: Duration = Duration::minutes(10);

    /// Create a CLI session: access (15 min) + rotating refresh (30-day family
    /// max). Both tokens are returned once.
    pub async fn create_cli_session(
        &self,
        user_id: UserId,
    ) -> Result<SessionCredentials, ManagerError> {
        let access = crate::auth::crypto::Secret::generate();
        let refresh = crate::auth::crypto::Secret::generate();
        let session_id = SessionId::new();
        let family_id = Uuid::new_v4();
        let now = Utc::now();
        let access_expires = now + Self::ACCESS_LIFETIME;
        let refresh_expires = now + Self::REFRESH_LIFETIME;
        self.insert_session(
            session_id,
            user_id,
            "cli",
            family_id,
            &access.hash(),
            access_expires,
            &refresh.hash(),
            refresh_expires,
            now,
        )
        .await?;
        Ok(SessionCredentials {
            session_id,
            family_id,
            access_token: access.encode(),
            refresh_token: refresh.encode(),
            access_expires_at: access_expires,
            refresh_expires_at: refresh_expires,
        })
    }

    /// Create a browser session (12h absolute expiry). The client is issued a
    /// cookie, so the returned credential is used internally to set the cookie;
    /// the refresh credential is generated to satisfy the schema but never
    /// exposed to the client.
    pub async fn create_browser_session(
        &self,
        user_id: UserId,
    ) -> Result<SessionCredentials, ManagerError> {
        let access = crate::auth::crypto::Secret::generate();
        let refresh = crate::auth::crypto::Secret::generate();
        let session_id = SessionId::new();
        let family_id = Uuid::new_v4();
        let now = Utc::now();
        let access_expires = now + Self::BROWSER_LIFETIME;
        let refresh_expires = now + Self::BROWSER_LIFETIME;
        self.insert_session(
            session_id,
            user_id,
            "browser",
            family_id,
            &access.hash(),
            access_expires,
            &refresh.hash(),
            refresh_expires,
            now,
        )
        .await?;
        Ok(SessionCredentials {
            session_id,
            family_id,
            access_token: access.encode(),
            refresh_token: refresh.encode(),
            access_expires_at: access_expires,
            refresh_expires_at: refresh_expires,
        })
    }

    /// Create a CLI session in the device-consumption transaction.
    pub async fn create_cli_session_in_tx(
        &self,
        conn: &mut PgConnection,
        user_id: UserId,
    ) -> Result<SessionCredentials, ManagerError> {
        let access = crate::auth::crypto::Secret::generate();
        let refresh = crate::auth::crypto::Secret::generate();
        let now = Utc::now();
        let creds = SessionCredentials {
            session_id: SessionId::new(),
            family_id: Uuid::new_v4(),
            access_token: access.encode(),
            refresh_token: refresh.encode(),
            access_expires_at: now + Self::ACCESS_LIFETIME,
            refresh_expires_at: now + Self::REFRESH_LIFETIME,
        };
        let inserted = sqlx::query(
            "INSERT INTO sessions (id,user_id,kind,access_hash,access_expires_at,
             refresh_hash,refresh_expires_at,family_id,last_authenticated_at)
             SELECT $1,id,'cli',$3,$4,$5,$6,$7,clock_timestamp()
             FROM users WHERE id=$2 AND status='active'",
        )
        .bind(creds.session_id.0)
        .bind(user_id.0)
        .bind(access.hash())
        .bind(creds.access_expires_at)
        .bind(refresh.hash())
        .bind(creds.refresh_expires_at)
        .bind(creds.family_id)
        .execute(&mut *conn)
        .await
        .map_err(ManagerError::from)?;
        if inserted.rows_affected() != 1 {
            return Err(ManagerError::api("UNAUTHORIZED", "user is not active"));
        }
        crate::audit::insert(
            &mut *conn,
            &crate::audit::AuditEvent::new("auth.cli_session_created")
                .actor(user_id)
                .resource("session", creds.session_id.0),
        )
        .await?;
        Ok(creds)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_session(
        &self,
        session_id: SessionId,
        user_id: UserId,
        kind: &str,
        family_id: Uuid,
        access_hash: &str,
        access_expires: DateTime<Utc>,
        refresh_hash: &str,
        refresh_expires: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        let inserted = sqlx::query(
            "INSERT INTO sessions
                (id, user_id, kind, access_hash, access_expires_at, refresh_hash,
                 refresh_expires_at, family_id, last_authenticated_at, last_used_at)
             SELECT $1, id, $3, $4, $5, $6, $7, $8, $9, $9
             FROM users WHERE id = $2 AND status = 'active'",
        )
        .bind(session_id.0)
        .bind(user_id.0)
        .bind(kind)
        .bind(access_hash)
        .bind(access_expires)
        .bind(refresh_hash)
        .bind(refresh_expires)
        .bind(family_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        if inserted.rows_affected() != 1 {
            return Err(ManagerError::api("UNAUTHORIZED", "user is not active"));
        }
        crate::audit::insert(
            &mut *tx,
            &crate::audit::AuditEvent::new("auth.session_created")
                .actor(user_id)
                .resource("session", session_id.0),
        )
        .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    /// Validate a browser session by its access token, enforcing the 12-hour
    /// absolute expiry, non-revocation, and a live user. Returns the session
    /// principal or `None` when the session is invalid.
    pub async fn validate_browser_session(
        &self,
        access_hash: &str,
    ) -> Result<Option<SessionPrincipal>, ManagerError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>)>(
            "SELECT s.id, s.user_id, s.kind, s.access_expires_at, s.created_at
               FROM sessions s
               JOIN users u ON u.id = s.user_id
              WHERE s.access_hash = $1 AND s.kind = 'browser'
                AND s.created_at + interval '12 hours' > clock_timestamp()
                AND s.revoked_at IS NULL
                AND s.access_expires_at > now()
                AND u.status = 'active'",
        )
        .bind(access_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(|(id, user, kind, access_expires, created)| {
            let _ = created;
            SessionPrincipal {
                session_id: SessionId::from_uuid(id),
                user_id: UserId::from_uuid(user),
                kind,
                access_expires_at: access_expires,
            }
        }))
    }

    /// Validate a CLI session by its access token (15-min expiry). Returns the
    /// principal or `None`.
    pub async fn validate_cli_session(
        &self,
        access_hash: &str,
    ) -> Result<Option<SessionPrincipal>, ManagerError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, String, DateTime<Utc>)>(
            "SELECT s.id, s.user_id, s.kind, s.access_expires_at
               FROM sessions s
               JOIN users u ON u.id = s.user_id
              WHERE s.access_hash = $1 AND s.kind = 'cli'
                AND s.created_at + interval '30 days' > clock_timestamp()
                AND s.revoked_at IS NULL
                AND s.access_expires_at > now()
                AND u.status = 'active'",
        )
        .bind(access_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(
            row.map(|(id, user, kind, access_expires)| SessionPrincipal {
                session_id: SessionId::from_uuid(id),
                user_id: UserId::from_uuid(user),
                kind,
                access_expires_at: access_expires,
            }),
        )
    }

    /// Refresh a CLI credential with rotation and reuse detection.
    ///
    /// On success returns a new credential pair and updates the session. If the
    /// presented refresh credential was already consumed (present in
    /// `refresh_history`), the entire session family is revoked and `Err` is
    /// returned so the client must re-authenticate. The family maximum lifetime
    /// (30 days from creation) is enforced and never extended.
    pub async fn refresh_cli_session(
        &self,
        refresh_hash: &str,
    ) -> Result<Option<SessionCredentials>, ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;

        // Serialize equal-token attempts before checking history. The losing
        // concurrent rotation must see the winner's consumed credential.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("refresh:{refresh_hash}"))
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;

        // Detect reuse: has this refresh credential been consumed before?
        let reused: Option<(Uuid,)> =
            sqlx::query_as("SELECT family_id FROM refresh_history WHERE refresh_hash = $1")
                .bind(refresh_hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(ManagerError::from)?;

        if let Some((family,)) = reused {
            // Reuse of a consumed refresh credential revokes the family.
            self.revoke_family_in_tx(&mut tx, family).await?;
            crate::audit::insert(
                &mut *tx,
                &crate::audit::AuditEvent::new("auth.refresh_reuse")
                    .resource("session_family", family)
                    .result(crate::audit::AuditResult::Denied),
            )
            .await?;
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok(None);
        }

        // Find the session owning this refresh credential.
        let row = sqlx::query_as::<_, (Uuid, Uuid, Uuid, DateTime<Utc>, DateTime<Utc>)>(
            "SELECT id, user_id, family_id, refresh_expires_at, created_at
               FROM sessions
              WHERE refresh_hash = $1 AND kind = 'cli' AND revoked_at IS NULL
                AND EXISTS (SELECT 1 FROM users u WHERE u.id = sessions.user_id AND u.status = 'active')
              FOR UPDATE",
        )
        .bind(refresh_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        let Some((session_id, user_id, family_id, refresh_expires, created)) = row else {
            // Unknown refresh credential: it may be from a family that was
            // already rotated away; treat as a plain invalid credential, not a
            // family revocation.
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok(None);
        };

        let family_max = created + Self::REFRESH_LIFETIME;
        let now = Utc::now();
        if refresh_expires <= now || now >= family_max {
            // Expired family: no rotation, no extension.
            tx.commit().await.map_err(ManagerError::from)?;
            return Ok(None);
        }

        // Record this refresh credential as consumed.
        sqlx::query(
            "INSERT INTO refresh_history (refresh_hash, family_id, consumed_at, expires_at)
             VALUES ($1, $2, now(), $3)",
        )
        .bind(refresh_hash)
        .bind(family_id)
        .bind(family_max)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        // Rotate: issue new access + refresh, same family, bounded by family
        // max lifetime.
        let new_access = crate::auth::crypto::Secret::generate();
        let new_refresh = crate::auth::crypto::Secret::generate();
        let new_access_expires = (now + Self::ACCESS_LIFETIME).min(family_max);
        let new_refresh_expires = now + Self::REFRESH_LIFETIME;
        // Never extend the family beyond its 30-day maximum.
        let new_refresh_expires = new_refresh_expires.min(family_max);

        sqlx::query(
            "UPDATE sessions
                SET access_hash = $1, access_expires_at = $2,
                    refresh_hash = $3, refresh_expires_at = $4,
                    last_used_at = now()
              WHERE id = $5",
        )
        .bind(new_access.hash())
        .bind(new_access_expires)
        .bind(new_refresh.hash())
        .bind(new_refresh_expires)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        crate::audit::insert(
            &mut *tx,
            &crate::audit::AuditEvent::new("auth.refresh_rotated")
                .actor(UserId::from_uuid(user_id))
                .resource("session", session_id),
        )
        .await?;
        tx.commit().await.map_err(ManagerError::from)?;

        Ok(Some(SessionCredentials {
            session_id: SessionId::from_uuid(session_id),
            family_id,
            access_token: new_access.encode(),
            refresh_token: new_refresh.encode(),
            access_expires_at: new_access_expires,
            refresh_expires_at: new_refresh_expires,
        }))
    }

    async fn revoke_family_in_tx(
        &self,
        tx: &mut PgConnection,
        family_id: Uuid,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        )
        .bind(family_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Revoke a single session (logout).
    pub async fn revoke_session(&self, session_id: SessionId) -> Result<bool, ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        let user:Option<Uuid>=sqlx::query_scalar("UPDATE sessions SET revoked_at=now() WHERE id=$1 AND revoked_at IS NULL RETURNING user_id")
            .bind(session_id.0).fetch_optional(&mut *tx).await.map_err(ManagerError::from)?;
        if let Some(user) = user {
            crate::audit::insert(
                &mut *tx,
                &crate::audit::AuditEvent::new("auth.logout")
                    .actor(UserId::from_uuid(user))
                    .resource("session", session_id.0),
            )
            .await?;
        }
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(user.is_some())
    }

    /// Revoke all sessions for a user (used on suspension).
    pub async fn revoke_all_for_user(&self, user_id: UserId) -> Result<(), ManagerError> {
        sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Record fresh authentication on a session (recent-auth requirement).
    pub async fn record_recent_auth(&self, session_id: SessionId) -> Result<(), ManagerError> {
        sqlx::query("UPDATE sessions SET last_authenticated_at = now() WHERE id = $1")
            .bind(session_id.0)
            .execute(&self.pool)
            .await
            .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Whether the session has fresh authentication within the 10-minute
    /// window. Returns `None` when the session is unknown.
    pub async fn has_recent_auth(
        &self,
        session_id: SessionId,
    ) -> Result<Option<bool>, ManagerError> {
        let row: Option<(Option<DateTime<Utc>>,)> =
            sqlx::query_as("SELECT last_authenticated_at FROM sessions WHERE id = $1")
                .bind(session_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.map(|(t,)| {
            t.map(|t| t + Self::RECENT_AUTH_WINDOW > Utc::now())
                .unwrap_or(false)
        }))
    }
}

impl std::fmt::Debug for SessionCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionCredentials([REDACTED])")
    }
}
