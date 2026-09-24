//! Server bootstrap, application state, and graceful serving utilities.

use crate::{
    error::ManagerError,
    services::{FleetService, KanbanService, TenantProvisioner, TenantService},
};
use axum::Router;
use pocketflow_core::SharedStore;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::{
    net::TcpListener,
    time::{timeout, Duration},
};

const READINESS_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

/// Async dependency probe used by `/ready`.
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
    tenant_service: TenantService,
    fleet_service: FleetService,
    kanban_service: KanbanService,
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
        let store = SharedStore::new_in_memory_with_tenant("test");
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        let tenant_service = TenantService::for_tests(store.clone());
        let fleet_service = FleetService::new(store.clone());
        let kanban_service = KanbanService::new(store.clone());

        Self {
            store,
            readiness,
            tenant_service,
            fleet_service,
            kanban_service,
        }
    }

    pub fn new(store: SharedStore) -> Self {
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        let tenant_service = TenantService::with_coder_provisioner(store.clone());
        let fleet_service = FleetService::new(store.clone());
        let kanban_service = KanbanService::new(store.clone());

        Self {
            store,
            readiness,
            tenant_service,
            fleet_service,
            kanban_service,
        }
    }

    pub fn with_tenant_provisioner(
        store: SharedStore,
        provisioner: Arc<dyn TenantProvisioner>,
    ) -> Self {
        let readiness = Arc::new(StoreReadiness {
            store: store.clone(),
        });
        let tenant_service = TenantService::new(store.clone(), provisioner);
        let fleet_service = FleetService::new(store.clone());
        let kanban_service = KanbanService::new(store.clone());

        Self {
            store,
            readiness,
            tenant_service,
            fleet_service,
            kanban_service,
        }
    }

    pub fn with_readiness_check(store: SharedStore, readiness: Arc<dyn ReadinessCheck>) -> Self {
        let tenant_service = TenantService::with_coder_provisioner(store.clone());
        let fleet_service = FleetService::new(store.clone());
        let kanban_service = KanbanService::new(store.clone());

        Self {
            store,
            readiness,
            tenant_service,
            fleet_service,
            kanban_service,
        }
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    pub fn tenant_service(&self) -> &TenantService {
        &self.tenant_service
    }

    pub fn fleet_service(&self) -> &FleetService {
        &self.fleet_service
    }

    pub fn kanban_service(&self) -> &KanbanService {
        &self.kanban_service
    }

    pub async fn check_readiness(&self) -> Result<(), ManagerError> {
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
    crate::routes::router()
        .layer(axum::middleware::from_fn(
            crate::middleware::request_id::request_id_middleware,
        ))
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
