//! GitHub credential broker: runtime-authenticated, short-lived installation
//! token exchange for an authorized repository.
//!
//! The broker authenticates a workspace runtime principal (already resolved to
//! a [`RuntimeScope`] by the credential service), derives the approved
//! permission profile from trusted server state, and exchanges the App JWT for
//! an installation token restricted to the workspace's tenant repository. All
//! authorization scope is derived server-side; a caller can never supply an
//! organization, installation, repository, or permission override.
//!
//! Concurrency safety: relevant connection/workspace generations are captured
//! before the external token exchange and rechecked (along with resource state)
//! before the lease is recorded and the token returned. No database transaction
//! is held open across the GitHub request. If authorization changes during the
//! exchange, the token is discarded, upstream revocation is attempted, and a
//! sanitized error is returned. An already-issued token may remain usable until
//! upstream revocation succeeds or it expires — local generation checks do not
//! invalidate a token at GitHub.

use crate::audit::{self, AuditEvent, AuditResult};
use crate::config::AuthConfig;
use crate::connections::app_jwt::AppSigner;
use crate::connections::github_app::{GithubAppApi, InstallationToken};
use crate::error::ManagerError;
use crate::id::CredentialLeaseId;
use crate::runtime::permissions::{resolve_profile, PermissionProfile, RuntimePurpose};
use crate::runtime::repository::{
    token_fingerprint, NewCredentialLease, RuntimeRepository, RuntimeScope,
};
use base64::Engine;
use chrono::{Duration, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The minimum remaining validity we accept from GitHub. Installation tokens
/// are issued with a one-hour lifetime; anything shorter is treated as an
/// invalid upstream expiry and fails closed.
const MIN_TOKEN_VALIDITY: Duration = Duration::minutes(30);

/// Number of in-process single-flight shards. Equal broker keys hash to the
/// same shard so concurrent exchanges for the same key serialize, avoiding
/// duplicate upstream token exchanges. A distributed lock is required when the
/// Manager runs multiple replicas (documented as an operations follow-up).
const SINGLE_FLIGHT_SHARDS: usize = 64;

/// The GitHub username reported to clients for an installation access token.
pub const INSTALLATION_TOKEN_USERNAME: &str = "x-access-token";

/// The broker's single-flight cache key: the tuple that uniquely identifies an
/// exchange so concurrent identical requests coalesce.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BrokerKey {
    app_id: i64,
    installation_id: i64,
    repository_id: i64,
    profile_hash: String,
    connection_generation: i64,
    workspace_generation: i64,
}

/// The credential broker service.
#[derive(Clone)]
pub struct CredentialBroker {
    pub repo: RuntimeRepository,
    pub api: Arc<dyn GithubAppApi>,
    pub signer: Arc<dyn AppSigner>,
    pub app_id: i64,
    pub auth_config: AuthConfig,
    inflight: Arc<Vec<tokio::sync::Mutex<()>>>,
}

impl CredentialBroker {
    pub fn new(
        repo: RuntimeRepository,
        api: Arc<dyn GithubAppApi>,
        signer: Arc<dyn AppSigner>,
        app_id: i64,
        auth_config: AuthConfig,
    ) -> Self {
        let inflight = Arc::new(
            (0..SINGLE_FLIGHT_SHARDS)
                .map(|_| tokio::sync::Mutex::new(()))
                .collect::<Vec<_>>(),
        );
        CredentialBroker {
            repo,
            api,
            signer,
            app_id,
            auth_config,
            inflight,
        }
    }

