//! Server bootstrap, application state, and graceful serving utilities.

use crate::error::ManagerError;
use axum::Router;
use pocketflow_core::SharedStore;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::net::TcpListener;

pub trait ReadinessCheck: Send + Sync {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ManagerError>> + Send + '_>>;
}

#[derive(Clone)]
struct StoreReadiness {
    store: SharedStore,
}

impl ReadinessCheck for StoreReadiness {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ManagerError>> + Send + '_>> {
        Box::pin(async move {
            self.store.ping().await?;
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    store: SharedStore,
    readiness: Arc<dyn ReadinessCheck>,
}

impl AppState {
    pub async fn from_env() -> Result<Self, ManagerError> {
        let env = config::EnvConfig::from_env()
            .map_err(|error| ManagerError::Config(error.to_string()))?;
        let redis_url = env.infra.effective_redis_url();
        let tenant = env.tenant.effective_tenant().to_string();
        let store = SharedStore::new_redis_with_tenant(&redis_url, Some(tenant)).await?;

        Ok(Self::new(store))
    }

    pub fn for_tests() -> Self {
        Self::new(SharedStore::new_in_memory_with_tenant("test"))
    }

    pub fn new(store: SharedStore) -> Self {
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        Self { store, readiness }
    }

    pub fn with_readiness_check(store: SharedStore, readiness: Arc<dyn ReadinessCheck>) -> Self {
        Self { store, readiness }
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    pub async fn check_readiness(&self) -> Result<(), ManagerError> {
        self.readiness.check().await
    }
}

pub fn create_router(state: AppState) -> Router {
    crate::routes::router().with_state(state)
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
