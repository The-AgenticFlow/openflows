//! Binary entry point for running the OpenFlows Manager service.

use clap::{Parser, Subcommand};
use openflows_manager::{config::ManagerConfig, error::ManagerError, server};
use std::net::SocketAddr;

#[derive(Parser)]
#[command(about = "OpenFlows control-plane service and deployment tools")]
struct Cli {
    /// Shared deployment file; defaults to .env.openflows, then legacy .env.prod.
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the Manager (also the default without a subcommand).
    Serve,
    /// Generate internal keys and register the OpenFlows OAuth application in Coder.
    Setup {
        /// Initialize an empty Coder deployment with a recoverable administrator account.
        #[arg(long, requires = "admin_email")]
        initialize_coder: bool,
        #[arg(long, requires = "initialize_coder")]
        admin_email: Option<String>,
    },
    /// Read-only deployment checks; exits nonzero if any check fails.
    Doctor,
    /// Reissue a single-use bootstrap after a hosted workspace lost its runtime session.
    RepairRuntime {
        /// OpenFlows workspace UUID (not the Coder workspace UUID).
        workspace_id: uuid::Uuid,
    },
    /// Replace the Coder lifecycle-hook secret from standard input.
    SetCoderHookSecret,
}

#[tokio::main]
async fn main() -> Result<(), ManagerError> {
    let cli = Cli::parse();
    openflows_manager::operator::load_configuration(cli.config.as_deref())
        .map_err(ManagerError::Service)?;
    // Initialize the default tracing subscriber before any fallible setup so
    // configuration and bind failures are visible to operators.
    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "openflows_manager=info,tower_http=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();

    let command = cli.command.unwrap_or(Command::Serve);
    if !matches!(&command, Command::Doctor) {
        openflows_manager::operator::load_hosted_configuration()
            .await
            .map_err(ManagerError::Service)?;
    }
    match command {
        Command::Setup {
            initialize_coder,
            admin_email,
        } => {
            return openflows_manager::operator::setup(initialize_coder, admin_email.as_deref())
                .await
                .map_err(ManagerError::Service);
        }
        Command::Doctor => {
            return openflows_manager::operator::doctor()
                .await
                .map_err(ManagerError::Service);
        }
        Command::RepairRuntime { workspace_id } => {
            return openflows_manager::operator::repair_runtime(workspace_id)
                .await
                .map_err(ManagerError::Service);
        }
        Command::SetCoderHookSecret => {
            return openflows_manager::operator::set_coder_hook_secret()
                .await
                .map_err(ManagerError::Service);
        }
        Command::Serve => {}
    }

    // Managed installations can repair or renew the Coder authority before
    // workers begin. The saved bootstrap password is never sent to runtimes.
    if let Some(email) = openflows_manager::operator::managed_bootstrap_email()
        .await
        .map_err(ManagerError::Service)?
    {
        openflows_manager::operator::setup(true, Some(&email))
            .await
            .map_err(ManagerError::Service)?;
    }

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
