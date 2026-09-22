//! Server bootstrap, application state, and graceful serving utilities.

use crate::error::ManagerError;
use axum::Router;
use pocketflow_core::SharedStore;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::{
    net::TcpListener,
    time::{timeout, Duration},
};

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
    // Keeping the concrete store behind this private adapter lets AppState
    // expose only the trait object used by handlers.
    store: SharedStore,
}

impl ReadinessCheck for StoreReadiness {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ManagerError>> + Send + '_>> {
        Box::pin(async move {
            // A successful ping proves the backing store can respond inside the
            // readiness deadline. The endpoint intentionally avoids exposing
            // connection details in the public response body.
            self.store.ping().await?;
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    // Kept in state even before many routes need it so future manager APIs can
    // share the same tenant-scoped store as readiness.
    store: SharedStore,
    // A trait object keeps handler code stable while allowing tests and future
    // deployments to provide richer readiness behavior.
    readiness: Arc<dyn ReadinessCheck>,
}

impl AppState {
    pub async fn from_env() -> Result<Self, ManagerError> {
        // Reuse the workspace config crate so the manager follows the same
        // environment precedence and tenant defaults as the other OpenFlows
        // components.
        let env = config::EnvConfig::from_env()
            .map_err(|error| ManagerError::Config(error.to_string()))?;
        let redis_url = env.infra.effective_redis_url();
        let tenant = env.tenant.effective_tenant().to_string();

        // Scope the store by tenant at construction time. Handlers receive only
        // AppState, so tenant selection should not be repeated per request.
        let store = SharedStore::new_redis_with_tenant(&redis_url, Some(tenant)).await?;

        Ok(Self::new(store))
    }

    pub fn for_tests() -> Self {
        // Tests should exercise router and handler behavior without requiring a
        // Redis instance. The tenant name is fixed so assertions stay
        // deterministic.
        Self::new(SharedStore::new_in_memory_with_tenant("test"))
    }

    pub fn new(store: SharedStore) -> Self {
        // The default readiness implementation mirrors production behavior:
        // report ready only when the shared store can be pinged.
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        Self { store, readiness }
    }

    pub fn with_readiness_check(store: SharedStore, readiness: Arc<dyn ReadinessCheck>) -> Self {
        // This constructor is intentionally public for smoke tests and any
        // embedding scenarios that need to compose the manager with a custom
        // health policy.
        Self { store, readiness }
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    pub async fn check_readiness(&self) -> Result<(), ManagerError> {
        // Bound readiness latency so a hung dependency cannot make the endpoint
        // hang indefinitely. This keeps orchestrator probes responsive and lets
        // callers distinguish liveness from dependency availability.
        match timeout(READINESS_CHECK_TIMEOUT, self.readiness.check()).await {
            Ok(result) => result,
            Err(_) => Err(ManagerError::Service(anyhow::anyhow!(
                "readiness check timed out after {:?}",
                READINESS_CHECK_TIMEOUT
            ))),
        }
    }
}

pub fn create_router(state: AppState) -> Router {
    // Apply state at the outer edge so nested routers can stay focused on route
    // definitions and handlers can opt into State<AppState> only when needed.
    crate::routes::router().with_state(state)
}

pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ManagerError> {
    // Axum owns the accept loop here. The caller supplies the listener so tests
    // can bind to port 0 and production can keep address parsing in main.
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
    // Binding is split from serving to give tests direct control over listener
    // setup while keeping the binary entry point compact.
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "OpenFlows Manager listening");
    serve(listener, state, shutdown).await
}
