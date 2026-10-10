//! GitHub user-authorization (web application flow) login orchestration.
//!
//! Flow (per 01-user-management.md §4 and GitHub's web application flow):
//!   1. `start`: generate a 256-bit state, create a login transaction with a
//!      hashed state and an encrypted PKCE verifier, bind it to an HttpOnly
//!      transaction cookie, and redirect to GitHub's authorize URL.
//!   2. `callback`: validate the state against the cookie (constant-time),
//!      atomically consume the unexpired transaction, exchange the code with
//!      the PKCE verifier, fetch the authenticated user, and create a browser
//!      session. The provider access token is discarded after login.
//!
//! Safety properties:
//!   * The state value is only ever compared in constant time; it is stored
//!     hashed and delivered to GitHub in plaintext only on the redirect.
//!   * The callback never opens a redirect to an arbitrary URL: successful
//!     logins redirect only to an allowlisted relative application path.
//!   * The provider access token is not a runtime credential and is never
//!     persisted; it is dropped at the end of the callback.
//!   * No database transaction spans the upstream GitHub exchange.

use crate::auth::crypto::{ct_eq, hash_token, PkcePair, Secret};
use crate::auth::github::{AuthorizeRequest, GithubAuth};
use crate::auth::repository::{AuthTransactionRepository, SessionsRepository, UsersRepository};
use crate::auth::sessions::SessionManager;
use crate::config::AuthConfig;
use crate::error::ManagerError;
use crate::id::UserId;
use sqlx::PgPool;

/// The name of the HttpOnly transaction cookie that binds a login to the
/// browser that started it.
pub const TRANSACTION_COOKIE: &str = "of_login_tx";

/// Build a safe application-relative redirect target.
///
/// Only paths starting with `/` and not containing `//` or backslashes are
/// accepted. Query strings on relative paths are allowed only when they carry
/// no authority. This prevents open-redirect abuse via `state`/`redirect`
/// parameters.
pub fn safe_redirect_target(raw: &str) -> Result<String, ManagerError> {
    if raw.len() > 2048 || raw.chars().any(|c| c.is_control()) || raw.contains(char::from(92)) {
        return Err(ManagerError::InvalidInput("unsafe redirect target".into()));
    }
    let path = raw.split('?').next().unwrap_or("");
    if !matches!(
        path,
        "/" | "/api/v1/me" | "/auth/cli/verify" | "/invitations/accept"
    ) {
        return Err(ManagerError::InvalidInput("unsafe redirect target".into()));
    }
    Ok(raw.to_string())
}

/// The login orchestrator. Holds the dependencies shared by auth handlers.
#[derive(Clone)]
pub struct LoginService {
    pub pool: PgPool,
    pub users: UsersRepository,
    pub transactions: AuthTransactionRepository,
    pub sessions: SessionsRepository,
    pub session_manager: SessionManager,
    pub github: std::sync::Arc<dyn GithubAuth>,
    pub auth_config: AuthConfig,
}

impl LoginService {
    /// Begin a login: create a transaction and return the redirect URL and the
    /// transaction cookie value (raw state) to set.
    pub async fn start(&self) -> Result<(String, String), ManagerError> {
        self.start_with_next("/api/v1/me").await
    }

