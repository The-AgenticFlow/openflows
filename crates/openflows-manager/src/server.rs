//! Server bootstrap, application state, and graceful serving utilities.

use crate::config::ManagerConfig;
use crate::db::Db;
use crate::error::ManagerError;
use crate::idempotency::IdempotencyService;
use crate::repositories::{OrganizationsRepository, TenantsRepository};
use axum::Router;
use pocketflow_core::SharedStore;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::{
    net::TcpListener,
    time::{timeout, Duration},
};
use tower_http::trace::TraceLayer;

const READINESS_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

fn coder_operator_token(bytes: Vec<u8>) -> Result<String, ManagerError> {
    let raw = String::from_utf8(bytes)
        .map_err(|_| ManagerError::Config("Coder operator token is not UTF-8".into()))?;
    let token = raw.trim();
    if token.is_empty()
        || reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).is_err()
    {
        return Err(ManagerError::Config(
            "Coder operator token is empty or contains invalid header characters".into(),
        ));
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod coder_operator_token_tests {
    use super::coder_operator_token;

    #[test]
    fn strips_line_ending_from_file_backed_token() {
        assert_eq!(
            coder_operator_token(b"operator-key\r\n".to_vec()).unwrap(),
            "operator-key"
        );
    }

    #[test]
    fn rejects_empty_or_header_unsafe_token() {
        assert!(coder_operator_token(b" \n".to_vec()).is_err());
        assert!(coder_operator_token(b"first\nsecond".to_vec()).is_err());
    }
}

/// Async dependency probe used by `/ready`.
///
/// The trait keeps the readiness endpoint decoupled from Redis and gives tests
/// a small seam for simulating unavailable or stalled dependencies.
pub trait ReadinessCheck: Send + Sync {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ManagerError>> + Send + '_>>;
}

#[derive(Clone)]
struct StoreReadiness {
    // SharedStore is cloned cheaply and owns the backing connection state.
    store: SharedStore,
}

impl ReadinessCheck for StoreReadiness {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ManagerError>> + Send + '_>> {
        Box::pin(async move {
            // A successful ping proves the backing store can respond inside the
            // readiness deadline.
            self.store.ping().await?;
            Ok(())
        })
    }
}

/// The database-backed domain services owned by the Manager.
///
/// Constructed only when the Manager has a control-plane database (hosted mode,
/// or local development with an explicit database). Repositories and services
/// expose organization-scoped operations. In addition to the core
/// repositories, this holds the human-authentication and organization services
/// added by the authorization layer.
#[derive(Clone)]
pub struct ManagerServices {
    pub db: Db,
    pub organizations: OrganizationsRepository,
    pub tenants: TenantsRepository,
    pub idempotency: IdempotencyService,
    /// Central tenant quota enforcement (minimal infrastructure safeguards).
    pub quotas: crate::quotas::TenantQuotaService,
    /// Tenant lifecycle service (create/list/detail/delete).
    pub tenants_service: crate::tenants::TenantService,
    pub users: crate::auth::repository::UsersRepository,
    pub sessions: crate::auth::repository::SessionsRepository,
    pub transactions: crate::auth::repository::AuthTransactionRepository,
    pub org_repo: crate::organizations::OrganizationRepository,
    pub orgs_service: crate::organizations::OrganizationService,
    pub session_manager: crate::auth::sessions::SessionManager,
    pub login: crate::auth::oauth::LoginService,
    pub device: crate::auth::device::DeviceFlowService,
    pub rate_limiter: crate::rate_limit::RateLimiter,
    pub auth_config: crate::config::AuthConfig,
    pub github: std::sync::Arc<dyn crate::auth::github::GithubAuth>,
    // GitHub App connection lifecycle.
    pub connections: crate::connections::ConnectionRepository,
    pub connections_service: crate::connections::ConnectionService,
    pub connection_worker: crate::connections::ConnectionWorker,
    pub webhooks: crate::connections::WebhookService,
    pub app_signer: std::sync::Arc<dyn crate::connections::AppSigner>,
    pub app_api: std::sync::Arc<dyn crate::connections::GithubAppApi>,
    pub webhook_secret: Option<Vec<u8>>,
    pub coder_hook_secret: Option<Vec<u8>>,
    pub secret_provider: std::sync::Arc<dyn crate::secrets::SecretProvider>,
    // Runtime authentication and GitHub credential broker.
    pub runtime_repo: crate::runtime::RuntimeRepository,
    pub runtime_credentials: crate::runtime::RuntimeCredentialService,
    pub broker: crate::runtime::CredentialBroker,
    // Tenant-scoped runtime resources (Redis isolation, hook keys, workspaces).
    pub tenant_runtime: crate::runtime::tenant::TenantRuntimeRepository,
    /// Runtime workspace lifecycle (Nexus-requested worker provisioning,
    /// heartbeat).
    pub runtime_workspaces: crate::runtime::RuntimeWorkspaceService,
    // Coder adapter and template releases.
    pub release_repo: crate::templates::repository::ReleaseRepository,
    pub release_service: crate::templates::service::ReleaseService,
    pub release_approvals: crate::releases::ReleaseApprovalService,
    /// Durable organization-provisioning worker, present when a Coder adapter
    /// and operator-configured release are available. `None` in pure
    /// local/test mode (tests build one directly with a mock adapter).
    pub org_provision: Option<crate::provisioning::OrgProvisionWorker>,
    /// Durable tenant-provisioning worker, present alongside the org worker
    /// when a Coder adapter is configured. `None` in pure local/test mode.
    pub tenant_provision: Option<crate::provisioning::TenantProvisionWorker>,
}

