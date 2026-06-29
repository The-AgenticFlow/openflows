use agent_forge::ForgePairNode;
use agent_lore::LoreNode;
use agent_nexus::NexusNode;
use agent_vessel::{VesselConfig, VesselNode};
use anyhow::Result;
use config::{
    ACTION_CI_FIX_NEEDED, ACTION_CONFLICTS_DETECTED, ACTION_DEPLOYED, ACTION_DEPLOY_FAILED,
    ACTION_DOCS_COMPLETE, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK, ACTION_PR_OPENED,
    ACTION_WORK_ASSIGNED, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS,
    WorkspaceProvider,
};
use pair_harness::WorkspaceManager;
use pocketflow_core::{Action, Flow, SharedStore};
use std::sync::Arc;
use tracing::{info, warn};

fn load_env() -> anyhow::Result<std::path::PathBuf> {
    let openflows_home = std::env::var("OPENFLOWS_HOME")
        .or_else(|_| {
            std::env::var("HOME").map(|h| format!("{}/.openflows", h.trim_end_matches('/')))
        })
        .or_else(|_| {
            std::env::var("USERPROFILE").map(|h| format!("{}/.openflows", h.trim_end_matches('/')))
        })
        .unwrap_or_else(|_| ".openflows".to_string());
    let env_paths = vec![
        std::path::PathBuf::from(format!("{}/.env", openflows_home)),
        std::env::current_dir().unwrap_or_default().join(".env"),
    ];
    for path in &env_paths {
        if path.exists() {
            match dotenvy::from_path(path) {
                Ok(_) => return Ok(path.clone()),
                Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
    }
    Ok(std::path::PathBuf::new())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--reset-orchestration" => {
                eprintln!("Loading environment...");
                let _ = load_env()?;
                let resolver = openflows::orchestration::OrchestrationResolver::new()?;
                let orch_dir = resolver.reset_orchestration_dir()?;
                println!(
                    "Orchestration files reset to bundled defaults at: {}",
                    orch_dir.display()
                );
                println!("Version: {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                eprintln!("openflows (agentflow) — Autonomous AI Development Team");
                eprintln!();
                eprintln!("USAGE:");
                eprintln!("  openflows                          Start the orchestration loop");
                eprintln!(
                    "  openflows --reset-orchestration    Reset orchestration files to defaults"
                );
                eprintln!("  openflows --help                   Show this help message");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                eprintln!("openflows (agentflow) — Autonomous AI Development Team");
                eprintln!();
                eprintln!("USAGE:");
                eprintln!("  openflows                          Start the orchestration loop");
                eprintln!(
                    "  openflows --reset-orchestration    Reset orchestration files to defaults"
                );
                eprintln!("  openflows --help                   Show this help message");
                std::process::exit(0);
            }
        }
    }

    let env_path = load_env()?;
    if !env_path.as_os_str().is_empty() {
        eprintln!("Loaded environment from {}", env_path.display());
    }
    // Initialize tracing: default to INFO level, allow RUST_LOG to override
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 1b. Determine workspace provider and bootstrap Coder if configured
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
                    let docker_err = tokio::process::Command::new("docker")
                        .args(["info"])
                        .stderr(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::null())
                        .output()
                        .await
                        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
                        .unwrap_or_else(|_| "unknown error".to_string());
                    let first_line = docker_err.lines().next().unwrap_or("unknown error");
                    eprintln!("  ✗ Docker daemon is not running or not accessible:");
                    eprintln!("    {}", first_line);
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
                    eprintln!("  Or switch to local mode: set WORKSPACE_PROVIDER=local");
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

                    let coder_password = std::env::var("CODER_ADMIN_PASSWORD").unwrap_or_else(|_| "Op3nFl0ws!".to_string());
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
                        // Give containers a moment to start, then verify
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

                        // Check logs for common startup issues
                        let logs_output = tokio::process::Command::new("docker")
                            .args(["compose", "--profile", "coder", "-f", compose_path.to_str().unwrap_or("docker-compose.yml"), "logs", "coder", "--tail", "5"])
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .output()
                            .await;

                        if let Ok(log_out) = logs_output {
                            let logs = String::from_utf8_lossy(&log_out.stdout);
                            if logs.contains("permission denied") || logs.contains("Cannot connect to the Docker daemon") {
                                eprintln!("  ✗ Docker permission issue detected in container logs");
                                eprintln!("    Try: sudo usermod -aG docker $USER && newgrp docker");
                            } else if logs.contains("port") && logs.contains("already in use") {
                                eprintln!("  ✗ Port conflict detected — another service may be using port {}", coder_port);
                                eprintln!("    Check: lsof -i :{}", coder_port);
                            } else if logs.contains("database") && (logs.contains("connection refused") || logs.contains("connect: connection refused")) {
                                eprintln!("  ⚳ Coder is waiting for its database to become ready...");
                            }
                        }
                    }
                    } // end else (port available, compose ran)
                } else {
                    eprintln!("  ✗ docker-compose.yml not found in any of:");
                    for p in &compose_paths {
                        eprintln!("    • {}", p.display());
                    }
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

        // Step 3: Bootstrap Coder (create admin user, push templates)
        //         Instead of calling bootstrapper.bootstrap() which silently waits up to 120s,
        //         we do our own health-wait loop with progress output, then call bootstrap for
        //         the user/token/template setup.
        //         If skip_coder is true, we already decided to fall back to local mode.
        if skip_coder {
            WorkspaceProvider::Local
        } else {
        eprintln!("  Bootstrapping Coder (creating admin user, pushing workspace templates)...");
        info!("Coder: bootstrapping...");

        // Wait for health with progress output
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
                match coder_client::bootstrap::CoderBootstrapper::from_env() {
                    Ok(bootstrapper) => match bootstrapper.bootstrap().await {
                        Ok(client) => {
                            std::env::set_var("CODER_API_TOKEN", client.token());
                            eprintln!("  ✓ Coder bootstrapped successfully");
                            eprintln!("    Admin user created, API token obtained, workspace templates pushed");
                            info!("Coder: bootstrapped — using Coder workspaces");
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
                    }
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

                // Container status
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

                    // Container logs
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
                        if logs.contains("permission denied") {
                            eprintln!("    • Docker socket permission issue detected");
                            eprintln!("      Fix: sudo usermod -aG docker $USER && newgrp docker");
                        }
                        if logs.contains("database") && logs.contains("connection refused") {
                            eprintln!("    • Coder cannot reach its database (coder-db may still be starting)");
                            eprintln!("      Try: docker compose --profile coder restart coder");
                        }
                    }
                } else {
                    eprintln!("    • No docker-compose.yml found for further diagnostics");
                }

                // Try a direct health check one more time to show what's happening
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
                eprintln!("  To retry with Coder, fix the issue above and re-run.");
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

    info!("Starting REAL End-to-End Orchestration (Event-Driven FORGE-SENTINEL Pairs + VESSEL)");

    // 1. Resolve and ensure orchestration directory
    use openflows::orchestration::OrchestrationResolver;
    let resolver = OrchestrationResolver::new()?;
    let orch_dir = resolver.ensure_orchestration_dir()?;
    resolver.validate()?;

    info!(dir = %orch_dir.display(), "Orchestration directory resolved");

    let registry_path = resolver.registry_path();
    let registry = config::Registry::load(&registry_path)?;
    let github_token = registry
        .resolve_github_token("forge")
        .expect("AGENT_FORGE_GITHUB_TOKEN or GITHUB_PERSONAL_ACCESS_TOKEN must be set");
    let repo = std::env::var("GITHUB_REPOSITORY")
        .expect("GITHUB_REPOSITORY must be set (e.g. owner/repo)");

    if std::env::var("LLM_PROVIDER").is_err() {
        std::env::set_var("LLM_PROVIDER", "openai");
    }

    // 2. Clone/Update the target repository workspace
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("Could not determine home directory");
    let workspaces_base = std::path::PathBuf::from(home)
        .join(".agentflow")
        .join("workspaces");

    let workspace_manager = WorkspaceManager::new(&workspaces_base, &repo);
    let workspace_dir = workspace_manager.ensure_workspace(&github_token).await?;

    info!(workspace = %workspace_dir.display(), "Target repository workspace ready");

    std::env::set_var("AGENTFLOW_WORKSPACE_ROOT", &workspace_dir);
    std::env::set_var("ORCHESTRATOR_DIR", resolver.orchestrator_dir());

    // 3. Initialize Nodes
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
        match LoreNode::new_with_registry(&workspace_dir, lore_persona, registry_path.clone()) {
            Ok(node) => Some(Arc::new(node)),
            Err(e) => {
                warn!(
                    "lore agent is active but could not initialize — skipping: {}",
                    e
                );
                None
            }
        }
    } else {
        info!("lore agent is inactive — skipping lore node initialization");
        None
    };

    // 4. Setup Flow with Routing
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
                ("suspended", "nexus"),
                (Action::NO_TICKETS, "nexus"),
            ],
        )
        .add_node("vessel", vessel, {
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
        });

    if let Some(ref lore_node) = lore {
        flow = flow.add_node(
            "lore",
            lore_node.clone(),
            vec![(ACTION_DOCS_COMPLETE, "nexus"), (ACTION_NO_WORK, "nexus")],
        );
    }

    // 5. Initialize Shared Store
    let store = SharedStore::new_in_memory();
    store.set("repository", serde_json::json!(repo)).await;
    store.set(KEY_TICKETS, serde_json::json!([])).await;
    store.set(KEY_WORKER_SLOTS, serde_json::json!({})).await;
    store.set(KEY_PENDING_PRS, serde_json::json!([])).await;

    // Store Coder context so downstream nodes can reconstruct CoderClient
    if matches!(workspace_provider, WorkspaceProvider::Coder) {
        if let Ok(token) = std::env::var("CODER_API_TOKEN") {
            store.set("coder_api_token", serde_json::json!(token)).await;
        }
        if let Ok(url) = std::env::var("CODER_URL") {
            store.set("coder_url", serde_json::json!(url)).await;
        }
    }

    // 6. Run Flow
    info!("Running orchestration loop for repository: {}", repo);
    info!("Each worker will use event-driven FORGE-SENTINEL pair with:");
    info!("  - PLAN.md -> CONTRACT.md (plan review)");
    info!("  - WORKLOG.md -> segment-N-eval.md (segment evaluation)");
    info!("  - final-review.md (final approval)");
    info!("  - STATUS.json (completion status)");
    info!("VESSEL will handle merge gate:");
    info!("  - CI status polling (10s interval, 10min timeout)");
    info!("  - Squash merge with ticket reference");
    info!("  - ticket_merged event emission");

    let final_action = flow.run(&store).await?;

    info!("Orchestration flow halted with action: {}", final_action);

    Ok(())
}