    fn shard_for(key: &BrokerKey) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        (h.finish() as usize) % SINGLE_FLIGHT_SHARDS
    }

    /// Exchange a short-lived installation token for the authenticated
    /// workspace's authorized repository. `scope` is the authenticated runtime
    /// principal; `purpose` selects the server-controlled permission profile.
    /// Returns the token and its metadata.
    pub async fn exchange(
        &self,
        scope: &RuntimeScope,
        purpose: RuntimePurpose,
        request_id: &str,
    ) -> Result<BrokerCredential, ManagerError> {
        // ---- 1. Validate resource state (fail closed). ---------------------
        validate_resource_state(scope)?;

        // ---- 2. Resolve the server-controlled permission profile. ----------
        let profile = resolve_profile(&scope.workspace_role, purpose)?;

        // ---- 3. Capture generations before the external exchange. ----------
        let captured_connection_generation = scope.connection_generation;
        let captured_workspace_generation = scope.workspace_generation;

        let key = BrokerKey {
            app_id: self.app_id,
            installation_id: scope.installation_id,
            repository_id: scope.repository_id,
            profile_hash: profile.profile_hash.clone(),
            connection_generation: captured_connection_generation,
            workspace_generation: captured_workspace_generation,
        };
        let shard = CredentialBroker::shard_for(&key);
        let _guard = self.inflight[shard].lock().await;

        // ---- 4. Exchange the App JWT for an installation token. ------------
        let jwt = self.signer.sign().await?;
        let token = self
            .api
            .exchange_installation_token(
                &jwt,
                scope.installation_id,
                &[scope.repository_id],
                &profile.permissions,
            )
            .await?;

        // Defense-in-depth: validate the returned repository scope, permission
        // set, and expiry before it is used. Even if the underlying client did
        // not enforce these, the broker fails closed on any mismatch.
        if let Err(e) = validate_token(&token, &profile, scope.repository_id) {
            let _ = self
                .api
                .revoke_installation_token(&jwt, scope.installation_id, &token.token)
                .await;
            return Err(e);
        }

        // ---- 5. Recheck authorization / resource state / generations. ------
        // The token was issued for a specific connection/workspace generation
        // and resource state. If any of that changed during the exchange, the
        // token must be discarded before it is recorded or returned.
        let fresh = self
            .repo
            .resolve_workspace_scope(scope.workspace_id)
            .await?
            .ok_or_else(|| ManagerError::api("AUTH_CHANGED", "runtime scope is no longer valid"))?;

        let credential_current = self
            .repo
            .credential_is_current(
                scope.credential_id,
                scope.workspace_id,
                captured_workspace_generation,
            )
            .await?;
        let recheck_result = recheck(
            &fresh,
            captured_connection_generation,
            captured_workspace_generation,
        )
        .and_then(|()| {
            if credential_current {
                Ok(())
            } else {
                Err(ManagerError::api(
                    "AUTH_CHANGED",
                    "runtime credential was revoked during token issuance",
                ))
            }
        });
        if let Err(e) = recheck_result {
            // Authorization changed during the exchange: discard the token,
            // attempt upstream revocation, and return a sanitized error.
            let _ = self
                .api
                .revoke_installation_token(&jwt, scope.installation_id, &token.token)
                .await;
            self.repo
                .revoke_leases_for_workspace(scope.workspace_id)
                .await?;
            audit::insert_with_pool(
                self.repo.pool(),
                &AuditEvent::new("runtime.github_credential_discarded")
                    .organization(scope.organization_id)
                    .actor_type("runtime".to_string())
                    .resource("workspace", scope.workspace_id.0)
                    .result(AuditResult::Denied)
                    .request_id(request_id),
            )
            .await?;
            return Err(e);
        }

        // ---- 6. Record the credential lease and return the token. ----------
        let lease_id = self.record_lease(scope, &profile, purpose, &token).await?;

        // Re-verify the recorded lease's generations are still current under an
        // authoritative read; if they moved, revoke the lease and fail closed.
        if !self
            .repo
            .lease_generations_current(
                lease_id,
                captured_connection_generation,
                captured_workspace_generation,
            )
            .await?
        {
            self.repo.revoke_lease(lease_id).await?;
            let _ = self
                .api
                .revoke_installation_token(&jwt, scope.installation_id, &token.token)
                .await;
            return Err(ManagerError::api(
                "AUTH_CHANGED",
                "authorization changed during token issuance",
            ));
        }

        audit::insert_with_pool(
            self.repo.pool(),
            &AuditEvent::new("runtime.github_credential_issued")
                .organization(scope.organization_id)
                .actor_type("runtime".to_string())
                .resource("workspace", scope.workspace_id.0)
                .result(AuditResult::Success)
                .request_id(request_id),
        )
        .await?;

        Ok(BrokerCredential {
            token: token.token,
            expires_at: token.expires_at,
            repository_id: scope.repository_id,
            username: INSTALLATION_TOKEN_USERNAME.to_string(),
            permissions: token.permissions,
        })
    }

    /// Persist the credential lease. The token is encrypted with the envelope
    /// cipher (the existing encrypted-secret mechanism) and stored only in the
    /// lease's `encrypted_ref`; the raw token never appears in an ordinary
    /// column, log, audit event, or outbox payload.
    async fn record_lease(
        &self,
        scope: &RuntimeScope,
        profile: &PermissionProfile,
        purpose: RuntimePurpose,
        token: &InstallationToken,
    ) -> Result<CredentialLeaseId, ManagerError> {
        let cipher = self
            .auth_config
            .envelope_cipher("runtime_lease")
            .map_err(|_| ManagerError::Config("envelope cipher unavailable".into()))?;
        let sealed = cipher.seal(token.token.as_bytes())?;
        let encrypted_ref = base64::engine::general_purpose::STANDARD.encode(&sealed);
        let lease = NewCredentialLease {
            organization_id: scope.organization_id,
            connection_id: scope.connection_id,
            tenant_id: Some(scope.tenant_id),
            workspace_id: Some(scope.workspace_id),
            installation_id: scope.installation_id,
            repository_id: scope.repository_id,
            permission_profile_hash: profile.profile_hash.clone(),
            connection_generation: scope.connection_generation,
            workspace_generation: scope.workspace_generation,
            purpose: purpose.as_str().to_string(),
            token_fingerprint: token_fingerprint(&token.token),
            encrypted_ref: Some(encrypted_ref),
            expires_at: token.expires_at,
        };
        self.repo.insert_lease(&lease).await
    }
}