/// The secret-provider adapter name for the isolated in-memory provider.
pub const IN_MEMORY_SECRET_PROVIDER: &str = "in-memory";
pub const FILESYSTEM_SECRET_PROVIDER: &str = "filesystem";
pub const AWS_SECRETS_MANAGER_SECRET_PROVIDER: &str = "aws-secrets-manager";

/// Resolve the selected deployment backend. AWS credentials come from the SDK
/// default chain (for example, a workload identity), never user sessions.
pub async fn resolve_secret_provider(
    name: &str,
) -> Result<Box<dyn crate::secrets::SecretProvider>, ManagerError> {
    match name {
        IN_MEMORY_SECRET_PROVIDER => Ok(Box::new(crate::secrets::InMemorySecretProvider::new())),
        FILESYSTEM_SECRET_PROVIDER => {
            let root = std::env::var("OPENFLOWS_SECRET_DIR").map_err(|_| {
                ManagerError::Config(
                    "filesystem secret provider requires OPENFLOWS_SECRET_DIR".into(),
                )
            })?;
            Ok(Box::new(
                crate::secrets::FilesystemSecretProvider::new(root)
                    .map_err(|e| ManagerError::Config(e.to_string()))?,
            ))
        }
        AWS_SECRETS_MANAGER_SECRET_PROVIDER => {
            let prefix = std::env::var("OPENFLOWS_AWS_SECRETS_PREFIX").map_err(|_| {
                ManagerError::Config(
                    "AWS Secrets Manager requires OPENFLOWS_AWS_SECRETS_PREFIX".into(),
                )
            })?;
            let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            if sdk_config.region().is_none() {
                return Err(ManagerError::Config(
                    "AWS Secrets Manager requires an AWS region".into(),
                ));
            }
            let client = aws_sdk_secretsmanager::Client::new(&sdk_config);
            let mut provider = crate::secrets::AwsSecretsManagerSecretProvider::new(client, prefix)
                .map_err(|e| ManagerError::Config(e.to_string()))?;
            if let Ok(tags) = std::env::var(crate::config::AWS_SECRETS_TAGS_ENV) {
                let tags = crate::secrets::parse_secret_tags(&tags).map_err(|e| {
                    ManagerError::Config(format!("{}: {e}", crate::config::AWS_SECRETS_TAGS_ENV))
                })?;
                provider = provider.with_tags(tags);
            }
            Ok(Box::new(provider))
        }
        other => Err(ManagerError::Config(format!(
            "secret provider '{other}' is not implemented; refusing to start"
        ))),
    }
}

/// Load a local JSON object mapping secret references to UTF-8 values.
/// Parse errors deliberately exclude file contents, which contain credentials.
///
/// This is deliberately idempotent. A filesystem provider already contains
/// the values after the first Manager start, so a restart must not fail just
/// because `put` correctly refuses to overwrite an existing secret.
///
/// `master_key_ref` names the secret that holds the envelope master key
/// (`OPENFLOWS_CRYPTO_MASTER_KEY_REF`). That secret must never be silently
/// replaced when the seeded value differs from the stored one, otherwise
/// existing encrypted records become unreadable.
async fn populate_development_secrets(
    provider: &dyn crate::secrets::SecretProvider,
    contents: &[u8],
    master_key_ref: &Option<String>,
) -> Result<(), ManagerError> {
    let values: std::collections::HashMap<String, String> = serde_json::from_slice(contents)
        .map_err(|_| ManagerError::Config("invalid development secrets file".into()))?;
    for (name, value) in values {
        match provider.put(&name, value.as_bytes()).await {
            Ok(_) => {}
            Err(crate::secrets::SecretError::AlreadyExists(_)) => {
                let current = provider
                    .get(&name)
                    .await
                    .map_err(|_| ManagerError::Config("cannot verify development secret".into()))?;
                if current != value.as_bytes() {
                    // Protect the configured master-key reference (not a
                    // hardcoded filename). When no ref is configured the dev
                    // key is used, so the legacy `master-key` name is still
                    // guarded for the local/test seed.
                    let is_master_key = matches!(
                        master_key_ref.as_deref(),
                        Some(ref_name)
                            if name == ref_name || name.ends_with(&format!("/{ref_name}"))
                    ) || (master_key_ref.is_none()
                        && (name == "master-key" || name.ends_with("/master-key")));
                    if is_master_key {
                        return Err(ManagerError::Config(
                            "development master key differs from the existing stored value; refusing to replace it"
                                .into(),
                        ));
                    }
                    provider
                        .replace(&name, value.as_bytes())
                        .await
                        .map_err(|_| {
                            ManagerError::Config(format!(
                                "cannot renew development secret '{name}'"
                            ))
                        })?;
                }
            }
            Err(_) => {
                return Err(ManagerError::Config(
                    "cannot populate development secrets".into(),
                ))
            }
        }
    }
    Ok(())
}

/// Resolve the 32-byte envelope master key from the secret provider, or use the
/// explicit dev key when no ref is configured (local/tests only).
async fn resolve_master_key(
    provider: &dyn crate::secrets::SecretProvider,
    master_key_ref: &Option<String>,
) -> Result<crate::config::MasterKey, ManagerError> {
    match master_key_ref {
        Some(ref_name) => {
            let bytes = provider
                .get(ref_name)
                .await
                .map_err(|e| ManagerError::Config(format!("failed to resolve master key: {e}")))?;
            if bytes.len() != crate::config::MASTER_KEY_BYTES {
                return Err(ManagerError::Config(format!(
                    "master key at '{ref_name}' has wrong length {} (expected {})",
                    bytes.len(),
                    crate::config::MASTER_KEY_BYTES
                )));
            }
            let mut key = [0u8; crate::config::MASTER_KEY_BYTES];
            key.copy_from_slice(&bytes);
            Ok(crate::config::MasterKey(key))
        }
        None => Ok(crate::config::dev_master_key()),
    }
}

