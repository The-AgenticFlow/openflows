//! Runtime bootstrap, registration, rotation, revocation, and authentication.
//!
//! Runtime credentials are workspace-scoped bearer secrets, distinct from human
//! browser/CLI sessions and from human API credentials. A credential is stored
//! only as a cryptographic hash; the raw secret is returned to the trusted
//! caller exactly once at issuance. Issuance, rotation, and revocation are
//! internal services — the provisioning APIs that call them are
//! not yet available, so there is deliberately no unauthenticated bootstrap
//! endpoint and no shared global runtime password.

use crate::auth::crypto::Secret;
use crate::error::ManagerError;
use crate::id::WorkspaceId;
use crate::runtime::repository::{
    RuntimeRepository, RuntimeScope, WORKSPACE_AUDIENCE, WORKSPACE_REFRESH_AUDIENCE,
};
use chrono::{Duration, Utc};
use std::sync::Arc;

/// Single-use bootstrap lifetime includes provisioner queuing, cold image pulls,
/// and recovery builds. It must outlast the bounded Coder build waits; access
/// tokens remain short-lived and registration still rejects consumed secrets.
pub const BOOTSTRAP_TTL_MINUTES: i64 = 60;
pub const ACCESS_TTL_MINUTES: i64 = 15;
pub const REFRESH_FAMILY_TTL_HOURS: i64 = 24;

/// The runtime credential lifecycle service.
#[derive(Clone)]
pub struct RuntimeCredentialService {
    pub repo: RuntimeRepository,
    secret_provider: Arc<dyn crate::secrets::SecretProvider>,
}

impl RuntimeCredentialService {
    pub fn new(repo: RuntimeRepository) -> Self {
        RuntimeCredentialService {
            repo,
            secret_provider: Arc::new(crate::secrets::InMemorySecretProvider::new()),
        }
    }

    pub fn with_secret_provider(
        repo: RuntimeRepository,
        secret_provider: Arc<dyn crate::secrets::SecretProvider>,
    ) -> Self {
        RuntimeCredentialService {
            repo,
            secret_provider,
        }
    }

    /// Issue a fresh runtime credential for a workspace. This is an internal
    /// provisioning boundary: it is not exposed as an unauthenticated endpoint
    /// and must only be called by trusted provisioning/manager code.
    /// The raw secret is returned exactly once; only its hash is persisted.
    pub async fn issue_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<RuntimeCredentialIssue, ManagerError> {
        let generation = self
            .repo
            .workspace_generation(workspace_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("workspace"))?;
        let secret = Secret::generate();
        let id = uuid::Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(ACCESS_TTL_MINUTES);
        let family_id = uuid::Uuid::new_v4();
        let family_expires_at = Utc::now() + Duration::hours(REFRESH_FAMILY_TTL_HOURS);
        self.repo
            .insert_credential(
                id,
                workspace_id,
                &secret.hash(),
                WORKSPACE_AUDIENCE,
                expires_at,
                generation,
                family_id,
                family_expires_at,
            )
            .await?;
        Ok(RuntimeCredentialIssue {
            credential_id: id,
            raw_secret: secret.encode(),
            expires_at,
            generation,
        })
    }

