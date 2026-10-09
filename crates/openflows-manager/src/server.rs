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
/// expose organization-scoped operations.
#[derive(Clone)]
pub struct ManagerServices {
    pub db: Db,
    pub organizations: OrganizationsRepository,
    pub tenants: TenantsRepository,
    pub idempotency: IdempotencyService,
}

impl ManagerServices {
    /// Connect to the control-plane database, apply migrations, and build the
    /// domain services. Returns an error when the database is unavailable or
    /// migrations fail; the Manager must remain unready in that case.
    pub async fn connect(database_url: &str) -> Result<Self, ManagerError> {
        let db = Db::connect(database_url).await?;
        Ok(ManagerServices::from_db(db))
    }

    pub fn from_db(db: Db) -> Self {
        let pool = db.pool().clone();
        ManagerServices {
            organizations: OrganizationsRepository::new(pool.clone()),
            tenants: TenantsRepository::new(pool.clone()),
            idempotency: IdempotencyService::new(pool),
            db,
        }
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
            Some(url) => Some(Arc::new(ManagerServices::connect(url).await?)),
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
        let config = ManagerConfig {
            mode: crate::config::Mode::Local,
            database_url: None,
            secret_provider: crate::config::DEFAULT_SECRET_PROVIDER.to_string(),
            http_addr: "127.0.0.1:3002".to_string(),
        };
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
            config: ManagerConfig {
                mode: crate::config::Mode::Local,
                database_url: None,
                secret_provider: crate::config::DEFAULT_SECRET_PROVIDER.to_string(),
                http_addr: "127.0.0.1:3002".to_string(),
            },
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
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %request.uri().path(),
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
    axum::serve(listener, create_router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
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