impl ManagerServices {
    /// Connect to the control-plane database, apply migrations, and build the
    /// domain services. Returns an error when the database is unavailable or
    /// migrations fail; the Manager must remain unready in that case.
    pub async fn connect(database_url: &str) -> Result<Self, ManagerError> {
        let db = Db::connect(database_url).await?;
        Ok(ManagerServices::from_db(db))
    }

    /// Build services from a connected database using the provided auth config
    /// and GitHub adapter (used by tests with fixture adapters). Connection
    /// services use fixture (fail-closed) App signer/API components so the
    /// lifecycle routes exist without external configuration; tests that need
    /// real connection behavior inject their own via [`Self::from_db_with_app`].
    pub fn from_db_with_auth(
        db: Db,
        auth_config: crate::config::AuthConfig,
        github: std::sync::Arc<dyn crate::auth::github::GithubAuth>,
    ) -> Self {
        Self::from_db_with_app(
            db,
            auth_config,
            github,
            0,
            std::sync::Arc::new(crate::connections::app_jwt::FixtureAppSigner::new(
                crate::connections::AppJwtClaims {
                    iss: 0,
                    iat: chrono::Utc::now().timestamp(),
                    exp: chrono::Utc::now().timestamp() + 540,
                },
            )),
            std::sync::Arc::new(UnconfiguredAppApi),
            None,
        )
    }