    pub async fn start_with_next(&self, next: &str) -> Result<(String, String), ManagerError> {
        if self.auth_config.client_id.trim().is_empty() {
            return Err(ManagerError::Config(
                "GitHub authentication is not configured".into(),
            ));
        }
        self.auth_config
            .validate_origin()
            .map_err(ManagerError::Config)?;
        let target = safe_redirect_target(next)?;
        let state = Secret::generate();
        let pkce = PkcePair::new();

        // Encrypt the PKCE verifier with the OAuth envelope key.
        let envelope = self.auth_config.envelope_cipher("oauth_pkce")?;
        // The return URL can contain an invitation token: encrypt it together
        // with PKCE material rather than persisting the raw URL.
        let payload =
            serde_json::to_vec(&serde_json::json!({"verifier":pkce.verifier,"target":target}))
                .map_err(|e| ManagerError::Service(e.into()))?;
        let encrypted = envelope.seal(&payload)?;

        self.transactions
            .create_login(&state.hash(), &encrypted, envelope.version() as i32)
            .await?;

        let redirect_uri = format!("{}/auth/github/callback", self.auth_config.public_url);
        let authorize_url = self.github.authorize_url(&AuthorizeRequest {
            client_id: self.auth_config.client_id.clone(),
            redirect_uri,
            state: state.encode(),
            code_challenge: pkce.challenge,
        });
        Ok((authorize_url, state.encode()))
    }

    /// Complete a login from the OAuth callback.
    ///
    /// `raw_state` is the state value from the callback query string;
    /// `cookie_state` is the raw state from the transaction cookie. `code` is
    /// the GitHub authorization code. `next` is the caller-supplied redirect
    /// target (default `/`).
    ///
    /// Returns `(safe_redirect_path, access_cookie_value)` on success.
    pub async fn callback(
        &self,
        raw_state: &str,
        cookie_state: &str,
        code: &str,
        next: &str,
    ) -> Result<(String, String), ManagerError> {
        if !next.is_empty() {
            safe_redirect_target(next)?;
        }
        // Transaction-cookie binding: the cookie must carry the same raw state.
        if !ct_eq(raw_state.as_bytes(), cookie_state.as_bytes()) {
            return Err(ManagerError::api("AUTH_FAILED", "login state mismatch"));
        }

        // Atomically consume the transaction (also rejects expiry/replay).
        let Some((_tx_id, encrypted_verifier, version)) = self
            .transactions
            .consume_login(&hash_token(raw_state))
            .await?
        else {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "login transaction invalid, expired, or already used",
            ));
        };

        // Decrypt the PKCE verifier using the version recorded alongside.
        let envelope = self
            .auth_config
            .envelope_cipher_for_version("oauth_pkce", version)?;
        let verifier_bytes = envelope.open(&encrypted_verifier).map_err(|_| {
            ManagerError::api("AUTH_FAILED", "login verifier could not be decrypted")
        })?;
        let payload: serde_json::Value = serde_json::from_slice(&verifier_bytes)
            .map_err(|_| ManagerError::api("AUTH_FAILED", "login verifier invalid"))?;
        let verifier = payload["verifier"]
            .as_str()
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "login verifier invalid"))?;
        let target = payload["target"]
            .as_str()
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "login return path invalid"))?;

        // Exchange the code server-side (no DB transaction held across this).
        let token = self.github.exchange_code(code, verifier).await?;

        // Fetch the authenticated user and map the immutable id to an identity.
        let github_user = self.github.fetch_user(&token.access_token).await?;
        let subject = github_user.id.to_string();
        let display_name = github_user
            .display_name
            .clone()
            .unwrap_or_else(|| github_user.login.clone());
        let (user_id, _created) = self
            .users
            .find_or_create_user(
                &self.pool,
                "github",
                &subject,
                &github_user.login,
                &display_name,
            )
            .await?;

        // Discard the provider access token after ordinary login.
        drop(token);

        // Create a browser session (12h absolute) and record fresh auth.
        let creds = self.sessions.create_browser_session(user_id).await?;
        self.session_manager
            .record_recent_auth(creds.session_id)
            .await?;

        let target = safe_redirect_target(target)?;
        Ok((target, creds.access_token))
    }

    /// Validate the browser session and return the user id, used by /me.
    pub async fn principal_for_browser(
        &self,
        access_token: &str,
    ) -> Result<Option<(crate::id::SessionId, UserId)>, ManagerError> {
        let principal = self
            .sessions
            .validate_browser_session(&hash_token(access_token))
            .await?;
        Ok(principal.map(|p| (p.session_id, p.user_id)))
    }
}