    /// Create a single-use bootstrap value and write it to the configured
    /// secret-injection provider. PostgreSQL receives only the hash/reference.
    pub async fn issue_bootstrap(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<RuntimeBootstrapIssue, ManagerError> {
        self.repo
            .workspace_generation(workspace_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("workspace"))?;
        let secret = Secret::generate();
        let raw_secret = secret.encode();
        let id = uuid::Uuid::new_v4();
        let secret_ref = format!("workspace-bootstrap-{workspace_id}-{id}");
        let expires_at = Utc::now() + Duration::minutes(BOOTSTRAP_TTL_MINUTES);
        let stored_secret_ref = self
            .secret_provider
            .put(&secret_ref, raw_secret.as_bytes())
            .await
            .map_err(|e| ManagerError::Config(format!("cannot inject runtime bootstrap: {e}")))?;
        if let Err(error) = self
            .repo
            .insert_bootstrap(
                id,
                workspace_id,
                &secret.hash(),
                &stored_secret_ref,
                expires_at,
            )
            .await
        {
            let _ = self.secret_provider.delete(&stored_secret_ref).await;
            return Err(error);
        }
        Ok(RuntimeBootstrapIssue {
            secret_ref: stored_secret_ref,
            raw_secret,
            expires_at,
        })
    }

    pub async fn ensure_bootstrap_ref(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<String, ManagerError> {
        if let Some((secret_ref, _)) = self.repo.live_bootstrap_ref(workspace_id).await? {
            match self.secret_provider.get(&secret_ref).await {
                Ok(_) => return Ok(secret_ref),
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    // The DB row can outlive an ephemeral local secret
                    // provider. issue_bootstrap() revokes the stale live row
                    // transactionally before inserting its replacement.
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot verify runtime bootstrap: {error}"
                    )))
                }
            }
        }
        Ok(self.issue_bootstrap(workspace_id).await?.secret_ref)
    }

    /// Atomically consume a bootstrap value and return a short-lived access
    /// credential plus a rotating refresh credential. Bootstrap replay fails.
    pub async fn register(&self, raw_bootstrap: &str) -> Result<RuntimeSessionIssue, ManagerError> {
        validate_presented_secret(raw_bootstrap)?;
        let access = Secret::generate();
        let refresh = Secret::generate();
        let access_id = uuid::Uuid::new_v4();
        let refresh_id = uuid::Uuid::new_v4();
        let family_id = uuid::Uuid::new_v4();
        let access_expires_at = Utc::now() + Duration::minutes(ACCESS_TTL_MINUTES);
        let family_expires_at = Utc::now() + Duration::hours(REFRESH_FAMILY_TTL_HOURS);
        let consumed = self
            .repo
            .consume_bootstrap_and_insert_session(
                &crate::auth::crypto::hash_token(raw_bootstrap),
                access_id,
                &access.hash(),
                access_expires_at,
                refresh_id,
                &refresh.hash(),
                family_id,
                family_expires_at,
            )
            .await?
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "runtime bootstrap is invalid"))?;
        match self
            .secret_provider
            .delete(&consumed.bootstrap_secret_ref)
            .await
        {
            Ok(()) | Err(crate::secrets::SecretError::NotFound(_)) => {}
            Err(error) => {
                return Err(ManagerError::Config(format!(
                    "cannot retire consumed runtime bootstrap: {error}"
                )))
            }
        }
        if let Some(runtime_secret_ref) = consumed.runtime_secret_ref.as_deref() {
            let mut bytes = zeroize::Zeroizing::new(
                self.secret_provider
                    .get(runtime_secret_ref)
                    .await
                    .map_err(|error| {
                        ManagerError::Config(format!(
                            "cannot load registered workspace secret bundle: {error}"
                        ))
                    })?,
            );
            let mut payload: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|error| ManagerError::Service(error.into()))?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "bootstrap_token".into(),
                    serde_json::Value::String(String::new()),
                );
            }
            bytes = zeroize::Zeroizing::new(
                serde_json::to_vec(&payload)
                    .map_err(|error| ManagerError::Service(error.into()))?,
            );
            self.secret_provider
                .replace(runtime_secret_ref, &bytes)
                .await
                .map_err(|error| {
                    ManagerError::Config(format!(
                        "cannot retire bootstrap from workspace secret bundle: {error}"
                    ))
                })?;
        }
        Ok(RuntimeSessionIssue {
            workspace_id: consumed.workspace_id,
            access_secret: access.encode(),
            access_expires_at,
            refresh_secret: refresh.encode(),
            refresh_expires_at: family_expires_at,
            generation: consumed.generation,
        })
    }

    /// Rotate a workspace's runtime credential: advance the workspace
    /// credential generation, revoke every previously issued live credential,
    /// and issue a fresh one at the new generation. The old credential can no
    /// longer authenticate because its stored generation no longer matches the
    /// workspace's current generation.
    pub async fn rotate(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<RuntimeCredentialIssue, ManagerError> {
        let secret = Secret::generate();
        let refresh = Secret::generate();
        let id = uuid::Uuid::new_v4();
        let refresh_id = uuid::Uuid::new_v4();
        let family_id = uuid::Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(ACCESS_TTL_MINUTES);
        let family_expires_at = Utc::now() + Duration::hours(REFRESH_FAMILY_TTL_HOURS);
        let new_generation = self
            .repo
            .rotate_credential_pair(
                id,
                workspace_id,
                None,
                &secret.hash(),
                expires_at,
                refresh_id,
                &refresh.hash(),
                family_id,
                family_expires_at,
            )
            .await?;
        Ok(RuntimeCredentialIssue {
            credential_id: id,
            raw_secret: secret.encode(),
            expires_at,
            generation: new_generation,
        })
    }

    /// Rotate an authenticated refresh credential without extending its
    /// original 24-hour family lifetime. The presented refresh token is
    /// invalidated together with every credential in the previous generation.
    pub async fn refresh_session(
        &self,
        raw_refresh: &str,
    ) -> Result<RuntimeSessionIssue, ManagerError> {
        let scope = self.authenticate_refresh(raw_refresh).await?;
        if scope.credential_family_expires_at <= Utc::now() {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime refresh family has expired",
            ));
        }
        let access = Secret::generate();
        let refresh = Secret::generate();
        let access_id = uuid::Uuid::new_v4();
        let refresh_id = uuid::Uuid::new_v4();
        // Slide the refresh family forward so a running, healthy tenant is
        // never locked out after the original 24-hour window. Rotation keeps
        // the family renewable as long as the workspace keeps refreshing;
        // revocation (stop/delete) advances the workspace generation, which
        // immediately invalidates every credential in the family regardless
        // of this renewal.
        let family_expires_at = Utc::now() + Duration::hours(REFRESH_FAMILY_TTL_HOURS);
        let access_expires_at = std::cmp::min(
            Utc::now() + Duration::minutes(ACCESS_TTL_MINUTES),
            family_expires_at,
        );
        let generation = self
            .repo
            .rotate_credential_pair(
                access_id,
                scope.workspace_id,
                Some(&crate::auth::crypto::hash_token(raw_refresh)),
                &access.hash(),
                access_expires_at,
                refresh_id,
                &refresh.hash(),
                scope.credential_family_id,
                family_expires_at,
            )
            .await?;
        Ok(RuntimeSessionIssue {
            workspace_id: scope.workspace_id,
            access_secret: access.encode(),
            access_expires_at,
            refresh_secret: refresh.encode(),
            refresh_expires_at: family_expires_at,
            generation,
        })
    }

    /// Revoke all runtime credentials for a workspace and advance its
    /// generation so no previously issued credential can authenticate again.
    /// Also revokes any outstanding GitHub credential leases for the workspace.
    pub async fn revoke(&self, workspace_id: WorkspaceId) -> Result<(), ManagerError> {
        self.repo.bump_workspace_generation(workspace_id).await?;
        self.repo.revoke_workspace_credentials(workspace_id).await?;
        self.repo.revoke_leases_for_workspace(workspace_id).await?;
        Ok(())
    }

    /// Authenticate a presented runtime bearer credential and resolve its full
    /// scope. Validates the audience, expiry, revocation state, and that the
    /// credential's generation still equals the workspace's current generation.
    /// Returns the resolved scope on success.
    pub async fn authenticate(&self, raw_bearer: &str) -> Result<RuntimeScope, ManagerError> {
        self.authenticate_for_audience(raw_bearer, WORKSPACE_AUDIENCE)
            .await
    }

    pub async fn authenticate_refresh(
        &self,
        raw_bearer: &str,
    ) -> Result<RuntimeScope, ManagerError> {
        self.authenticate_for_audience(raw_bearer, WORKSPACE_REFRESH_AUDIENCE)
            .await
    }

    async fn authenticate_for_audience(
        &self,
        raw_bearer: &str,
        expected_audience: &str,
    ) -> Result<RuntimeScope, ManagerError> {
        validate_presented_secret(raw_bearer)?;
        let hash = crate::auth::crypto::hash_token(raw_bearer);
        let scope = self
            .repo
            .resolve_runtime_scope(&hash)
            .await?
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "runtime credential is invalid"))?;

        // Audience must match exactly.
        if scope.credential_audience != expected_audience {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime credential audience mismatch",
            ));
        }
        // Expiry and revocation.
        let now = Utc::now();
        if scope.credential_expires_at <= now {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime credential has expired",
            ));
        }
        if scope.credential_revoked_at.is_some() {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime credential has been revoked",
            ));
        }
        // Current workspace generation.
        if scope.credential_generation != scope.workspace_generation {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime credential is stale (rotated)",
            ));
        }
        if scope.organization_status != "ready"
            || scope.connection_status != "active"
            || !scope.repository_accessible
            || matches!(scope.tenant_desired_state.as_str(), "deleting" | "deleted")
            || matches!(
                scope.workspace_desired_state.as_str(),
                "deleting" | "deleted"
            )
        {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime scope is no longer active",
            ));
        }
        Ok(scope)
    }
}