    /// Build services with an explicit App signer and App API. Used by
    /// tests and by real configuration resolution.
    #[allow(clippy::too_many_arguments)]
    pub fn from_db_with_app(
        db: Db,
        auth_config: crate::config::AuthConfig,
        github: std::sync::Arc<dyn crate::auth::github::GithubAuth>,
        app_id: i64,
        app_signer: std::sync::Arc<dyn crate::connections::AppSigner>,
        app_api: std::sync::Arc<dyn crate::connections::GithubAppApi>,
        webhook_secret: Option<Vec<u8>>,
    ) -> Self {
        Self::from_db_with_app_config(
            db,
            auth_config,
            github,
            app_id,
            app_signer,
            app_api,
            webhook_secret,
            crate::config::github_app_from_env(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_db_with_app_config(
        db: Db,
        auth_config: crate::config::AuthConfig,
        github: std::sync::Arc<dyn crate::auth::github::GithubAuth>,
        app_id: i64,
        app_signer: std::sync::Arc<dyn crate::connections::AppSigner>,
        app_api: std::sync::Arc<dyn crate::connections::GithubAppApi>,
        webhook_secret: Option<Vec<u8>>,
        github_app: crate::config::GithubAppConfig,
    ) -> Self {
        let pool = db.pool().clone();
        let users = crate::auth::repository::UsersRepository::new(pool.clone());
        let sessions = crate::auth::repository::SessionsRepository::new(pool.clone());
        let transactions = crate::auth::repository::AuthTransactionRepository::new(pool.clone());
        let org_repo = crate::organizations::OrganizationRepository::new(pool.clone());
        let organizations = OrganizationsRepository::new(pool.clone());
        let tenants = TenantsRepository::new(pool.clone());
        let idempotency = IdempotencyService::new(pool.clone());
        let session_manager = crate::auth::sessions::SessionManager::new(sessions.clone());
        let login = crate::auth::oauth::LoginService {
            pool: pool.clone(),
            users: users.clone(),
            transactions: transactions.clone(),
            sessions: sessions.clone(),
            session_manager: session_manager.clone(),
            github: github.clone(),
            auth_config: auth_config.clone(),
        };
        let device = crate::auth::device::DeviceFlowService::new(
            pool.clone(),
            sessions.clone(),
            auth_config.public_url.clone(),
        );
        let orgs_service = crate::organizations::OrganizationService::new(
            pool.clone(),
            org_repo.clone(),
            organizations.clone(),
            idempotency.clone(),
        );

        let connections = crate::connections::ConnectionRepository::new(pool.clone());
        let attempts = crate::connections::AttemptService::new(
            connections.clone(),
            github.clone(),
            auth_config.clone(),
            github_app.clone(),
        );
        let authority = crate::connections::AuthorityService::new(app_api.clone());
        let sync = crate::connections::SyncService::new(
            connections.clone(),
            app_api.clone(),
            app_signer.clone(),
        );
        let runtime_repo = crate::runtime::RuntimeRepository::new(pool.clone());
        let secret_provider: std::sync::Arc<dyn crate::secrets::SecretProvider> =
            std::sync::Arc::new(crate::secrets::InMemorySecretProvider::new());
        let tenant_runtime = crate::runtime::tenant::TenantRuntimeRepository::with_secret_provider(
            pool.clone(),
            secret_provider.clone(),
        );
        let webhooks = crate::connections::WebhookService::new(
            pool.clone(),
            connections.clone(),
            webhook_secret.clone().unwrap_or_default(),
            github_app.webhook_body_limit,
            app_id,
            sync.clone(),
            runtime_repo.clone(),
        );
        let binding = crate::connections::BindingService::new(
            pool.clone(),
            connections.clone(),
            authority.clone(),
            app_api.clone(),
            app_signer.clone(),
            app_id,
        );
        let connections_service = crate::connections::ConnectionService::new(
            pool.clone(),
            connections.clone(),
            attempts,
            authority.clone(),
            binding,
            webhooks.clone(),
            idempotency.clone(),
            org_repo.clone(),
            auth_config.clone(),
            app_id,
            app_api.clone(),
        );
        let connection_worker = crate::connections::ConnectionWorker::new(
            pool.clone(),
            connections.clone(),
            sync,
            webhooks.clone(),
            runtime_repo.clone(),
        );

        // Runtime authentication and GitHub credential broker.
        let runtime_credentials =
            crate::runtime::RuntimeCredentialService::new(runtime_repo.clone());
        let broker = crate::runtime::CredentialBroker::new(
            runtime_repo.clone(),
            app_api.clone(),
            app_signer.clone(),
            app_id,
            auth_config.clone(),
        );

        // Template release infrastructure.
        let release_repo = crate::templates::repository::ReleaseRepository::new(pool.clone());
        let release_service =
            crate::templates::service::ReleaseService::new(pool.clone(), release_repo.clone());
        let release_approvals = crate::releases::ReleaseApprovalService::new(
            pool.clone(),
            release_service.clone(),
            release_repo.clone(),
        );

        let quotas = crate::quotas::TenantQuotaService::new(pool.clone(), Default::default());
        let tenants_service =
            crate::tenants::TenantService::new(crate::tenants::TenantServiceInputs::new(
                pool.clone(),
                tenants.clone(),
                org_repo.clone(),
                idempotency.clone(),
                quotas.clone(),
            ));
        let runtime_workspaces = crate::runtime::RuntimeWorkspaceService::new(
            pool.clone(),
            tenant_runtime.clone(),
            quotas.clone(),
        );

        ManagerServices {
            db,
            organizations,
            tenants,
            idempotency,
            quotas,
            tenants_service,
            users,
            sessions,
            transactions,
            org_repo,
            orgs_service,
            session_manager,
            login,
            device,
            rate_limiter: crate::rate_limit::RateLimiter::new(pool),
            auth_config,
            github,
            connections,
            connections_service,
            connection_worker,
            webhooks,
            app_signer,
            app_api,
            webhook_secret,
            coder_hook_secret: None,
            secret_provider,
            runtime_repo,
            runtime_credentials,
            broker,
            tenant_runtime,
            runtime_workspaces,
            release_repo,
            release_service,
            release_approvals,
            org_provision: None,
            tenant_provision: None,
        }
    }

    /// Build services from a connected database using configuration and the
    /// secret provider, resolving the GitHub client secret, App private key,
    /// webhook secret, and envelope master key. Hosted-mode secret resolution
    /// failures fail closed.
    pub async fn from_db_with_config(
        db: Db,
        config: &crate::config::ManagerConfig,
        provider: std::sync::Arc<dyn crate::secrets::SecretProvider>,
    ) -> Result<Self, ManagerError> {
        // Resolve the GitHub client secret for the OAuth exchange.
        let client_secret = match &config.github_client_secret_ref {
            Some(ref_name) => String::from_utf8(provider.get(ref_name).await.map_err(|e| {
                ManagerError::Config(format!("failed to resolve GitHub client secret: {e}"))
            })?)
            .map_err(|_| ManagerError::Config("GitHub client secret is not UTF-8".into()))?,
            None => String::new(),
        };

        // Resolve the envelope master key.
        let master_key =
            resolve_master_key(provider.as_ref(), &config.crypto_master_key_ref).await?;

        let mut auth_config = config.auth.clone();
        auth_config.master_key = Some(master_key);

        let github: std::sync::Arc<dyn crate::auth::github::GithubAuth> = std::sync::Arc::new(
            crate::auth::github::RealGithubAuth::new(
                auth_config.client_id.clone(),
                client_secret,
                auth_config.github_api_base.clone(),
            )
            .with_redirect_uri(format!(
                "{}/auth/github/callback",
                auth_config.public_url.trim_end_matches('/')
            )),
        );

        // Resolve the App private key (never logged/persisted) and webhook
        // secret through the secret provider. The App JWT is generated fresh and
        // never persisted.
        let app_id = config.github_app.app_id.unwrap_or(0);
        let private_key = match &config.github_app.private_key_ref {
            Some(ref_name) => provider.get(ref_name).await.map_err(|e| {
                ManagerError::Config(format!("failed to resolve GitHub App private key: {e}"))
            })?,
            None => Vec::new(),
        };
        let app_signer: std::sync::Arc<dyn crate::connections::AppSigner> =
            std::sync::Arc::new(crate::connections::RealAppSigner::new(app_id, private_key));
        let app_api: std::sync::Arc<dyn crate::connections::GithubAppApi> = std::sync::Arc::new(
            crate::connections::RealGithubAppApi::new(auth_config.github_api_base.clone()),
        );
        let webhook_secret = match &config.github_app.webhook_secret_ref {
            Some(ref_name) => Some(provider.get(ref_name).await.map_err(|e| {
                ManagerError::Config(format!("failed to resolve GitHub webhook secret: {e}"))
            })?),
            None => None,
        };

        let services = Self::from_db_with_app_config(
            db,
            auth_config,
            github,
            app_id,
            app_signer,
            app_api,
            webhook_secret,
            config.github_app.clone(),
        );
        let mut services = services;
        services.coder_hook_secret = match &config.coder_hook_secret_ref {
            Some(reference) => Some(provider.get(reference).await.map_err(|e| {
                ManagerError::Config(format!("failed to resolve Coder hook secret: {e}"))
            })?),
            None => None,
        };
        services.secret_provider = provider.clone();
        let redis_isolation: std::sync::Arc<dyn crate::runtime::redis::RedisIsolationBackend> =
            match &config.redis_admin_url {
                Some(url) => std::sync::Arc::new(
                    crate::runtime::redis::FredRedisIsolation::connect(url).await?,
                ),
                None => {
                    std::sync::Arc::new(crate::runtime::redis::InMemoryRedisIsolation::default())
                }
            };
        services.tenant_runtime = crate::runtime::tenant::TenantRuntimeRepository::with_backends(
            services.db.pool().clone(),
            provider.clone(),
            redis_isolation,
        );
        services.tenant_runtime.restore_redis_credentials().await?;
        services.runtime_credentials =
            crate::runtime::RuntimeCredentialService::with_secret_provider(
                services.runtime_repo.clone(),
                provider.clone(),
            );
        // Bind the operator-configured tenant limits (platform-operator
        // configuration) into the central quota service.
        services.quotas = crate::quotas::TenantQuotaService::new(
            services.db.pool().clone(),
            config.tenant_limits.clone(),
        );
        services.tenants_service =
            crate::tenants::TenantService::new(crate::tenants::TenantServiceInputs::new(
                services.db.pool().clone(),
                services.tenants.clone(),
                services.org_repo.clone(),
                services.idempotency.clone(),
                services.quotas.clone(),
            ));
        services.runtime_workspaces = crate::runtime::RuntimeWorkspaceService::new(
            services.db.pool().clone(),
            services.tenant_runtime.clone(),
            services.quotas.clone(),
        );
        services.with_org_provision(config, provider).await
    }

    /// Build and attach the durable organization-provisioning worker using the
    /// configured Coder adapter and operator-configured release.
    ///
    /// When the Coder adapter configuration (URL + operator token ref) is
    /// absent (pure local/test mode) this returns `self` unchanged with
    /// `org_provision: None`. When configured, it resolves the platform Coder
    /// operator token through the secret provider (never logged/persisted),
    /// imports the operator-configured release, and attaches the worker. A
    /// broken release source or operator token is a startup error (fail closed),
    /// so organizations never silently become ready.
    pub async fn with_org_provision(
        mut self,
        config: &crate::config::ManagerConfig,
        provider: std::sync::Arc<dyn crate::secrets::SecretProvider>,
    ) -> Result<Self, ManagerError> {
        let Some(url) = &config.coder_url else {
            return Ok(self);
        };
        let Some(ref_name) = &config.coder_operator_token_ref else {
            return Ok(self);
        };
        crate::config::runtime_hook_url(uuid::Uuid::nil()).map_err(ManagerError::Config)?;
        let token_bytes = provider.get(ref_name).await.map_err(|e| {
            ManagerError::Config(format!("failed to resolve Coder operator token: {e}"))
        })?;
        let token = coder_operator_token(token_bytes)?;

        let shared_token = std::sync::Arc::new(std::sync::RwLock::new(token.clone()));
        let mut real_adapter =
            crate::coder::RealCoderAdapter::from_parts(url, &token, &config.coder_version_baseline)
                .with_shared_operator_token(shared_token.clone());
        if let Some(email) = crate::operator::managed_bootstrap_email()
            .await
            .map_err(ManagerError::Service)?
        {
            let provider = provider.clone();
            let ref_name = ref_name.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(5 * 60));
                interval.tick().await;
                loop {
                    interval.tick().await;
                    match crate::operator::setup(true, Some(&email)).await {
                        Ok(()) => match provider.get(&ref_name).await {
                            Ok(bytes) => match coder_operator_token(bytes) {
                                Ok(token) => {
                                    *shared_token.write().unwrap_or_else(|e| e.into_inner()) =
                                        token;
                                }
                                Err(error) => {
                                    tracing::error!(%error, "renewed Coder operator key is invalid")
                                }
                            },
                            Err(error) => {
                                tracing::error!(%error, "cannot reload Coder operator key")
                            }
                        },
                        Err(error) => tracing::warn!(%error, "Coder operator key renewal failed"),
                    }
                }
            });
        }
        if config.provisioner_backend == "docker" {
            let master =
                resolve_master_key(provider.as_ref(), &config.crypto_master_key_ref).await?;
            let cipher =
                crate::auth::crypto::EnvelopeCipher::derive(&master.0, "coder-provisioner", 1);
            real_adapter = real_adapter.with_docker_provisioner(
                crate::provisioning::docker::DockerProvisioner::from_env(
                    self.db.pool().clone(),
                    cipher,
                    url,
                    &config.coder_version_baseline,
                )?,
            );
        }
        let adapter: std::sync::Arc<dyn crate::coder::CoderAdapter> =
            std::sync::Arc::new(real_adapter);
        let template_token = match &config.release_source {
            crate::config::ReleaseSourceConfig::Github(hosted) => match &hosted.github_token_ref {
                Some(ref_name) => Some(
                    String::from_utf8(provider.get(ref_name).await.map_err(|e| {
                        ManagerError::Config(format!(
                            "failed to resolve GitHub template token: {e}"
                        ))
                    })?)
                    .map_err(|_| {
                        ManagerError::Config("GitHub template token is not UTF-8".into())
                    })?,
                ),
                None => None,
            },
            crate::config::ReleaseSourceConfig::Local => None,
        };
        let source = crate::provisioning::ReleaseSource::from_config(
            &config.release_source,
            template_token,
        )?;
        let cache = match &config.release_source {
            crate::config::ReleaseSourceConfig::Github(hosted) => {
                crate::provisioning::ReleaseCache::from_config(hosted)
            }
            crate::config::ReleaseSourceConfig::Local => {
                crate::provisioning::ReleaseCache::new(std::path::PathBuf::new(), 0, 0)
            }
        };
        let resolved = source.resolve(&cache).await?;
        let (release_id, _digest) = self
            .release_service
            .import_release(resolved.artifacts.clone(), &resolved.source_reference)
            .await?;
        // A release supplied through the trusted operator configuration is the
        // platform-selected release for this deployment. Customer-facing
        // promotion still requires the platform-operator approval service.
        self.release_repo
            .approve_configured_release(release_id)
            .await?;

        let worker = crate::provisioning::OrgProvisionWorker::new(
            self.db.pool().clone(),
            self.organizations.clone(),
            self.release_service.clone(),
            self.release_repo.clone(),
            adapter.clone(),
            Some(std::sync::Arc::new(resolved.artifacts)),
            Some(release_id),
            config.provisioner_backend.clone(),
            config.coder_version_baseline.clone(),
        );
        self.org_provision = Some(worker);

        // Durable tenant-provisioning worker sharing the same platform Coder
        // adapter, quota service, and tenant runtime repository.
        let tenant_worker = crate::provisioning::TenantProvisionWorker::new(
            self.db.pool().clone(),
            self.organizations.clone(),
            self.quotas.clone(),
            self.tenant_runtime.clone(),
            self.runtime_credentials.clone(),
            self.release_repo.clone(),
            adapter,
        )
        .with_runtime_endpoints(
            crate::config::runtime_manager_url(&config.auth.public_url)
                .map_err(crate::error::ManagerError::Config)?,
            config
                .runtime_redis_url
                .clone()
                .unwrap_or_else(|| "redis://127.0.0.1:6379".into()),
        )
        .with_local_docker_cleanup(
            config.mode == crate::config::Mode::Local && config.provisioner_backend == "docker",
        );
        self.tenant_provision = Some(tenant_worker);
        Ok(self)
    }

