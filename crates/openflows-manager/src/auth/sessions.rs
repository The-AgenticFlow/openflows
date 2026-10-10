//! Session lifecycle for browser and CLI authentication.
//!
//! `SessionManager` is a thin service over [`SessionsRepository`] that exposes
//! the operations the auth handlers and CSRF layer need, keeping route handlers
//! small. It also centralizes the "recent authentication" bookkeeping used to
//! gate ownership transfer and organization deletion.

use crate::auth::crypto::hash_token;
use crate::auth::repository::{SessionCredentials, SessionsRepository};
use crate::error::ManagerError;
use crate::id::{SessionId, UserId};

#[derive(Clone)]
pub struct SessionManager {
    repo: SessionsRepository,
}

impl SessionManager {
    pub fn new(repo: SessionsRepository) -> Self {
        SessionManager { repo }
    }

    /// Create a CLI session (access + refresh pair returned once).
    pub async fn create_cli_session(
        &self,
        user_id: UserId,
    ) -> Result<SessionCredentials, ManagerError> {
        self.repo.create_cli_session(user_id).await
    }

    /// Create a browser session (12h absolute; access token used as cookie).
    pub async fn create_browser_session(
        &self,
        user_id: UserId,
    ) -> Result<SessionCredentials, ManagerError> {
        self.repo.create_browser_session(user_id).await
    }

    /// Validate a browser session by its raw access token.
    pub async fn validate_browser(
        &self,
        raw_access_token: &str,
    ) -> Result<Option<crate::auth::repository::SessionPrincipal>, ManagerError> {
        self.repo
            .validate_browser_session(&hash_token(raw_access_token))
            .await
    }

    /// Validate a CLI session by its raw access token.
    pub async fn validate_cli(
        &self,
        raw_access_token: &str,
    ) -> Result<Option<crate::auth::repository::SessionPrincipal>, ManagerError> {
        self.repo
            .validate_cli_session(&hash_token(raw_access_token))
            .await
    }

    /// Refresh a CLI credential with rotation/reuse detection.
    pub async fn refresh_cli(
        &self,
        raw_refresh_token: &str,
    ) -> Result<Option<SessionCredentials>, ManagerError> {
        self.repo
            .refresh_cli_session(&hash_token(raw_refresh_token))
            .await
    }

    /// Record fresh authentication (recent-auth window).
    pub async fn record_recent_auth(&self, session_id: SessionId) -> Result<(), ManagerError> {
        self.repo.record_recent_auth(session_id).await
    }

    /// Whether the session has fresh authentication within 10 minutes.
    pub async fn has_recent_auth(
        &self,
        session_id: SessionId,
    ) -> Result<Option<bool>, ManagerError> {
        self.repo.has_recent_auth(session_id).await
    }

    /// Revoke a single session (logout).
    pub async fn revoke_session(&self, session_id: SessionId) -> Result<bool, ManagerError> {
        self.repo.revoke_session(session_id).await
    }

    /// Revoke every session for a user (suspension).
    pub async fn revoke_all_for_user(&self, user_id: UserId) -> Result<(), ManagerError> {
        self.repo.revoke_all_for_user(user_id).await
    }
}
