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
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(%error, "failed to install Ctrl-C shutdown handler");
    }
}