    pub fn from_db(db: Db) -> Self {
        // Local/test default: use a fixture GitHub adapter and the dev master
        // key. Handlers that require real GitHub will fail closed when the
        // adapter is a fixture without configured endpoints.
        let auth_config = crate::config::local_test_config().auth;
        let github: std::sync::Arc<dyn crate::auth::github::GithubAuth> =
            std::sync::Arc::new(UnconfiguredGithub);
        Self::from_db_with_auth(db, auth_config, github)
    }
}

/// A GitHub adapter that fails closed when no provider is configured. Used by
/// `ManagerServices::from_db` in pure-local/test mode so unauthenticated GitHub
/// calls never silently succeed.
struct UnconfiguredGithub;

#[async_trait::async_trait]
impl crate::auth::github::GithubAuth for UnconfiguredGithub {
    fn authorize_url(&self, _: &crate::auth::github::AuthorizeRequest) -> String {
        "/auth/github/start".to_string()
    }
    async fn exchange_code(
        &self,
        _: &str,
        _: &str,
    ) -> Result<crate::auth::github::UserToken, ManagerError> {
        Err(ManagerError::Config(
            "GitHub authentication is not configured".into(),
        ))
    }
    async fn fetch_user(&self, _: &str) -> Result<crate::auth::github::GithubUser, ManagerError> {
        Err(ManagerError::Config(
            "GitHub authentication is not configured".into(),
        ))
    }
    async fn resolve_login(
        &self,
        _: &str,
    ) -> Result<crate::auth::github::GithubUserLookup, ManagerError> {
        Err(ManagerError::Config(
            "GitHub authentication is not configured".into(),
        ))
    }
}

