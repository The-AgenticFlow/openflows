//! Tenant-scoped runtime resources: Redis credentials with immutable
//! namespaces, tenant-scoped lifecycle-hook signing keys, and workspace rows.
//!
//! Every tenant has its own Redis credential and an immutable namespace (the
//! tenant UUID). Only the credential hash is persisted; the raw password is
//! disclosed exactly once through the secret provider and mounted into the
//! workspace. The service (not the runtime) enforces that a tenant's Redis
//! client can only reach its own immutable namespace.
//!
//! Coder lifecycle-hook verification moves to the Manager ingress; each tenant
//! has its own hook signing key (stored only as a hash) so a compromised tenant
//! cannot forge hooks for another tenant, and rotation does not require a
//! workspace rebuild.

use crate::auth::crypto::Secret;
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId, WorkspaceId};
use crate::secrets::SecretProvider;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::warn;
use zeroize::Zeroizing;

mod tenant_delivery;

/// The Redis username prefix for tenant-scoped clients.
pub const TENANT_REDIS_USER_PREFIX: &str = "tenant";

/// The tenant controller's Redis user (see
/// `RedisIsolationBackend::ensure_controller`): only the Nexus workspace
/// receives it; worker workspaces get the tenant user.
pub fn controller_redis_username(tenant_username: &str) -> String {
    format!("{tenant_username}-ctl")
}

/// A tenant's isolated Redis credential (namespace is the immutable tenant
/// UUID, never a customer-chosen name).
#[derive(Debug, Clone)]
pub struct TenantRedisCredential {
    pub tenant_id: TenantId,
    pub username: String,
    pub namespace: String,
    pub credential_hash: String,
    pub secret_ref: String,
    pub generation: i64,
}

/// A tenant-scoped lifecycle-hook signing key (hash only).
#[derive(Debug, Clone)]
pub struct TenantHookKey {
    pub tenant_id: TenantId,
    pub key_hash: String,
    pub generation: i64,
    pub secret_ref: String,
}

/// A workspace row within a tenant.
#[derive(Debug, Clone)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub organization_id: OrganizationId,
    pub tenant_id: TenantId,
    pub role: String,
    pub slot: Option<i32>,
}

/// Persistence and issuance for tenant-scoped runtime resources.
#[derive(Clone)]
pub struct TenantRuntimeRepository {
    pool: PgPool,
    secret_provider: std::sync::Arc<dyn SecretProvider>,
    redis_isolation: std::sync::Arc<dyn crate::runtime::redis::RedisIsolationBackend>,
    delivery: std::sync::Arc<dyn crate::runtime::delivery::SecretDelivery>,
    isolation: std::sync::Arc<dyn crate::runtime::isolation::TenantIsolation>,
}

impl TenantRuntimeRepository {
    pub fn new(pool: PgPool) -> Self {
        TenantRuntimeRepository {
            pool,
            secret_provider: std::sync::Arc::new(crate::secrets::InMemorySecretProvider::new()),
            redis_isolation: std::sync::Arc::new(
                crate::runtime::redis::InMemoryRedisIsolation::default(),
            ),
            delivery: std::sync::Arc::new(crate::runtime::delivery::StoreReferenceDelivery),
            isolation: std::sync::Arc::new(crate::runtime::isolation::NoIsolation),
        }
    }

    pub fn with_secret_provider(
        pool: PgPool,
        secret_provider: std::sync::Arc<dyn SecretProvider>,
    ) -> Self {
        TenantRuntimeRepository {
            pool,
            secret_provider,
            redis_isolation: std::sync::Arc::new(
                crate::runtime::redis::InMemoryRedisIsolation::default(),
            ),
            delivery: std::sync::Arc::new(crate::runtime::delivery::StoreReferenceDelivery),
            isolation: std::sync::Arc::new(crate::runtime::isolation::NoIsolation),
        }
    }