/// Validate the trusted resource state of a resolved runtime scope. Every
/// condition must hold for an exchange to proceed; any mismatch fails closed.
pub(crate) fn validate_resource_state(scope: &RuntimeScope) -> Result<(), ManagerError> {
    if scope.organization_status != "ready" && scope.organization_status != "provisioning" {
        return Err(ManagerError::api(
            "ORG_UNAVAILABLE",
            "organization is not available",
        ));
    }
    if scope.tenant_desired_state == "deleting"
        || scope.tenant_desired_state == "deleted"
        || scope.tenant_desired_state == "paused"
        || scope.tenant_observed_state == "deleted"
        || scope.tenant_observed_state == "deleting"
    {
        return Err(ManagerError::api(
            "TENANT_UNAVAILABLE",
            "tenant is not available",
        ));
    }
    if scope.workspace_observed_state != "running" || scope.workspace_desired_state != "running" {
        return Err(ManagerError::api(
            "WORKSPACE_UNAVAILABLE",
            "workspace is not available for runtime access",
        ));
    }
    if scope.connection_status != "active" {
        return Err(ManagerError::api(
            "CONNECTION_UNAVAILABLE",
            "GitHub connection is not active",
        ));
    }
    if !scope.repository_accessible {
        return Err(ManagerError::api(
            "REPOSITORY_UNAVAILABLE",
            "repository is not accessible",
        ));
    }
    Ok(())
}

/// Recheck that the authoritative scope still authorizes the exchange: the
/// connection and workspace generations must be unchanged and resource state
/// must still permit issuance.
fn recheck(
    fresh: &RuntimeScope,
    expected_connection_generation: i64,
    expected_workspace_generation: i64,
) -> Result<(), ManagerError> {
    if fresh.connection_generation != expected_connection_generation
        || fresh.workspace_generation != expected_workspace_generation
    {
        return Err(ManagerError::api(
            "AUTH_CHANGED",
            "authorization changed during token issuance",
        ));
    }
    validate_resource_state(fresh)
}

/// Validate that a returned installation token exactly matches the requested
/// repository scope and permission profile and has a usable lifetime. Any
/// mismatch, unexpected or unverifiable scope, or truncated expiry fails
/// closed.
fn validate_token(
    token: &InstallationToken,
    profile: &PermissionProfile,
    repository_id: i64,
) -> Result<(), ManagerError> {
    let returned: std::collections::BTreeSet<i64> = token.repository_ids.iter().copied().collect();
    if returned.len() != 1 || !returned.contains(&repository_id) {
        return Err(ManagerError::api(
            "GITHUB_UNAVAILABLE",
            "GitHub returned a different repository scope than requested",
        ));
    }
    if token.permissions != profile.permissions {
        return Err(ManagerError::api(
            "GITHUB_UNAVAILABLE",
            "GitHub returned different permissions than requested",
        ));
    }
    if token.expires_at - Utc::now() < MIN_TOKEN_VALIDITY {
        return Err(ManagerError::api(
            "GITHUB_UNAVAILABLE",
            "GitHub returned an invalid token expiry",
        ));
    }
    Ok(())
}

/// A credential returned to the authorized runtime. The token must never be
/// logged or persisted; the response carries `Cache-Control: no-store`.
pub struct BrokerCredential {
    pub token: String,
    pub expires_at: chrono::DateTime<Utc>,
    pub repository_id: i64,
    pub username: String,
    pub permissions: BTreeMap<String, String>,
}

impl std::fmt::Debug for BrokerCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerCredential")
            .field("expires_at", &self.expires_at)
            .field("repository_id", &self.repository_id)
            .field("username", &self.username)
            .field("permissions", &self.permissions)
            .field("token", &"[REDACTED]")
            .finish()
    }
}