/// A GitHub App API adapter that fails closed when no provider is configured.
/// Used in pure-local/test mode so unauthenticated App calls never silently
/// succeed.
struct UnconfiguredAppApi;

#[async_trait::async_trait]
impl crate::connections::GithubAppApi for UnconfiguredAppApi {
    async fn installation(
        &self,
        _: &crate::connections::SignedAppJwt,
        _: i64,
    ) -> Result<Option<crate::connections::github_app::Installation>, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn accessible_installations(
        &self,
        _: &str,
    ) -> Result<Vec<crate::connections::github_app::AccessibleInstallation>, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn fetch_user(&self, _: &str) -> Result<crate::auth::github::GithubUser, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn organization_membership(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<crate::connections::github_app::OrgMembership>, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn installation_repositories(
        &self,
        _: &crate::connections::SignedAppJwt,
        _: i64,
        _: u32,
        _: u32,
    ) -> Result<Vec<crate::connections::github_app::RepoRef>, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn exchange_installation_token(
        &self,
        _: &crate::connections::SignedAppJwt,
        _: i64,
        _: &[i64],
        _: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::connections::github_app::InstallationToken, ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
    async fn revoke_installation_token(
        &self,
        _: &crate::connections::SignedAppJwt,
        _: i64,
        _: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::Config("GitHub App is not configured".into()))
    }
}

#[derive(Clone)]
pub struct AppState {
    // Kept in state so future manager APIs can share the same tenant-scoped
    // store as readiness.
    store: SharedStore,
    readiness: Arc<dyn ReadinessCheck>,
    /// The loaded configuration; mode drives whether the database-backed
    /// services are present.
    config: ManagerConfig,
    /// Database-backed services, present only when a control-plane database is
    /// configured. `None` in pure-local test mode.
    services: Option<Arc<ManagerServices>>,
}

impl AppState {
    pub async fn from_env() -> Result<Self, ManagerError> {
        let config = ManagerConfig::from_env().map_err(|e| ManagerError::Config(e.to_string()))?;

        // Reuse the workspace config crate so the manager follows the same
        // environment precedence and tenant defaults as the other OpenFlows
        // components.
        let env = config::EnvConfig::from_env()
            .map_err(|error| ManagerError::Config(error.to_string()))?;
        let redis_url = env.infra.effective_redis_url();
        let tenant = env.tenant.effective_tenant().to_string();
        let store = SharedStore::new_redis_with_tenant(&redis_url, Some(tenant)).await?;

        // Connect the control-plane database when configured. In hosted mode the
        // database is required and a failure here is fatal (never a local
        // fallback). In local mode an explicit database is optional.
        let services = match &config.database_url {
            Some(url) => {
                let db = Db::connect(url).await?;
                let provider = resolve_secret_provider(&config.secret_provider).await?;
                if let Some(path) = std::env::var_os("OPENFLOWS_DEV_SECRETS_FILE") {
                    let contents = std::fs::read(path).map_err(|_| {
                        ManagerError::Config("cannot read development secrets file".into())
                    })?;
                    // The development file is a seed for both local providers:
                    // in-memory for ephemeral tests and filesystem for a local
                    // Manager that must survive restarts.
                    populate_development_secrets(
                        provider.as_ref(),
                        &contents,
                        &config.crypto_master_key_ref,
                    )
                    .await?;
                }
                let services =
                    ManagerServices::from_db_with_config(db, &config, provider.into()).await?;
                Some(Arc::new(services))
            }
            None => None,
        };

        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });

