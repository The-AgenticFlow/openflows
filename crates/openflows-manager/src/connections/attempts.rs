//! Connection attempt state machine.
//!
//! A connection attempt is bound to an authenticated Openflows admin and
//! organization. It tracks up to two high-entropy states — a user-OAuth state
//! and an installation-setup state — each stored only as a SHA-256 hash. The
//! connection is only activated once both required proofs are present. Attempts
//! expire after 10 minutes and consumed callbacks are rejected.

use crate::auth::crypto::{PkcePair, Secret};
use crate::auth::github::{AuthorizeRequest, GithubAuth};
use crate::config::{AuthConfig, GithubAppConfig};
use crate::connections::repository::{AttemptRow, ConnectionRepository, FlowType};
use crate::error::ManagerError;
use crate::id::OrganizationId;
use crate::id::UserId;
use std::sync::Arc;

/// Attempt lifetime in minutes (spec: expire after 10 minutes).
pub const ATTEMPT_LIFETIME_MINUTES: i64 = 10;

/// The freshly-generated material for a connection flow. The raw state values
/// are returned exactly once to the initiating browser; only hashes are stored.
#[derive(Debug, Clone)]
pub struct FlowMaterial {
    pub attempt_id: uuid::Uuid,
    /// Raw OAuth state (delivered to the browser once).
    pub oauth_state: String,
    /// Raw setup state (delivered to the browser once).
    pub setup_state: String,
    /// Encrypted PKCE verifier used to exchange the OAuth code.
    pub encrypted_verifier: Vec<u8>,
    /// The OAuth authorization URL.
    pub authorization_url: String,
    /// The installation-setup URL.
    pub setup_url: String,
}

/// The connection-flow service: builds attempts and resolves callbacks.
#[derive(Clone)]
pub struct AttemptService {
    pub repo: ConnectionRepository,
    pub github: Arc<dyn GithubAuth>,
    pub auth_config: AuthConfig,
    pub github_app: GithubAppConfig,
}

impl AttemptService {
    pub fn new(
        repo: ConnectionRepository,
        github: Arc<dyn GithubAuth>,
        auth_config: AuthConfig,
        github_app: GithubAppConfig,
    ) -> Self {
        AttemptService {
            repo,
            github,
            auth_config,
            github_app,
        }
    }

    /// Generate a fresh flow (states, verifier, URLs) and persist the attempt.
    /// Returns the material so the caller can deliver the URLs once.
    pub async fn start(
        &self,
        organization_id: OrganizationId,
        initiated_by: UserId,
        flow_type: FlowType,
    ) -> Result<FlowMaterial, ManagerError> {
        let material = self.build_flow(initiated_by, flow_type)?;
        self.repo
            .create_attempt(
                material.attempt_id,
                organization_id,
                initiated_by,
                flow_type,
                Some(&Secret::hash_raw(&material.oauth_state)),
                Some(&Secret::hash_raw(&material.setup_state)),
                Some(&material.encrypted_verifier),
                ATTEMPT_LIFETIME_MINUTES,
            )
            .await?;
        Ok(material)
    }

    /// Build the flow material without persisting (used when the caller needs
    /// to drive idempotency around the insert). Generates fresh high-entropy
    /// states and encrypts the PKCE verifier.
    pub fn build_flow(
        &self,
        _initiated_by: UserId,
        flow_type: FlowType,
    ) -> Result<FlowMaterial, ManagerError> {
        let oauth_state = Secret::generate();
        let setup_state = Secret::generate();
        let pkce = PkcePair::new();

        let cipher = self
            .auth_config
            .envelope_cipher("oauth_pkce")
            .map_err(|_| ManagerError::Config("envelope cipher unavailable".into()))?;
        let encrypted_verifier = cipher.seal(pkce.verifier.as_bytes())?;

        let callback_base = self.auth_config.public_url.trim_end_matches('/');
        let redirect_uri = format!("{callback_base}/api/v1/github/oauth/callback");
        let authorize = AuthorizeRequest {
            client_id: self.auth_config.client_id.clone(),
            redirect_uri: redirect_uri.clone(),
            state: oauth_state.encode(),
            code_challenge: pkce.challenge,
        };
        let authorization_url = self.github.authorize_url(&authorize);

        let setup_url = match (&self.github_app.app_slug, flow_type) {
            (Some(slug), _) => format!(
                "https://github.com/apps/{}/installations/new?state={}",
                slug,
                setup_state.encode()
            ),
            (None, _) => String::new(),
        };

        Ok(FlowMaterial {
            attempt_id: uuid::Uuid::new_v4(),
            oauth_state: oauth_state.encode(),
            setup_state: setup_state.encode(),
            encrypted_verifier,
            authorization_url,
            setup_url,
        })
    }

    /// Resolve an attempt by its raw OAuth state (used by the OAuth callback).
    /// The raw state is hashed and matched against the stored hash.
    pub async fn resolve_oauth(&self, raw_state: &str) -> Result<Option<AttemptRow>, ManagerError> {
        self.repo
            .attempt_by_oauth_state(&Secret::hash_raw(raw_state))
            .await
    }

    /// Resolve an attempt by its raw setup state (used by the setup callback).
    pub async fn resolve_setup(&self, raw_state: &str) -> Result<Option<AttemptRow>, ManagerError> {
        self.repo
            .attempt_by_setup_state(&Secret::hash_raw(raw_state))
            .await
    }

    /// Whether an OAuth state has ever been issued (for replay diagnostics).
    pub async fn oauth_state_exists(&self, raw_state: &str) -> Result<bool, ManagerError> {
        self.repo
            .attempt_oauth_state_exists(&Secret::hash_raw(raw_state))
            .await
    }

    /// Exchange an OAuth authorization code for a user token using the
    /// connection-specific redirect URI.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<crate::auth::github::UserToken, ManagerError> {
        self.github
            .exchange_code_with_redirect(code, verifier, redirect_uri)
            .await
    }

    /// Decrypt the stored PKCE verifier for an attempt.
    pub fn decrypt_verifier(&self, row: &AttemptRow) -> Result<String, ManagerError> {
        let encrypted = row
            .oauth_code_verifier_ref
            .as_deref()
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "missing OAuth verifier"))?;
        let cipher = self
            .auth_config
            .envelope_cipher("oauth_pkce")
            .map_err(|_| ManagerError::Config("envelope cipher unavailable".into()))?;
        let plain = cipher.open(encrypted)?;
        String::from_utf8(plain)
            .map_err(|_| ManagerError::api("AUTH_FAILED", "invalid OAuth verifier"))
    }
}
