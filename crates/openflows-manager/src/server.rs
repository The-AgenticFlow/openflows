use crate::error::ManagerError;
use axum::Router;
use pocketflow_core::SharedStore;
use std::{future::Future, net::SocketAddr};
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct AppState {
    store: SharedStore,
}

impl AppState {
    pub async fn from_env() -> Result<Self, ManagerError> {
        let env = config::EnvConfig::from_env()
            .map_err(|error| ManagerError::Config(error.to_string()))?;
        let redis_url = env.infra.effective_redis_url();
        let tenant = env.tenant.effective_tenant().to_string();
        let store = SharedStore::new_redis_with_tenant(&redis_url, Some(tenant)).await?;

        Ok(Self { store })
    }

    pub fn for_tests() -> Self {
        Self {
            store: SharedStore::new_in_memory_with_tenant("test"),
        }
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
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
