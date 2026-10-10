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
/// expose organization-scoped operations. In addition to the WP-01
/// repositories, this holds the human-authentication and organization services
/// added by WP-02.
#[derive(Clone)]
pub struct ManagerServices {
    pub db: Db,
    pub organizations: OrganizationsRepository,
    pub tenants: TenantsRepository,
    pub idempotency: IdempotencyService,
    // authentication and organization services.
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
}

/// The secret-provider adapter name for the isolated in-memory provider.
pub const IN_MEMORY_SECRET_PROVIDER: &str = "in-memory";

/// Resolve a [`SecretProvider`] by adapter name. Only the in-memory provider is
/// implemented; any other name fails closed (a durable production adapter is a
/// deployment decision, not something to invent here).
pub fn resolve_secret_provider(
    name: &str,
) -> Result<Box<dyn crate::secrets::SecretProvider>, ManagerError> {
    match name {
        IN_MEMORY_SECRET_PROVIDER => Ok(Box::new(crate::secrets::InMemorySecretProvider::new())),
        other => Err(ManagerError::Config(format!(
            "secret provider '{other}' is not implemented; refusing to start"
        ))),
    }
}

/// Load a local JSON object mapping secret references to UTF-8 values.
/// Parse errors deliberately exclude file contents, which contain credentials.
async fn populate_development_secrets(
    provider: &dyn crate::secrets::SecretProvider,
    contents: &[u8],
) -> Result<(), ManagerError> {
    let values: std::collections::HashMap<String, String> = serde_json::from_slice(contents)
        .map_err(|_| ManagerError::Config("invalid development secrets file".into()))?;
    for (name, value) in values {
        provider
            .put(&name, value.as_bytes())
            .await
            .map_err(|_| ManagerError::Config("cannot populate development secrets".into()))?;
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

    /// Build services with an explicit App signer and App API (WP-03). Used by
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
        Self::from_db_with_app_config(db, auth_config, github, app_id, app_signer, app_api,
            webhook_secret, crate::config::github_app_from_env())
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
        let webhooks = crate::connections::WebhookService::new(
            pool.clone(),
            connections.clone(),
            webhook_secret.clone().unwrap_or_default(),
            github_app.webhook_body_limit,
            app_id,
            sync.clone(),
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
        );

        ManagerServices {
            db,
            organizations,
            tenants,
            idempotency,
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
        }
    }

    /// Build services from a connected database using configuration and the
    /// secret provider, resolving the GitHub client secret, App private key,
    /// webhook secret, and envelope master key. Hosted-mode secret resolution
    /// failures fail closed.
    pub async fn from_db_with_config(
        db: Db,
        config: &crate::config::ManagerConfig,
        provider: &dyn crate::secrets::SecretProvider,
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
        let master_key = resolve_master_key(provider, &config.crypto_master_key_ref).await?;

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
        let app_signer: std::sync::Arc<dyn crate::connections::AppSigner> = std::sync::Arc::new(
            crate::connections::RealAppSigner::new(app_id, private_key),
        );
        let app_api: std::sync::Arc<dyn crate::connections::GithubAppApi> = std::sync::Arc::new(
            crate::connections::RealGithubAppApi::new(auth_config.github_api_base.clone()),
        );
        let webhook_secret = match &config.github_app.webhook_secret_ref {
            Some(ref_name) => Some(provider.get(ref_name).await.map_err(|e| {
                ManagerError::Config(format!("failed to resolve GitHub webhook secret: {e}"))
            })?),
            None => None,
        };

        Ok(Self::from_db_with_app_config(
            db,
            auth_config,
            github,
            app_id,
            app_signer,
            app_api,
            webhook_secret,
            config.github_app.clone(),
        ))
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
    ) -> Result<crate::connections::github_app::InstallationToken, ManagerError> {
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
                let provider = resolve_secret_provider(&config.secret_provider)?;
                if config.secret_provider == IN_MEMORY_SECRET_PROVIDER {
                    if let Some(path) = std::env::var_os("OPENFLOWS_DEV_SECRETS_FILE") {
                        let contents = std::fs::read(path).map_err(|_| {
                            ManagerError::Config("cannot read development secrets file".into())
                        })?;
                        populate_development_secrets(provider.as_ref(), &contents).await?;
                    }
                }
                let services =
                    ManagerServices::from_db_with_config(db, &config, provider.as_ref()).await?;
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
    let worker = state.services().map(|services| services.connection_worker.clone());
    let worker_loop = async move {
        match worker {
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
        result = worker_loop => result?,
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
    // The local test console is intentionally a single inline HTML/JS page.
    // Permit its script while keeping all other resource types blocked.
    response.headers_mut().insert("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    response
}

#[cfg(test)]
mod development_secret_tests {
    use super::*;

    #[tokio::test]
    async fn development_file_populates_oauth_and_master_key_references() {
        let provider = resolve_secret_provider(IN_MEMORY_SECRET_PROVIDER).unwrap();
        populate_development_secrets(
            provider.as_ref(),
            br#"{"oauth":"client-secret","master":"01234567890123456789012345678901"}"#,
        )
        .await
        .unwrap();
        assert_eq!(provider.get("oauth").await.unwrap(), b"client-secret");
        let key = resolve_master_key(provider.as_ref(), &Some("master".into()))
            .await
            .unwrap();
        assert_eq!(key.0, *b"01234567890123456789012345678901");
        assert!(provider.get("missing").await.is_err());
        let error = populate_development_secrets(provider.as_ref(), b"private-credential")
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-credential"));
    }
}
