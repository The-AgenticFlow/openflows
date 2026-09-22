//! Binary entry point for running the OpenFlows Manager service.

use openflows_manager::{error::ManagerError, server};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), ManagerError> {
    // Initialize the default tracing subscriber before any fallible setup so
    // configuration and bind failures are visible to operators.
    tracing_subscriber::fmt::init();

    // The manager defaults to localhost to avoid exposing the service by
    // accident in development. Deployments can opt into a different interface
    // or port with OPENFLOWS_MANAGER_ADDR.
    let addr = std::env::var("OPENFLOWS_MANAGER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3002".to_string())
        .parse::<SocketAddr>()
        .map_err(|error| {
            ManagerError::Config(format!("invalid OPENFLOWS_MANAGER_ADDR: {error}"))
        })?;

    // AppState owns the shared dependencies used by handlers. Keep this
    // construction in one place so future process-level validation has a single
    // path before the socket is opened.
    let state = server::AppState::from_env().await?;

    server::bind_and_serve(addr, state, shutdown_signal()).await
}

async fn shutdown_signal() {
    // Ctrl-C is available on all supported platforms and is enough for local
    // development and foreground process execution.
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl-C shutdown handler");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        // Container orchestrators usually request shutdown with SIGTERM. When
        // installing the handler fails, keep the future pending so Ctrl-C still
        // remains the active graceful-shutdown trigger.
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

        // Either user interruption or orchestrator termination should begin
        // graceful shutdown. The server layer is responsible for draining
        // existing connections after this future resolves.
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
