//! Runtime credential lifecycle: issuance, rotation, revocation, and
//! authentication.
//!
//! Runtime credentials are workspace-scoped bearer secrets, distinct from human
//! browser/CLI sessions and from human API credentials. A credential is stored
//! only as a cryptographic hash; the raw secret is returned to the trusted
//! caller exactly once at issuance. Issuance, rotation, and revocation are
//! internal services — the WP-05/06 provisioning APIs that will call them are
//! not yet available, so there is deliberately no unauthenticated bootstrap
//! endpoint and no shared global runtime password.

use crate::auth::crypto::Secret;
use crate::error::ManagerError;
use crate::id::WorkspaceId;
use crate::runtime::repository::{RuntimeRepository, RuntimeScope, WORKSPACE_AUDIENCE};
use chrono::{Duration, Utc};

/// Default lifetime of a workspace runtime credential. Runtime credentials are
/// long-lived identity bearer tokens used only to authenticate to the broker,
/// which enforces scope on every exchange. Rotation shortens exposure when the
/// credential is rotated or the workspace generation advances.
pub const RUNTIME_CREDENTIAL_TTL_DAYS: i64 = 30;

/// The runtime credential lifecycle service.
#[derive(Clone)]
pub struct RuntimeCredentialService {
    pub repo: RuntimeRepository,
}

impl RuntimeCredentialService {
    pub fn new(repo: RuntimeRepository) -> Self {
        RuntimeCredentialService { repo }
    }

    /// Issue a fresh runtime credential for a workspace. This is an internal
    /// provisioning boundary: it is not exposed as an unauthenticated endpoint
    /// and must only be called by trusted provisioning/manager code (WP-05/06).
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
        let expires_at = Utc::now() + Duration::days(RUNTIME_CREDENTIAL_TTL_DAYS);
        self.repo
            .insert_credential(
                id,
                workspace_id,
                &secret.hash(),
                WORKSPACE_AUDIENCE,
                expires_at,
                generation,
            )
            .await?;
        Ok(RuntimeCredentialIssue {
            credential_id: id,
            raw_secret: secret.encode(),
            expires_at,
            generation,
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
        let id = uuid::Uuid::new_v4();
        let expires_at = Utc::now() + Duration::days(RUNTIME_CREDENTIAL_TTL_DAYS);
        let new_generation = self
            .repo
            .rotate_credential(id, workspace_id, &secret.hash(), expires_at)
            .await?;
        Ok(RuntimeCredentialIssue {
            credential_id: id,
            raw_secret: secret.encode(),
            expires_at,
            generation: new_generation,
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
        if raw_bearer.is_empty() || raw_bearer.len() > 4096 {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "runtime credential is invalid",
            ));
        }
        let hash = crate::auth::crypto::hash_token(raw_bearer);
        let scope = self
            .repo
            .resolve_runtime_scope(&hash)
            .await?
            .ok_or_else(|| ManagerError::api("AUTH_FAILED", "runtime credential is invalid"))?;

        // Audience must match exactly.
        if scope.credential_audience != WORKSPACE_AUDIENCE {
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
        Ok(scope)
    }
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
