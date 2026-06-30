// binary/src/main.rs
mod nodes;
mod state;

use anyhow::Result;
use config::WorkspaceProvider;
use pocketflow_core::{Action, Flow, SharedStore};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use crate::nodes::{ForgePairNode, LoreNode, NexusNode, VesselConfig, VesselNode};
use openflows::orchestration::OrchestrationResolver;
use crate::state::{
    Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_CI_FIX_NEEDED,
    ACTION_CONFLICTS_DETECTED, ACTION_DEPLOYED, ACTION_DEPLOY_FAILED, ACTION_DOCS_COMPLETE,
    ACTION_EMPTY, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK, ACTION_PR_OPENED,
    ACTION_WORK_ASSIGNED, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS,
};

fn print_usage_and_exit() -> ! {
    eprintln!("openflows — Autonomous AI Development Team");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  openflows                  Start the orchestration loop");
    eprintln!("  openflows --reset-orchestration  Reset all orchestration files to bundled defaults");
    eprintln!("  openflows --help            Show this help message");
    eprintln!();
    eprintln!("The orchestration directory is resolved by searching:");
    eprintln!("  1. Next to the binary");
    eprintln!("  2. Binary's parent directory (npm layout)");
    eprintln!("  3. OPENFLOWS_HOME (~/.openflows)");
    eprintln!("  4. Current working directory");
    eprintln!();
    eprintln!("On first run, all missing orchestration files are written from");
    eprintln!("built-in defaults. Use --reset-orchestration to overwrite all files.");
    std::process::exit(0);
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--reset-orchestration" => {
                let resolver = OrchestrationResolver::new()?;
                let orch_dir = resolver.reset_orchestration_dir()?;
                println!("Orchestration files reset to bundled defaults at: {}", orch_dir.display());
                println!("Version: {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print_usage_and_exit();
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                print_usage_and_exit();
            }
        }
    }

    let openflows_home = std::env::var("OPENFLOWS_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.openflows", h)))
        .or_else(|_| std::env::var("USERPROFILE").map(|h| format!("{}/.openflows", h)))
        .unwrap_or_else(|_| ".openflows".to_string());
    let env_paths = vec![
        std::path::PathBuf::from(format!("{}/.env", openflows_home)),
        std::env::current_dir().unwrap_or_default().join(".env"),
    ];
    for path in &env_paths {
        if path.exists() {
            match dotenvy::from_path(path) {
                Ok(_) => {
                    eprintln!("Loaded environment from {}", path.display());
                    break;
                }
                Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
    }

    // Load persisted Coder session token (for coder ssh authentication)
    if let Ok(home) = std::env::var("HOME") {
        let session_file = format!("{}/.openflows/coder-session-token", home);
        if std::env::var("CODER_SESSION_TOKEN").is_err() {
            if let Ok(token) = std::fs::read_to_string(&session_file) {
                let token = token.trim().to_string();
                if !token.is_empty() {
                    std::env::set_var("CODER_SESSION_TOKEN", &token);
                    info!("Loaded Coder session token from file");
                }
            }
        }
    }

    // Initialize tracing: default to INFO level, allow RUST_LOG to override
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Autonomous AI Dev Team starting (Phase 3 Integration with VESSEL)...");

    // 1. Check for target repository configuration
    let github_token = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN");
    let github_repo = std::env::var("GITHUB_REPOSITORY");

    // Determine workspace directory
    let workspace_dir = if let (Ok(token), Ok(repo)) = (&github_token, &github_repo) {
        // Production mode: clone/update target repository
        info!(repo = %repo, "Target repository configured, setting up workspace...");

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect("Could not determine home directory");
        let workspaces_base = std::path::PathBuf::from(home)
            .join(".agentflow")
            .join("workspaces");

        let workspace_manager = pair_harness::WorkspaceManager::new(&workspaces_base, repo);
        workspace_manager.ensure_workspace(token).await?
    } else {
        // Dev mode: use current directory for testing
        info!("No GITHUB_REPOSITORY configured - using current directory (dev mode)");
        std::env::current_dir()?
    };

    // 2. Initialise SharedStore (Redis or In-Memory)
    let store = if let Ok(url) = std::env::var("REDIS_URL") {
        info!("Using Redis backend: {}", url);
        SharedStore::new_redis(&url).await?
    } else {
        info!("REDIS_URL not set - using in-memory store (dev mode)");
        SharedStore::new_in_memory()
    };

// 2b. Determine workspace provider and bootstrap Coder if configured
    let workspace_provider = if std::env::var("CODER_URL").is_ok() || std::env::var("WORKSPACE_PROVIDER").as_deref() == Ok("coder") {
        let coder_url = std::env::var("CODER_URL").unwrap_or_else(|_| "http://localhost:7080".to_string());

        // Ensure CODER_URL is set (WORKSPACE_PROVIDER=coder without CODER_URL)
        if std::env::var("CODER_URL").is_err() {
            std::env::set_var("CODER_URL", &coder_url);
        }

        // Parse host and port from CODER_URL for consistent use
        let coder_port = reqwest::Url::parse(&coder_url)
            .ok()
            .and_then(|u| u.port())
            .unwrap_or(7080);
        let coder_host = reqwest::Url::parse(&coder_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| "localhost".to_string());

        eprintln!();
        eprintln!("═══ Coder Workspace Setup ═══");

        // Save compose_path for use in diagnostics later
        let compose_paths = vec![
            std::path::PathBuf::from("docker-compose.yml"),
            std::path::PathBuf::from(format!("{}/.openflows/docker-compose.yml",
                std::env::var("HOME").unwrap_or_default())),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("docker-compose.yml"))
                .unwrap_or_else(|| std::path::PathBuf::from("docker-compose.yml")),
        ];
        let compose_path = compose_paths.iter().find(|p| p.exists()).cloned();

        // Step 1: Check if Coder is already reachable
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        let mut coder_available = false;
        match http_client.get(format!("{}/api/v2/buildinfo", coder_url.trim_end_matches('/'))).send().await {
            Ok(resp) if resp.status().is_success() => {
                eprintln!("  ✓ Coder server already running at {}", coder_url);
                info!("Coder server already running at {}", coder_url);
                coder_available = true;
            }
            Ok(resp) => {
                eprintln!("  ⚠ Coder server at {} returned status {} — may still be starting", coder_url, resp.status());
            }
            Err(e) => {
                eprintln!("  • Coder server not reachable at {}", coder_url);
                if e.is_connect() {
                    eprintln!("    Reason: Connection refused — no service listening on that port.");
                } else if e.is_timeout() {
                    eprintln!("    Reason: Connection timed out — service may be starting or a firewall is blocking.");
                } else {
                    eprintln!("    Reason: {}", e);
                }
            }
        }

        // Step 2: If Coder is not available, try to start it
        let mut skip_coder = false;
        if !coder_available {
            eprintln!();
            eprintln!("  Attempting to start Coder services...");

            // Check Docker availability first
            let docker_check = tokio::process::Command::new("docker")
                .args(["info"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await;

            match docker_check {
                Ok(out) if out.status.success() => {
                    eprintln!("  ✓ Docker daemon is running");
                }
                Ok(_) => {
                    eprintln!("  ✗ Docker daemon is not running or not accessible");
                    eprintln!();
                    eprintln!("  Please start Docker and try again:");
                    eprintln!("    • Linux: sudo systemctl start docker");
                    eprintln!("    • macOS: Open Docker Desktop");
                    eprintln!();
                    eprintln!("  Falling back to local mode (git worktrees).");
                    eprintln!();
                    warn!("Docker not running, falling back to local mode");
                    std::env::set_var("WORKSPACE_PROVIDER", "local");
                    std::env::remove_var("CODER_URL");
                    skip_coder = true;
                }
                Err(e) => {
                    eprintln!("  ✗ Docker command not found: {}", e);
                    eprintln!("    Docker is required for Coder workspaces.");
                    eprintln!();
                    eprintln!("  Install Docker: https://docs.docker.com/get-docker/");
                    eprintln!();
                    eprintln!("  Falling back to local mode (git worktrees).");
                    eprintln!();
                    warn!("Docker not installed, falling back to local mode");
                    std::env::set_var("WORKSPACE_PROVIDER", "local");
                    std::env::remove_var("CODER_URL");
                    skip_coder = true;
                }
            }

            if !skip_coder {
                // Use the compose_path found earlier
                if let Some(ref compose_path) = compose_path {
                    // Check if the port is already in use before starting containers
                    if tokio::net::TcpStream::connect(format!("{}:{}", coder_host, coder_port)).await.is_ok()
                        && !coder_available
                    {
                        eprintln!("  ⚠ Port {} is already in use on {}", coder_port, coder_host);
                        eprintln!("    Another service is listening on that port.");
                        eprintln!("    To use a different port, set CODER_URL in your .env file, e.g.:");
                        eprintln!("      CODER_URL=http://localhost:7081");
                        eprintln!();
                        eprintln!("  Falling back to local mode (git worktrees).");
                        eprintln!();
                        warn!("Port {} already in use, falling back to local mode", coder_port);
                        std::env::set_var("WORKSPACE_PROVIDER", "local");
                        std::env::remove_var("CODER_URL");
                        skip_coder = true;
                    } else {
                    eprintln!("  Using {}", compose_path.display());

                    let raw_coder_password = std::env::var("CODER_ADMIN_PASSWORD").unwrap_or_else(|_| "Op3nFl0ws!".to_string());
                    // Validate password meets Coder's security requirements.
                    let coder_password = if raw_coder_password.len() >= 8
                        && raw_coder_password.chars().any(|c| c.is_uppercase())
                        && raw_coder_password.chars().any(|c| c.is_lowercase())
                        && raw_coder_password.chars().any(|c| c.is_ascii_digit())
                        && raw_coder_password.chars().any(|c| !c.is_alphanumeric())
                    {
                        raw_coder_password
                    } else {
                        eprintln!("  ⚠ CODER_ADMIN_PASSWORD does not meet Coder security requirements");
                        eprintln!("    (needs uppercase, lowercase, digit, special char, min 8 chars).");
                        eprintln!("    Using default secure password instead.");
                        "Op3nFl0ws!".to_string()
                    };
                    let pg_password = std::env::var("CODER_PG_PASSWORD").unwrap_or_else(|_| "coder".to_string());

                    let output = tokio::process::Command::new("docker")
                        .args([
                            "compose",
                            "--profile", "coder",
                            "-f", compose_path.to_str().unwrap_or("docker-compose.yml"),
                            "--env-file", "/dev/null",
                            "up", "-d", "coder-db", "coder",
                        ])
                        .env("CODER_URL", &coder_url)
                        .env("CODER_PORT", format!("{}", coder_port))
                        .env("CODER_ADMIN_PASSWORD", &coder_password)
                        .env("CODER_PG_PASSWORD", &pg_password)
                        .env("CODER_HTTP_ADDRESS", format!("0.0.0.0:{}", coder_port))
                        .env("CODER_ACCESS_URL", format!("http://localhost:{}", coder_port))
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .await;

                match output {
                    Ok(out) if out.status.success() => {
                        eprintln!("  ✓ Coder services starting");
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        eprintln!("  ✗ docker compose failed:");
                        for line in stderr.lines().take(5) {
                            eprintln!("    {}", line);
                        }
                        eprintln!();
                        eprintln!("  Falling back to local mode (git worktrees).");
                        eprintln!();
                        warn!("docker compose failed, falling back to local mode");
                        std::env::set_var("WORKSPACE_PROVIDER", "local");
                        std::env::remove_var("CODER_URL");
                        skip_coder = true;
                    }
                    Err(e) => {
                        eprintln!("  ✗ Could not run docker compose: {}", e);
                        eprintln!();
                        eprintln!("  Falling back to local mode (git worktrees).");
                        eprintln!();
                        warn!("docker compose command failed: {}, falling back to local mode", e);
                        std::env::set_var("WORKSPACE_PROVIDER", "local");
                        std::env::remove_var("CODER_URL");
                        skip_coder = true;
                    }
                }

                if !skip_coder {
                    // Give containers time to start, then verify
                    eprintln!("  Waiting for Coder containers to start...");
                for i in 1..=6 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let ps_output = tokio::process::Command::new("docker")
                        .args(["compose", "--profile", "coder", "-f", compose_path.to_str().unwrap_or("docker-compose.yml"), "ps"])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .output()
                        .await;

                    if let Ok(ps_out) = ps_output {
                        let ps_text = String::from_utf8_lossy(&ps_out.stdout);
                        let running = ps_text.lines().skip(1)
                            .filter(|l| l.to_lowercase().contains("running") || l.to_lowercase().contains("up"))
                            .count();
                        if running >= 2 {
                            eprintln!("  ✓ Coder containers are up (2/2 running)");
                            break;
                        }
                        if i < 6 {
                            eprintln!("  ⚳ Containers starting ({}/2)... attempt {}/6", running.min(2), i);
                        }
                    }
                }
                } // end if !skip_coder (container check)
                }
            } else {
                eprintln!("  ✗ docker-compose.yml not found");
                eprintln!();
                eprintln!("  Falling back to local mode (git worktrees).");
                eprintln!();
                warn!("docker-compose.yml not found, falling back to local mode");
                std::env::set_var("WORKSPACE_PROVIDER", "local");
                std::env::remove_var("CODER_URL");
                skip_coder = true;
            }
            }
        }

        if skip_coder {
            WorkspaceProvider::Local
        } else {
        // Step 3: Wait for health with progress, then bootstrap
        eprintln!("  Bootstrapping Coder (creating admin user, pushing workspace templates)...");
        info!("Coder: bootstrapping...");

        // Wait for health with progress output instead of silent 120s timeout
        let healthy_client = {
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(120);
            let mut attempts = 0u32;
            loop {
                if start.elapsed() >= timeout {
                    break None;
                }
                attempts += 1;
                match http_client.get(format!("{}/api/v2/buildinfo", coder_url.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(5)).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        eprintln!("  ✓ Coder server is healthy (after {}s)", start.elapsed().as_secs());
                        break Some(http_client.clone());
                    }
                    Ok(resp) => {
                        if attempts % 5 == 1 {
                            eprintln!("  ⏳ Coder not healthy yet (HTTP {}), retrying... [{}s elapsed]", resp.status(), start.elapsed().as_secs());
                        }
                    }
                    Err(e) => {
                        if attempts % 5 == 1 {
                            eprintln!("  ⏳ Coder not reachable yet ({}), retrying... [{}s elapsed]",
                                if e.is_connect() { "connection refused" } else { "timeout" },
                                start.elapsed().as_secs());
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        };

        match healthy_client {
            Some(_) => {
                // Server is healthy — proceed with bootstrap (create admin, get token, push templates)
                match coder_client::CoderBootstrapper::from_env() {
                    Ok(bootstrapper) => match bootstrapper.bootstrap().await {
                        Ok(client) => {
                            let coder_token = client.token().to_string();
                            let coder_url_str = client.base_url().to_string();
                            info!("Coder: bootstrapped — using Coder workspaces");
                            eprintln!("  ✓ Coder bootstrapped successfully");
                            eprintln!("    Admin user created, API token obtained, workspace templates pushed");
                            store.set("coder_api_token", serde_json::json!(coder_token)).await;
                            store.set("coder_url", serde_json::json!(coder_url_str)).await;
                            std::env::set_var("CODER_API_TOKEN", client.token());
                            std::env::set_var("CODER_URL", client.base_url());
                            WorkspaceProvider::Coder
                        }
                        Err(e) => {
                            eprintln!();
                            eprintln!("  ✗ Coder user/token setup failed:");
                            eprintln!("    {}", e);
                            eprintln!();
                            eprintln!("  Falling back to local mode (git worktrees).");
                            eprintln!();
                            warn!("Coder: bootstrap failed ({}), falling back to local mode", e);
                            std::env::set_var("WORKSPACE_PROVIDER", "local");
                            std::env::remove_var("CODER_URL");
                            WorkspaceProvider::Local
                        }
                    },
                    Err(e) => {
                        eprintln!();
                        eprintln!("  ✗ Coder configuration error: {}", e);
                        eprintln!();
                        eprintln!("  Falling back to local mode (git worktrees).");
                        eprintln!();
                        warn!("Coder: configuration error ({}), falling back to local mode", e);
                        std::env::set_var("WORKSPACE_PROVIDER", "local");
                        std::env::remove_var("CODER_URL");
                        WorkspaceProvider::Local
                    }
                }
            }
            None => {
                // Health check timed out — provide diagnostics
                eprintln!();
                eprintln!("  ✗ Coder server did not become healthy within 120s");
                eprintln!();
                eprintln!("  Diagnostics:");

                // Port check — use the same host/port as CODER_URL
                let check_addr = {
                    let url = reqwest::Url::parse(&coder_url).unwrap_or_else(|_| reqwest::Url::parse("http://localhost:7080").unwrap());
                    let host = url.host_str().unwrap_or("localhost");
                    let port = url.port().unwrap_or(7080);
                    format!("{}:{}", host, port)
                };
                let port_check = tokio::net::TcpStream::connect(&check_addr).await;
                match port_check {
                    Ok(_) => eprintln!("    • Port 7080 is open — Coder process is listening but not healthy"),
                    Err(_) => eprintln!("    • Port 7080 is not open — Coder is not listening"),
                }

                // Container status and logs
                if let Some(ref cp) = compose_path {
                    let ps_output = tokio::process::Command::new("docker")
                        .args(["compose", "--profile", "coder", "-f", cp.to_str().unwrap_or("docker-compose.yml"), "ps"])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .output()
                        .await;

                    if let Ok(out) = ps_output {
                        let table = String::from_utf8_lossy(&out.stdout);
                        let running = table.lines().skip(1)
                            .filter(|l| l.to_lowercase().contains("running") || l.to_lowercase().contains("up"))
                            .count();
                        if running >= 2 {
                            eprintln!("    • Containers are running ({}/2 up)", running);
                        } else {
                            eprintln!("    • Not all containers running ({}/2 up)", running);
                            eprintln!("      Check: docker compose --profile coder ps");
                        }
                    } else {
                        eprintln!("    • Could not check container status");
                    }

                    let logs_output = tokio::process::Command::new("docker")
                        .args(["compose", "--profile", "coder", "-f", cp.to_str().unwrap_or("docker-compose.yml"), "logs", "coder", "--tail", "15"])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .output()
                        .await;

                    if let Ok(log_out) = logs_output {
                        let logs = String::from_utf8_lossy(&log_out.stdout);
                        let lines: Vec<&str> = logs.lines().rev().take(8).collect();
                        if !lines.is_empty() {
                            eprintln!("    Recent Coder logs:");
                            for line in lines.into_iter().rev() {
                                eprintln!("      {}", line);
                            }
                        }
                    }
                } else {
                    eprintln!("    • No docker-compose.yml found for further diagnostics");
                }

                // Direct health check
                match http_client.get(format!("{}/api/v2/buildinfo", coder_url.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(5)).send().await {
                    Ok(resp) => {
                        eprintln!("    • Coder health endpoint returned HTTP {} (expected 200)", resp.status());
                    }
                    Err(e) => {
                        eprintln!("    • Coder health endpoint unreachable: {}", e);
                    }
                }

                eprintln!();
                eprintln!("  Falling back to local mode (git worktrees).");
                eprintln!();
                warn!("Coder: health check timed out, falling back to local mode");

                std::env::set_var("WORKSPACE_PROVIDER", "local");
                std::env::remove_var("CODER_URL");
                WorkspaceProvider::Local
            }
        }
        }
    } else {
        info!("Coder: disabled (local mode)");
        WorkspaceProvider::Local
    };

    eprintln!();
    eprintln!("═══ OpenFlows Configuration ═══");
    eprintln!("  Workspace Provider: {:?}", workspace_provider);
    match workspace_provider {
        WorkspaceProvider::Coder => {
            if let Ok(url) = std::env::var("CODER_URL") {
                eprintln!("  Coder URL: {}", url);
            }
            eprintln!("  Mode: Coder workspaces (isolated per pair)");
        }
        WorkspaceProvider::Local => {
            eprintln!("  Mode: Local (git worktrees)");
        }
    }
    eprintln!();

    // 3. Dry Run Setup: Inject a test ticket and 2 worker slots
    info!("Injecting dry-run data...");
    let test_ticket = Ticket {
        id: "T-001".to_string(),
        title: "Implement landing page glassmorphism".to_string(),
        body: "Add a new CSS class for glassmorphism and apply to the hero section.".to_string(),
        priority: 1,
        branch: None,
        status: TicketStatus::Open,
        issue_url: None,
        attempts: 0,
    };

    let worker_slots = HashMap::from([
        (
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Idle,
                workspace_id: None,
                workspace_provider: workspace_provider.clone(),
            },
        ),
        (
            "forge-2".to_string(),
            WorkerSlot {
                id: "forge-2".to_string(),
                status: WorkerStatus::Idle,
                workspace_id: None,
                workspace_provider: workspace_provider.clone(),
            },
        ),
    ]);

    store
        .set(KEY_TICKETS, serde_json::to_value(vec![test_ticket])?)
        .await;
    store
        .set(KEY_WORKER_SLOTS, serde_json::to_value(worker_slots)?)
        .await;
    store.set(KEY_PENDING_PRS, serde_json::json!([])).await;

    // 4. Resolve and ensure orchestration directory is complete
    //    Bundled files are embedded at compile time and materialized on disk if missing.
    let resolver = OrchestrationResolver::new()?;
    let orch_dir = resolver.ensure_orchestration_dir()?;
    resolver.validate()?;

    info!(dir = %orch_dir.display(), "Orchestration directory resolved");

    let registry_path = resolver.registry_path();
    let registry = config::Registry::load(&registry_path)?;
    let registry_json = serde_json::to_string_pretty(&registry)?;
    std::env::set_var("OPENFLOWS_REGISTRY_PATH", &registry_path);
    std::env::set_var("OPENFLOWS_REGISTRY_JSON", &registry_json);
    store
        .set("registry_json", serde_json::json!(registry_json))
        .await;

    std::env::set_var("ORCHESTRATOR_DIR", resolver.orchestrator_dir());

    let nexus_persona = resolver.persona_path("nexus.agent.md");
    let nexus = Arc::new(NexusNode::new(nexus_persona, registry_path.clone()));
    let forge_pair = Arc::new(ForgePairNode::new_with_registry(
        &workspace_dir,
        registry_path.clone(),
    ));
    let vessel = Arc::new(VesselNode::new(
        VesselConfig::from_registry(&registry_path).unwrap_or_else(|e| {
            warn!(error = %e, "Failed to load vessel config from registry, using fallback");
            VesselConfig::from_env()
        }),
    ));
    let lore = if registry.get("lore").is_some() {
        let lore_persona = resolver.persona_path("lore.agent.md");
        match LoreNode::new_with_registry(
            &workspace_dir,
            lore_persona,
            registry_path.clone(),
        ) {
            Ok(node) => Some(Arc::new(node)),
            Err(e) => {
                warn!("lore agent is active but could not initialize — skipping: {}", e);
                None
            }
        }
    } else {
        info!("lore agent is inactive — skipping lore node initialization");
        None
    };

    let mut flow = Flow::new("nexus")
        .add_node(
            "nexus",
            nexus,
            vec![
                (ACTION_WORK_ASSIGNED, "forge_pair"),
                (ACTION_MERGE_PRS, "vessel"),
                (ACTION_NO_WORK, "nexus"),
                ("approve_command", "forge_pair"),
                ("reject_command", "nexus"),
            ],
        )
        .add_node(
            "forge_pair",
            forge_pair,
            vec![
                (ACTION_PR_OPENED, "vessel"),
                (ACTION_FAILED, "nexus"),
                (ACTION_EMPTY, "nexus"),
                (Action::NO_TICKETS, "nexus"),
                ("suspended", "nexus"),
            ],
        )
        .add_node(
            "vessel",
            vessel,
            {
                let mut routes = vec![
                    (ACTION_DEPLOY_FAILED, "nexus"),
                    (ACTION_CI_FIX_NEEDED, "forge_pair"),
                    ("merge_blocked", "nexus"),
                    (ACTION_CONFLICTS_DETECTED, "forge_pair"),
                    (Action::AWAITING_HUMAN, "nexus"),
                    ("no_work", "nexus"),
                ];
                if lore.is_some() {
                    routes.insert(0, (ACTION_DEPLOYED, "lore"));
                } else {
                    routes.insert(0, (ACTION_DEPLOYED, "nexus"));
                }
                routes
            },
        );

    if let Some(ref lore_node) = lore {
        flow = flow.add_node(
            "lore",
            lore_node.clone(),
            vec![(ACTION_DOCS_COMPLETE, "nexus"), (ACTION_NO_WORK, "nexus")],
        );
    }
    
    let flow = flow.max_steps(20);

    // 5. Run Flow
    info!("Starting Flow execution loop...");
    let _final_action = flow.run(&store).await?;

    // 6. Results
    let final_slots: HashMap<String, WorkerSlot> =
        store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

    for slot in final_slots.values() {
        info!(worker = slot.id, status = ?slot.status, "Final worker status");
    }

    info!("Phase 3 Dry Run complete.");
    Ok(())
}