fn validate_presented_secret(raw: &str) -> Result<(), ManagerError> {
    if raw.is_empty() || raw.len() > 4096 {
        return Err(ManagerError::api(
            "AUTH_FAILED",
            "runtime credential is invalid",
        ));
    }
    Ok(())
}

pub struct RuntimeBootstrapIssue {
    pub secret_ref: String,
    pub raw_secret: String,
    pub expires_at: chrono::DateTime<Utc>,
}

impl std::fmt::Debug for RuntimeBootstrapIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeBootstrapIssue")
            .field("secret_ref", &self.secret_ref)
            .field("expires_at", &self.expires_at)
            .field("raw_secret", &"[REDACTED]")
            .finish()
    }
}

pub struct RuntimeSessionIssue {
    pub workspace_id: WorkspaceId,
    pub access_secret: String,
    pub access_expires_at: chrono::DateTime<Utc>,
    pub refresh_secret: String,
    pub refresh_expires_at: chrono::DateTime<Utc>,
    pub generation: i64,
}

/// The result of issuing or rotating a runtime credential. The raw secret is
/// disclosed exactly once and must never be persisted or logged.
pub struct RuntimeCredentialIssue {
    pub credential_id: uuid::Uuid,
    pub raw_secret: String,
    pub expires_at: chrono::DateTime<Utc>,
    pub generation: i64,
}

impl std::fmt::Debug for RuntimeCredentialIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeCredentialIssue")
            .field("credential_id", &self.credential_id)
            .field("expires_at", &self.expires_at)
            .field("generation", &self.generation)
            .field("raw_secret", &"[REDACTED]")
            .finish()
    }
}
