//! Binary entry point for running the OpenFlows Manager service.

use openflows_manager::{error::ManagerError, server};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), ManagerError> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("OPENFLOWS_MANAGER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3002".to_string())
        .parse::<SocketAddr>()
        .map_err(|error| {
            ManagerError::Config(format!("invalid OPENFLOWS_MANAGER_ADDR: {error}"))
        })?;
    let state = server::AppState::from_env().await?;

    server::bind_and_serve(addr, state, shutdown_signal()).await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl-C shutdown handler");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let terminate = async {
            match signal(SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to install SIGTERM shutdown handler");
                    std::future::pending::<()>().await;
                }
            }
        };

        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
