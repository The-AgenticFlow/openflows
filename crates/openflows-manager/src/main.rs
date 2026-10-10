//! Binary entry point for running the OpenFlows Manager service.

use openflows_manager::{config::ManagerConfig, error::ManagerError, server};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), ManagerError> {
    load_env_file()?;
    // Initialize the default tracing subscriber before any fallible setup so
    // configuration and bind failures are visible to operators.
    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "openflows_manager=info,tower_http=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();

    // Centralized, typed configuration. Hosted-mode configuration failures are
    // surfaced here rather than silently falling back to local behavior.
    let config = ManagerConfig::from_env().map_err(|e| ManagerError::Config(e.to_string()))?;
    tracing::info!(mode = %config.mode, addr = %config.http_addr, "starting OpenFlows Manager");
    let addr = config.http_addr.parse::<SocketAddr>().map_err(|error| {
        ManagerError::Config(format!("invalid OPENFLOWS_MANAGER_ADDR: {error}"))
    })?;

    // AppState owns the shared dependencies used by handlers. In hosted mode it
    // connects the control-plane database; a failure leaves the process unable
    // to start rather than silently serving without durable state.
    let state = server::AppState::from_env().await?;

    server::bind_and_serve(addr, state, shutdown_signal()).await
}

/// Load repository-local deployment settings before typed configuration is
/// parsed. Existing process environment variables win, so CI and explicit
/// shell overrides remain authoritative.
fn load_env_file() -> Result<(), ManagerError> {
    let path = std::path::Path::new(".env.prod");
    if path.exists() {
        dotenvy::from_path(path)
            .map_err(|error| ManagerError::Config(format!("cannot load .env.prod: {error}")))?;
    }
    Ok(())
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