    pub fn with_backends(
        pool: PgPool,
        secret_provider: std::sync::Arc<dyn SecretProvider>,
        redis_isolation: std::sync::Arc<dyn crate::runtime::redis::RedisIsolationBackend>,
    ) -> Self {
        TenantRuntimeRepository {
            pool,
            secret_provider,
            redis_isolation,
            delivery: std::sync::Arc::new(crate::runtime::delivery::StoreReferenceDelivery),
            isolation: std::sync::Arc::new(crate::runtime::isolation::NoIsolation),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Reconcile active tenant ACLs after a Redis restart. Redis ACL users are
    /// server state, while the credential rows and secret references are the
    /// durable source of truth.
    pub async fn restore_redis_credentials(&self) -> Result<(), ManagerError> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT username, namespace, secret_ref, tenant_id::text
               FROM tenant_redis_credentials
              WHERE revoked_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        for (username, namespace, secret_ref, _tenant_id) in rows {
            let password = match self.secret_provider.get(&secret_ref).await {
                Ok(password) => password,
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    // The credential row is durable in PostgreSQL, but the
                    // local in-memory provider lost its secret on restart. Do
                    // not block Manager startup (and its recovery workers) on
                    // it: `ensure_redis_credential` replaces the stale
                    // credential on the next reconciliation.
                    warn!(
                        username,
                        secret_ref, "skipping Redis credential restore: stored secret is missing"
                    );
                    continue;
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot restore tenant Redis secret: {error}"
                    )))
                }
            };
            let password = std::str::from_utf8(&password)
                .map_err(|_| ManagerError::Config("tenant Redis secret is not UTF-8".into()))?;
            self.redis_isolation
                .ensure_tenant(&username, password, &namespace)
                .await?;
        }
        let controllers: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT username, namespace, controller_secret_ref
               FROM tenant_redis_credentials
              WHERE revoked_at IS NULL AND controller_secret_ref IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        for (username, namespace, secret_ref) in controllers {
            let password = match self.secret_provider.get(&secret_ref).await {
                Ok(password) => password,
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    warn!(
                        username,
                        secret_ref,
                        "skipping controller Redis credential restore: stored secret is missing"
                    );
                    continue;
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot restore controller Redis secret: {error}"
                    )))
                }
            };
            let password = std::str::from_utf8(&password)
                .map_err(|_| ManagerError::Config("controller Redis secret is not UTF-8".into()))?;
            self.redis_isolation
                .ensure_controller(&controller_redis_username(&username), password, &namespace)
                .await?;
        }
        Ok(())
    }

    /// Allocate a tenant-scoped Redis credential and its immutable namespace.
    /// Returns the issued credential with the raw password mounted exactly once
    /// (the caller stores the secret reference; only the hash is persisted).
    pub async fn allocate_redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        secret_ref: &str,
    ) -> Result<TenantRedisCredential, ManagerError> {
        let secret = Secret::generate();
        let encoded = secret.encode();
        let username = format!(
            "{}-{}",
            TENANT_REDIS_USER_PREFIX,
            tenant_id.0.to_string().replace('-', "")
        );
        let namespace = tenant_id.0.to_string();
        let id = uuid::Uuid::new_v4();
        self.redis_isolation
            .ensure_tenant(&username, &encoded, &namespace)
            .await?;
        let stored_secret_ref = self
            .secret_provider
            .put(secret_ref, encoded.as_bytes())
            .await
            .map_err(|e| ManagerError::Config(format!("cannot store tenant Redis secret: {e}")))?;
        let generation = sqlx::query_scalar::<_, i64>(
            "INSERT INTO tenant_redis_credentials
                (id, tenant_id, organization_id, username, namespace, credential_hash, secret_ref, generation)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 0)
             ON CONFLICT (tenant_id) DO UPDATE
                SET credential_hash = EXCLUDED.credential_hash,
                    secret_ref = EXCLUDED.secret_ref,
                    generation = tenant_redis_credentials.generation + 1,
                    revoked_at = NULL, updated_at = now()
              WHERE tenant_redis_credentials.organization_id = EXCLUDED.organization_id
                AND tenant_redis_credentials.revoked_at IS NOT NULL
             RETURNING generation",
        )
        .bind(id)
        .bind(tenant_id.0)
        .bind(org_id.0)
        .bind(&username)
        .bind(&namespace)
        .bind(secret.hash())
        .bind(&stored_secret_ref)
        .fetch_one(&self.pool)
        .await;
        let generation = match generation {
            Ok(generation) => generation,
            Err(error) => {
                let _ = self.secret_provider.delete(&stored_secret_ref).await;
                let _ = self.redis_isolation.revoke_tenant(&username).await;
                // Another Manager replica may have created the live
                // credential between the initial lookup and this insert.
                // Converge on that durable row instead of surfacing a
                // duplicate-key retry loop.
                if let Some(existing) = self.redis_credential(org_id, tenant_id).await? {
                    return Ok(existing);
                }
                return Err(error.into());
            }
        };
        Ok(TenantRedisCredential {
            tenant_id,
            username,
            namespace,
            credential_hash: secret.hash(),
            secret_ref: stored_secret_ref,
            generation,
        })
    }

    /// Ensure the tenant's Redis credential and its controller credential
    /// ([`controller_redis_username`]) both exist and are live in Redis.
    pub async fn ensure_redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        secret_ref: &str,
    ) -> Result<TenantRedisCredential, ManagerError> {
        let credential = self
            .ensure_tenant_redis_credential(org_id, tenant_id, secret_ref)
            .await?;
        self.ensure_controller_redis_credential(org_id, tenant_id, &credential)
            .await?;
        Ok(credential)
    }

    /// The controller's Redis password secret reference, allocating it (and
    /// the Redis ACL user) when missing or lost.
    pub async fn ensure_controller_redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        credential: &TenantRedisCredential,
    ) -> Result<String, ManagerError> {
        let username = controller_redis_username(&credential.username);
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT controller_secret_ref FROM tenant_redis_credentials
              WHERE tenant_id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        if let Some(reference) = existing {
            match self.secret_provider.get(&reference).await {
                Ok(password) => {
                    let password = std::str::from_utf8(&password).map_err(|_| {
                        ManagerError::Config("controller Redis secret is not UTF-8".into())
                    })?;
                    self.redis_isolation
                        .ensure_controller(&username, password, &credential.namespace)
                        .await?;
                    return Ok(reference);
                }
                // Lost (in-memory provider restart): issue a new password below.
                Err(crate::secrets::SecretError::NotFound(_)) => {}
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot verify controller Redis secret: {error}"
                    )))
                }
            }
        }
        let secret = Secret::generate();
        let encoded = secret.encode();
        self.redis_isolation
            .ensure_controller(&username, &encoded, &credential.namespace)
            .await?;
        let name = format!("tenant-redis-controller-{}", tenant_id.0);
        let reference = match self.secret_provider.put(&name, encoded.as_bytes()).await {
            Ok(reference) => reference,
            Err(crate::secrets::SecretError::AlreadyExists(_)) => {
                self.secret_provider
                    .replace(&name, encoded.as_bytes())
                    .await
                    .map_err(|e| {
                        ManagerError::Config(format!("cannot store controller Redis secret: {e}"))
                    })?;
                name
            }
            Err(error) => {
                return Err(ManagerError::Config(format!(
                    "cannot store controller Redis secret: {error}"
                )))
            }
        };
        sqlx::query(
            "UPDATE tenant_redis_credentials
                SET controller_secret_ref = $3, updated_at = now()
              WHERE tenant_id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .bind(&reference)
        .execute(&self.pool)
        .await?;
        Ok(reference)
    }

    async fn ensure_tenant_redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        secret_ref: &str,
    ) -> Result<TenantRedisCredential, ManagerError> {
        if let Some(existing) = self.redis_credential(org_id, tenant_id).await? {
            match self.secret_provider.get(&existing.secret_ref).await {
                Ok(password) => {
                    let password = std::str::from_utf8(&password).map_err(|_| {
                        ManagerError::Config("tenant Redis secret is not UTF-8".into())
                    })?;
                    self.redis_isolation
                        .ensure_tenant(&existing.username, password, &existing.namespace)
                        .await?;
                    return Ok(existing);
                }
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    // The database is durable, but an in-memory development
                    // provider is not. Revoke the stale row and converge on a
                    // fresh credential instead of retrying the same dead ref.
                    self.redis_isolation
                        .revoke_tenant(&existing.username)
                        .await?;
                    sqlx::query(
                        "UPDATE tenant_redis_credentials
                            SET revoked_at = COALESCE(revoked_at, now()), updated_at = now()
                          WHERE organization_id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
                    )
                    .bind(org_id.0)
                    .bind(tenant_id.0)
                    .execute(&self.pool)
                    .await?;
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot verify tenant Redis secret: {error}"
                    )))
                }
            }
        }
        self.allocate_redis_credential(org_id, tenant_id, secret_ref)
            .await
    }

    pub async fn clear_redis_namespace(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<u64, ManagerError> {
        let Some(credential) = self.redis_credential_any(org_id, tenant_id).await? else {
            return Ok(0);
        };
        self.redis_isolation
            .clear_namespace(&credential.namespace)
            .await
    }

    pub async fn clean_redis_namespace(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        reset_all: bool,
    ) -> Result<super::redis::NamespaceCleanResult, ManagerError> {
        let Some(credential) = self.redis_credential(org_id, tenant_id).await? else {
            return Ok(super::redis::NamespaceCleanResult::default());
        };
        self.redis_isolation
            .clean_namespace(&credential.namespace, reset_all)
            .await
    }

    pub async fn revoke_redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError> {
        if let Some(credential) = self.redis_credential_any(org_id, tenant_id).await? {
            self.redis_isolation
                .revoke_tenant(&controller_redis_username(&credential.username))
                .await?;
            let controller_ref: Option<String> = sqlx::query_scalar(
                "SELECT controller_secret_ref FROM tenant_redis_credentials
                  WHERE organization_id = $1 AND tenant_id = $2",
            )
            .bind(org_id.0)
            .bind(tenant_id.0)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
            if let Some(reference) = controller_ref {
                match self.secret_provider.delete(&reference).await {
                    Ok(()) | Err(crate::secrets::SecretError::NotFound(_)) => {}
                    Err(error) => {
                        return Err(ManagerError::Config(format!(
                            "cannot delete controller Redis secret: {error}"
                        )))
                    }
                }
            }
            self.redis_isolation
                .revoke_tenant(&credential.username)
                .await?;
            match self.secret_provider.delete(&credential.secret_ref).await {
                Ok(()) | Err(crate::secrets::SecretError::NotFound(_)) => {}
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot delete tenant Redis secret: {error}"
                    )))
                }
            }
            sqlx::query(
                "UPDATE tenant_redis_credentials SET revoked_at = COALESCE(revoked_at, now()), updated_at = now()
                  WHERE organization_id = $1 AND tenant_id = $2",
            )
            .bind(org_id.0)
            .bind(tenant_id.0)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Assemble the per-workspace credential bundle and deliver it. Only this method reads
    /// the component secrets; templates receive the opaque returned reference.
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_workspace_secret(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
        bootstrap_ref: &str,
        manager_url: &str,
        runtime_redis_url: &str,
    ) -> Result<String, ManagerError> {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT runtime_secret_ref FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        let existing_bundle = existing.filter(|value| {
            std::path::Path::new(value)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("workspace-runtime-"))
        });
        let redis = self
            .redis_credential(org_id, tenant_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant Redis credential"))?;
        // The controller (Nexus) gets the controller Redis user and the hook
        // key; worker workspaces get neither, so nothing they can read lets
        // them rewrite operator decisions or derive the docs approval key.
        let role: String = sqlx::query_scalar("SELECT role FROM workspaces WHERE id = $1")
            .bind(workspace_id.0)
            .fetch_one(&self.pool)
            .await?;
        let controller = role == "nexus";
        let (redis_username, redis_secret_ref) = if controller {
            (
                controller_redis_username(&redis.username),
                self.ensure_controller_redis_credential(org_id, tenant_id, &redis)
                    .await?,
            )
        } else {
            (redis.username.clone(), redis.secret_ref.clone())
        };
        let redis_password =
            Zeroizing::new(
                String::from_utf8(self.secret_provider.get(&redis_secret_ref).await.map_err(
                    |e| ManagerError::Config(format!("cannot resolve Redis secret: {e}")),
                )?)
                .map_err(|_| ManagerError::Config("tenant Redis secret is not UTF-8".into()))?,
            );
        let bootstrap = Zeroizing::new(
            String::from_utf8(
                self.secret_provider
                    .get(bootstrap_ref)
                    .await
                    .map_err(|e| ManagerError::Config(format!("cannot resolve bootstrap: {e}")))?,
            )
            .map_err(|_| ManagerError::Config("runtime bootstrap is not UTF-8".into()))?,
        );
        let hook_ref: String = sqlx::query_scalar(
            "SELECT secret_ref FROM tenant_hook_keys
              WHERE organization_id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_one(&self.pool)
        .await?;
        let hook_key = Zeroizing::new(
            String::from_utf8(
                self.secret_provider
                    .get(&hook_ref)
                    .await
                    .map_err(|e| ManagerError::Config(format!("cannot resolve hook key: {e}")))?,
            )
            .map_err(|_| ManagerError::Config("tenant hook key is not UTF-8".into()))?,
        );

        let mut redis_url = url::Url::parse(runtime_redis_url)
            .map_err(|_| ManagerError::Config("invalid runtime Redis URL".into()))?;
        redis_url
            .set_username(&redis_username)
            .map_err(|_| ManagerError::Config("invalid Redis username".into()))?;
        redis_url
            .set_password(Some(redis_password.as_str()))
            .map_err(|_| ManagerError::Config("invalid Redis password".into()))?;
        let hook_url =
            crate::config::runtime_hook_url(tenant_id.into()).map_err(ManagerError::Config)?;
        let mut bundle = serde_json::json!({
            "schema_version": 1,
            "workspace_id": workspace_id,
            "tenant_id": tenant_id,
            "manager_url": manager_url.trim_end_matches('/'),
            "bootstrap_token": bootstrap.as_str(),
            "redis_url": redis_url.as_str(),
            "redis_username": redis_username,
            "redis_namespace": redis.namespace,
        });
        if controller {
            bundle["hook_key"] = serde_json::json!(hook_key.as_str());
            bundle["hook_url"] = serde_json::json!(hook_url);
        }
        let payload = Zeroizing::new(
            serde_json::to_vec(&bundle).map_err(|e| ManagerError::Service(e.into()))?,
        );
        let stored_secret_ref = if let Some(existing) = existing_bundle {
            match self.secret_provider.replace(&existing, &payload).await {
                Ok(()) => existing,
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    // The workspace row may survive a restart while an
                    // in-memory provider loses the mounted bundle. Recreate
                    // it under the same stable reference.
                    match self.secret_provider.put(&existing, &payload).await {
                        Ok(reference) => reference,
                        Err(crate::secrets::SecretError::AlreadyExists(_)) => {
                            self.secret_provider
                                .replace(&existing, &payload)
                                .await
                                .map_err(|e| {
                                    ManagerError::Config(format!(
                                        "cannot restore workspace secret: {e}"
                                    ))
                                })?;
                            existing
                        }
                        Err(error) => {
                            return Err(ManagerError::Config(format!(
                                "cannot restore workspace secret: {error}"
                            )))
                        }
                    }
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot rotate workspace secret: {error}"
                    )))
                }
            }
        } else {
            let secret_ref = format!("workspace-runtime-{workspace_id}");
            match self.secret_provider.put(&secret_ref, &payload).await {
                Ok(reference) => reference,
                Err(crate::secrets::SecretError::AlreadyExists(_)) => {
                    self.secret_provider
                        .replace(&secret_ref, &payload)
                        .await
                        .map_err(|e| {
                            ManagerError::Config(format!("cannot rotate workspace secret: {e}"))
                        })?;
                    secret_ref
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot inject workspace secret: {error}"
                    )))
                }
            }
        };
        let stored_secret_ref = self
            .secret_provider
            .canonical_reference(&stored_secret_ref)
            .map_err(|e| {
                ManagerError::Config(format!("cannot resolve workspace secret reference: {e}"))
            })?;
        sqlx::query(
            "UPDATE workspaces SET runtime_secret_ref = $1, updated_at = now() WHERE id = $2",
        )
        .bind(&stored_secret_ref)
        .bind(workspace_id.0)
        .execute(&self.pool)
        .await?;
        self.deliver_workspace_bundle(
            org_id,
            tenant_id,
            workspace_id,
            &stored_secret_ref,
            &payload,
        )
        .await
    }

    /// Look up a tenant's Redis credential (namespace + username + generation),
    /// used by the runtime to resolve its isolated scope. Never returns the raw
    /// password.
    pub async fn redis_credential(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<Option<TenantRedisCredential>, ManagerError> {
        let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT username, namespace, credential_hash, secret_ref, generation
               FROM tenant_redis_credentials
              WHERE tenant_id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(username, namespace, credential_hash, secret_ref, generation)| {
                TenantRedisCredential {
                    tenant_id,
                    username,
                    namespace,
                    credential_hash,
                    secret_ref,
                    generation,
                }
            },
        ))
    }

    async fn redis_credential_any(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<Option<TenantRedisCredential>, ManagerError> {
        let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT username, namespace, credential_hash, secret_ref, generation
               FROM tenant_redis_credentials
              WHERE tenant_id = $1 AND organization_id = $2",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(username, namespace, credential_hash, secret_ref, generation)| {
                TenantRedisCredential {
                    tenant_id,
                    username,
                    namespace,
                    credential_hash,
                    secret_ref,
                    generation,
                }
            },
        ))
    }

    /// Allocate a tenant-scoped lifecycle-hook signing key (hash only). The raw
    /// key is delivered to the manager ingress relay; only its hash is stored.
    pub async fn allocate_hook_key(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        secret_ref: &str,
    ) -> Result<TenantHookKey, ManagerError> {
        let secret = Secret::generate();
        let encoded = secret.encode();
        let key_hash = secret.hash();
        let stored_secret_ref = self
            .secret_provider
            .put(secret_ref, encoded.as_bytes())
            .await
            .map_err(|e| ManagerError::Config(format!("cannot store tenant hook key: {e}")))?;
        let id = uuid::Uuid::new_v4();
        let generation = sqlx::query_scalar::<_, i64>(
            "INSERT INTO tenant_hook_keys
                (id, tenant_id, organization_id, key_hash, secret_ref, generation)
             VALUES ($1, $2, $3, $4, $5, 0)
             ON CONFLICT (tenant_id) DO UPDATE
                SET key_hash = EXCLUDED.key_hash, secret_ref = EXCLUDED.secret_ref,
                    generation = tenant_hook_keys.generation + 1,
                    revoked_at = NULL, updated_at = now()
              WHERE tenant_hook_keys.organization_id = EXCLUDED.organization_id
                AND tenant_hook_keys.revoked_at IS NOT NULL
             RETURNING generation",
        )
        .bind(id)
        .bind(tenant_id.0)
        .bind(org_id.0)
        .bind(&key_hash)
        .bind(&stored_secret_ref)
        .fetch_one(&self.pool)
        .await;
        let generation = match generation {
            Ok(generation) => generation,
            Err(error) => {
                let _ = self.secret_provider.delete(&stored_secret_ref).await;
                return Err(error.into());
            }
        };
        Ok(TenantHookKey {
            tenant_id,
            key_hash,
            generation,
            secret_ref: stored_secret_ref,
        })
    }

    pub async fn ensure_hook_key(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        secret_ref: &str,
    ) -> Result<TenantHookKey, ManagerError> {
        let existing: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT key_hash, secret_ref, generation FROM tenant_hook_keys
              WHERE tenant_id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((key_hash, stored_ref, generation)) = existing {
            match self.secret_provider.get(&stored_ref).await {
                Ok(_) => {
                    return Ok(TenantHookKey {
                        tenant_id,
                        key_hash,
                        generation,
                        secret_ref: stored_ref,
                    })
                }
                Err(crate::secrets::SecretError::NotFound(_)) => {
                    sqlx::query(
                        "UPDATE tenant_hook_keys
                            SET revoked_at = COALESCE(revoked_at, now()), updated_at = now()
                          WHERE organization_id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
                    )
                    .bind(org_id.0)
                    .bind(tenant_id.0)
                    .execute(&self.pool)
                    .await?;
                }
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot verify tenant hook secret: {error}"
                    )))
                }
            }
        }
        self.allocate_hook_key(org_id, tenant_id, secret_ref).await
    }

    /// Look up a tenant's hook signing key hash (for the ingress relay to verify
    /// a Coder hook JWT). Never returns the raw key.
    pub async fn hook_key_hash(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<Option<String>, ManagerError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT key_hash FROM tenant_hook_keys
              WHERE tenant_id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Insert a workspace row for a tenant (Nexus or worker role). Returns the
    /// new workspace id.
    pub async fn insert_workspace(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        role: &str,
        slot: Option<i32>,
    ) -> Result<WorkspaceId, ManagerError> {
        let id = WorkspaceId::new();
        sqlx::query(
            "INSERT INTO workspaces
                (id, organization_id, tenant_id, role, slot, desired_state, observed_state)
             VALUES ($1, $2, $3, $4, $5, 'provisioning', 'provisioning')",
        )
        .bind(id.0)
        .bind(org_id.0)
        .bind(tenant_id.0)
        .bind(role)
        .bind(slot)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Return the existing live workspace for a deterministic role/slot, or
    /// reserve it once. This makes provisioning retries converge instead of
    /// creating duplicate control-plane rows.
    pub async fn ensure_workspace(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        role: &str,
        slot: Option<i32>,
    ) -> Result<WorkspaceId, ManagerError> {
        let existing: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM workspaces
              WHERE organization_id = $1 AND tenant_id = $2 AND role = $3
                AND slot IS NOT DISTINCT FROM $4 AND desired_state <> 'deleted'
              ORDER BY created_at LIMIT 1",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .bind(role)
        .bind(slot)
        .fetch_optional(&self.pool)
        .await?;
        match existing {
            Some(id) => Ok(WorkspaceId::from_uuid(id)),
            None => self.insert_workspace(org_id, tenant_id, role, slot).await,
        }
    }

    /// Insert a workspace row within an existing transaction (used by the
    /// runtime worker-provisioning path so the row, quota reservation, and
    /// operation commit together).
    pub async fn insert_workspace_in_tx(
        &self,
        conn: &mut sqlx::PgConnection,
        org_id: OrganizationId,
        tenant_id: TenantId,
        role: &str,
        slot: Option<i32>,
    ) -> Result<WorkspaceId, ManagerError> {
        let id = WorkspaceId::new();
        sqlx::query(
            "INSERT INTO workspaces
                (id, organization_id, tenant_id, role, slot, desired_state, observed_state)
             VALUES ($1, $2, $3, $4, $5, 'provisioning', 'provisioning')",
        )
        .bind(id.0)
        .bind(org_id.0)
        .bind(tenant_id.0)
        .bind(role)
        .bind(slot)
        .execute(&mut *conn)
        .await?;
        Ok(id)
    }

    /// Persist the Coder workspace id for a workspace row.
    pub async fn set_coder_workspace_id(
        &self,
        workspace_id: WorkspaceId,
        coder_workspace_id: &str,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "UPDATE workspaces SET coder_workspace_id = $1, updated_at = now()
              WHERE id = $2",
        )
        .bind(coder_workspace_id)
        .bind(workspace_id.0)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a workspace observed running.
    pub async fn mark_workspace_running(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "UPDATE workspaces SET observed_state = 'running', updated_at = now()
              WHERE id = $1",
        )
        .bind(workspace_id.0)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Resolve a tenant for a workspace (used by the runtime to derive its
    /// tenant scope). Returns the tenant id.
    pub async fn tenant_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<TenantId>, ManagerError> {
        let row: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT tenant_id FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| TenantId::from_uuid(r.0)))
    }

    /// Hash a high-entropy tenant hook key for storage.
    pub fn hash_secret(raw: &str) -> String {
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    }
}