        Ok(Self {
            store,
            readiness,
            config,
            services,
        })
    }

    pub fn for_tests() -> Self {
        Self::new(SharedStore::new_in_memory_with_tenant("test"))
    }

    pub fn new(store: SharedStore) -> Self {
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        let config = crate::config::local_test_config();
        Self {
            store,
            readiness,
            config,
            services: None,
        }
    }

    pub fn with_readiness_check(store: SharedStore, readiness: Arc<dyn ReadinessCheck>) -> Self {
        Self {
            store,
            readiness,
            config: crate::config::local_test_config(),
            services: None,
        }
    }

    /// Attach database-backed services, used by hosted/local-with-db setups and
    /// integration tests.
    pub fn with_services(
        store: SharedStore,
        readiness: Arc<dyn ReadinessCheck>,
        services: ManagerServices,
        config: ManagerConfig,
    ) -> Self {
        Self {
            store,
            readiness,
            config,
            services: Some(Arc::new(services)),
        }
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    pub fn services(&self) -> Option<&Arc<ManagerServices>> {
        self.services.as_ref()
    }

    pub fn mode(&self) -> crate::config::Mode {
        self.config.mode
    }

    /// Bound readiness latency so a hung dependency cannot make the endpoint
    /// hang indefinitely.
    ///
    /// In hosted mode this also requires the control-plane database to be
    /// reachable; upstream capability failures are reported by the endpoint
    /// separately from process liveness.
    pub async fn check_readiness(&self) -> Result<(), ManagerError> {
        // Existing dependency probe (Redis store).
        timeout(READINESS_CHECK_TIMEOUT, self.readiness.check())
            .await
            .map_err(|_| {
                ManagerError::Service(anyhow::anyhow!(
                    "readiness check timed out after {:?}",
                    READINESS_CHECK_TIMEOUT
                ))
            })??;

        // Control-plane database, when present.
        if let Some(services) = &self.services {
            timeout(READINESS_CHECK_TIMEOUT, services.db.ping())
                .await
                .map_err(|_| {
                    ManagerError::Service(anyhow::anyhow!(
                        "database readiness check timed out after {:?}",
                        READINESS_CHECK_TIMEOUT
                    ))
                })??;
        }

        Ok(())
    }

    /// Produce a structured readiness report listing each dependency
    /// individually. Used by `/ready` so upstream capability failures are
    /// reported separately rather than collapsing into a single "unready".
    pub async fn readiness_report(&self) -> ReadinessReport {
        let mut deps = Vec::new();

        // Existing dependency probe (Redis store), bounded.
        let store = timeout(READINESS_CHECK_TIMEOUT, self.readiness.check()).await;
        deps.push(match store {
            Ok(Ok(())) => DependencyStatus::ok("store"),
            Ok(Err(e)) => DependencyStatus::err("store", &e.to_string()),
            Err(_) => DependencyStatus::err("store", "timed out"),
        });

        // Control-plane database, when present.
        if let Some(services) = &self.services {
            let db = timeout(READINESS_CHECK_TIMEOUT, services.db.ping()).await;
            deps.push(match db {
                Ok(Ok(())) => DependencyStatus::ok("database"),
                Ok(Err(e)) => DependencyStatus::err("database", &e.to_string()),
                Err(_) => DependencyStatus::err("database", "timed out"),
            });
        }

        ReadinessReport {
            ready: deps.iter().all(|d| d.ok),
            dependencies: deps,
        }
    }
}

/// The readiness of a single dependency.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DependencyStatus {
    pub name: &'static str,
    pub ok: bool,
    pub error: Option<String>,
}

impl DependencyStatus {
    fn ok(name: &'static str) -> Self {
        DependencyStatus {
            name,
            ok: true,
            error: None,
        }
    }

    fn err(name: &'static str, error: &str) -> Self {
        DependencyStatus {
            name,
            ok: false,
            error: Some(error.to_string()),
        }
    }
}

/// A structured readiness report for the `/ready` endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadinessReport {
    pub ready: bool,
    pub dependencies: Vec<DependencyStatus>,
}

pub fn create_router(state: AppState) -> Router {
    crate::routes::router()
        .layer(axum::middleware::from_fn(request_context))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = request.extensions().get::<axum::extract::MatchedPath>().map(|p| p.as_str()).unwrap_or("unmatched"),
                        status = tracing::field::Empty,
                        latency_ms = tracing::field::Empty,
                    )
                })
                .on_response(|response: &axum::http::Response<_>, latency: std::time::Duration, span: &tracing::Span| {
                    span.record("status", response.status().as_u16());
                    span.record("latency_ms", latency.as_millis() as u64);
                    tracing::info!(parent: span, "request completed");
                })
                .on_failure(|error: tower_http::classify::ServerErrorsFailureClass, latency: std::time::Duration, span: &tracing::Span| {
                    tracing::warn!(parent: span, %error, latency_ms = latency.as_millis() as u64, "request failed");
                }),
        )
        .with_state(state)
}

pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ManagerError> {
    let connection_worker = state
        .services()
        .map(|services| services.connection_worker.clone());
    let connection_loop = async move {
        match connection_worker {
            Some(worker) => worker.run_forever().await,
            None => std::future::pending().await,
        }
    };
    // Durable organization-provisioning worker (startup recovery resumes
    // unfinished org.provision operations). `None` in pure local/test mode.
    let org_provision = state
        .services()
        .and_then(|services| services.org_provision.clone());
    let promotion_dependencies = state.services().and_then(|services| {
        services.org_provision.clone().map(|worker| {
            (
                services.release_approvals.pool.clone(),
                services.release_approvals.clone(),
                worker,
            )
        })
    });
    let promotion_loop = async move {
        match promotion_dependencies {
            Some((pool, approvals, worker)) => {
                crate::routes::platform_releases::run_promotion_worker(pool, approvals, worker)
                    .await
            }
            None => std::future::pending().await,
        }
    };
    let provisioner_adapter = org_provision.as_ref().map(|worker| worker.adapter.clone());
    let provisioner_recovery_loop = async move {
        let Some(adapter) = provisioner_adapter else {
            return std::future::pending::<Result<(), ManagerError>>().await;
        };
        loop {
            if let Err(error) = adapter.reconcile_provisioners().await {
                tracing::warn!(%error, "provisioner recovery failed; will retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    };
    let org_provision_loop = async move {
        match org_provision {
            Some(worker) => worker.run_forever().await,
            None => std::future::pending().await,
        }
    };
    // Durable tenant-provisioning worker (startup recovery resumes unfinished
    // tenant operations). `None` in pure local/test mode.
    let tenant_provision = state
        .services()
        .and_then(|services| services.tenant_provision.clone());
    let tenant_provision_loop = async move {
        match tenant_provision {
            Some(worker) => worker.run_forever().await,
            None => std::future::pending().await,
        }
    };
    let server = axum::serve(
        listener,
        create_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown);
    tokio::select! {
        result = async { server.await } => result?,
        result = connection_loop => result?,
        result = org_provision_loop => result?,
        result = promotion_loop => result?,
        result = provisioner_recovery_loop => result?,
        result = tenant_provision_loop => result?,
    }
    Ok(())
}

pub async fn bind_and_serve(
    addr: SocketAddr,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ManagerError> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "OpenFlows Manager listening");
    serve(listener, state, shutdown).await
}

tokio::task_local! { pub static REQUEST_ID: String; }

async fn request_context(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_test_console = request.uri().path() == "/";
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip().to_string())
        .unwrap_or_else(|| "unknown-peer".into());
    request
        .headers_mut()
        .insert("x-openflows-peer", peer.parse().expect("IP header"));
    let request_id = uuid::Uuid::new_v4().to_string();
    request
        .headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());
    let mut response = REQUEST_ID
        .scope(request_id.clone(), next.run(request))
        .await;
    if (response.status().is_client_error() || response.status().is_server_error())
        && !response
            .headers()
            .get("content-type")
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        use axum::response::IntoResponse;
        let status = response.status();
        let code = if status == axum::http::StatusCode::NOT_FOUND {
            "RESOURCE_NOT_FOUND"
        } else if status == axum::http::StatusCode::METHOD_NOT_ALLOWED {
            "METHOD_NOT_ALLOWED"
        } else {
            "INVALID_INPUT"
        };
        response = (status,axum::Json(serde_json::json!({"error":{
            "code":code,"message":"request could not be accepted","request_id":request_id,"retryable":false
        }}))).into_response();
    }
    response
        .headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    let csp = if is_test_console {
        "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
    } else {
        "default-src 'none'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
    };
    response
        .headers_mut()
        .insert("content-security-policy", csp.parse().unwrap());
    response
}

#[cfg(test)]
mod development_secret_tests {
    use super::*;

    #[tokio::test]
    async fn development_file_populates_oauth_and_master_key_references() {
        let provider = resolve_secret_provider(IN_MEMORY_SECRET_PROVIDER)
            .await
            .unwrap();
        populate_development_secrets(
            provider.as_ref(),
            br#"{"oauth":"client-secret","master":"01234567890123456789012345678901"}"#,
            &Some("master".into()),
        )
        .await
        .unwrap();
        assert_eq!(provider.get("oauth").await.unwrap(), b"client-secret");
        let key = resolve_master_key(provider.as_ref(), &Some("master".into()))
            .await
            .unwrap();
        assert_eq!(key.0, *b"01234567890123456789012345678901");
        assert!(provider.get("missing").await.is_err());
        let error = populate_development_secrets(provider.as_ref(), b"private-credential", &None)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-credential"));
    }

    #[tokio::test]
    async fn configured_master_key_ref_is_protected_from_replacement() {
        let provider = resolve_secret_provider(IN_MEMORY_SECRET_PROVIDER)
            .await
            .unwrap();
        let ref_name = "crypto-master-key";
        // Seed the configured master-key reference.
        populate_development_secrets(
            provider.as_ref(),
            br#"{"crypto-master-key":"01234567890123456789012345678901"}"#,
            &Some(ref_name.into()),
        )
        .await
        .unwrap();
        // A restart with a different seed must refuse to replace it, not
        // silently overwrite the key (which would break existing records).
        let error = populate_development_secrets(
            provider.as_ref(),
            br#"{"crypto-master-key":"99999999999999999999999999999999"}"#,
            &Some(ref_name.into()),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("refusing to replace"),
            "expected a refusal, got: {error}"
        );
    }
}
