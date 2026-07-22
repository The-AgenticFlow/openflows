//! openflows binary entry point (Coder-only redesign).
//!
//! The Controller runs inside the long-lived openflows-nexus Coder workspace.
//! It requires these env vars (all injected by the template — no fallback):
//!   CODER_URL              — Coder server URL
//!   CODER_SESSION_TOKEN    — Scoped tenant-owner token
//!   REDIS_URL              — Redis SharedStore URL
//!   OPENFLOWS_TENANT       — Tenant identifier
//!   GITHUB_REPOSITORY      — Target repo (owner/repo)

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

const CONTROLLER_POLL_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Parser)]
#[command(name = "openflows")]
#[command(about = "OpenFlows — Autonomous AI Dev Team orchestrator (Coder-only)")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the Controller orchestration loop (default inside nexus workspace)
    Run,
    /// Bootstrap Coder: admin, templates, LLM check, external auth check
    Bootstrap,
    /// Tenant management
    Tenant {
        #[command(subcommand)]
        action: TenantCommands,
    },
    /// Read-only status from Redis (tickets, workers, heartbeats, PRs)
    Status {
        /// Filter by tenant name
        #[arg(long)]
        tenant: Option<String>,
        /// Output JSON instead of table
        #[arg(long)]
        json: bool,
    },
    /// Diagnose Coder integration health
    Doctor,
    /// Reset orchestration files to bundled defaults
    ResetOrchestration,
}

#[derive(Subcommand)]
enum TenantCommands {
    /// Add a new tenant (owner/repo) — creates Coder user + nexus workspace
    Add {
        /// GitHub repository in owner/repo format
        repo: String,
        /// Tenant name (defaults to repo owner)
        #[arg(long)]
        name: Option<String>,
    },
    /// List all tenants (from Redis namespaces)
    List,
    /// Remove a tenant: archive chats, delete workspaces, optionally purge Redis
    Remove {
        /// Tenant name
        name: String,
        /// Also purge ns:{tenant}:* from Redis
        #[arg(long)]
        purge: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env if present (dev / host CLI use). In the nexus workspace, vars
    // are injected by the template and this silently does nothing.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run_controller().await,
        Commands::Bootstrap => run_bootstrap().await,
        Commands::Tenant { action } => run_tenant(action).await,
        Commands::Status { tenant, json } => run_status(tenant, json).await,
        Commands::Doctor => openflows::doctor::run_checks().await,
        Commands::ResetOrchestration => run_reset().await,
    }
}

async fn run_controller() -> Result<()> {
    // ── Fail-fast environment validation (no fallback) ──────────────────
    let coder_url = std::env::var("CODER_URL").context(
        "CODER_URL is not set. The Controller must run inside an openflows-nexus workspace.",
    )?;
    let _coder_token = std::env::var("CODER_SESSION_TOKEN")
        .context("CODER_SESSION_TOKEN is not set. The Controller must run inside an openflows-nexus workspace.")?;
    let redis_url = std::env::var("REDIS_URL").context(
        "REDIS_URL is not set. The Controller must run inside an openflows-nexus workspace.",
    )?;
    let tenant = std::env::var("OPENFLOWS_TENANT").context(
        "OPENFLOWS_TENANT is not set. The Controller must run inside an openflows-nexus workspace.",
    )?;
    let github_repo = std::env::var("GITHUB_REPOSITORY")
        .context("GITHUB_REPOSITORY is not set. The Controller must run inside an openflows-nexus workspace.")?;

    tracing::info!(
        coder_url,
        redis_url,
        tenant,
        github_repo,
        "OpenFlows Controller starting (Coder-only mode)"
    );

    // ── Initialize SharedStore (Redis required — no in-memory fallback) ─
    // Tenant-aware: all keys are prefixed with ns:{tenant}: for isolation
    let store =
        pocketflow_core::SharedStore::new_redis_with_tenant(&redis_url, Some(tenant.clone()))
            .await?;

    // ── Resolve orchestration directory ─────────────────────────────────
    let resolver = openflows::orchestration::OrchestrationResolver::new()?;
    let orch_dir = resolver.ensure_orchestration_dir()?;
    resolver.validate()?;

    let registry_path = resolver.registry_path();
    let registry = config::Registry::load(&registry_path)?;
    let registry_json = serde_json::to_string_pretty(&registry)?;
    std::env::set_var("OPENFLOWS_REGISTRY_PATH", &registry_path);
    std::env::set_var("OPENFLOWS_REGISTRY_JSON", &registry_json);
    std::env::set_var("ORCHESTRATOR_DIR", resolver.orchestrator_dir());

    store
        .set("registry_json", serde_json::json!(registry_json))
        .await;

    // ── Build flow nodes ────────────────────────────────────────────────
    let nexus_persona = resolver.persona_path("nexus.agent.md");
    let nexus = std::sync::Arc::new(openflows::nodes::NexusNode::new(
        nexus_persona,
        registry_path.clone(),
    ));
    let forge_pair = std::sync::Arc::new(openflows::nodes::ForgePairNode::new_with_registry(
        &orch_dir,
        registry_path.clone(),
    ));
    let sentinel = std::sync::Arc::new(openflows::nodes::SentinelNode::new(registry_path.clone()));
    let vessel = std::sync::Arc::new(openflows::nodes::VesselNode::new(
        openflows::nodes::VesselConfig::from_registry(&registry_path).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to load vessel config from registry, using fallback");
            openflows::nodes::VesselConfig::from_env()
        }),
    ));
    let lore = if registry.get("lore").map(|e| e.enabled).unwrap_or(false) {
        let lore_persona = resolver.persona_path("lore.agent.md");
        match openflows::nodes::LoreNode::new_with_registry(
            &orch_dir,
            lore_persona,
            registry_path.clone(),
        ) {
            Ok(node) => Some(std::sync::Arc::new(node)),
            Err(e) => {
                tracing::warn!(
                    "lore agent is active but could not initialize — skipping: {}",
                    e
                );
                None
            }
        }
    } else {
        tracing::info!("lore agent is inactive — skipping lore node initialization");
        None
    };

    // ── Build flow graph ────────────────────────────────────────────────
    use openflows::state::{
        ACTION_CI_FIX_NEEDED, ACTION_CONFLICTS_DETECTED, ACTION_DEPLOYED, ACTION_DEPLOY_FAILED,
        ACTION_DOCS_COMPLETE, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK, ACTION_PR_OPENED,
        ACTION_WORK_ASSIGNED,
    };

    let review_approve = "review_approve";
    let review_reject = "review_reject";

    let mut flow = pocketflow_core::Flow::new("nexus")
        .add_node(
            "nexus",
            nexus,
            vec![
                (ACTION_WORK_ASSIGNED, "forge_pair"),
                (ACTION_MERGE_PRS, "vessel"),
                ("approve_command", "forge_pair"),
                ("reject_command", "nexus"),
            ],
        )
        .add_node(
            "forge_pair",
            forge_pair,
            vec![
                (ACTION_PR_OPENED, "sentinel"),
                (ACTION_FAILED, "nexus"),
                (pocketflow_core::Action::NO_TICKETS, "nexus"),
                ("suspended", "nexus"),
            ],
        )
        .add_node(
            "sentinel",
            sentinel,
            vec![
                (review_approve, "vessel"),
                (review_reject, "forge_pair"),
                ("no_work", "nexus"),
            ],
        )
        .add_node("vessel", vessel, {
            let mut routes = vec![
                (ACTION_DEPLOY_FAILED, "nexus"),
                (ACTION_CI_FIX_NEEDED, "forge_pair"),
                ("merge_blocked", "nexus"),
                (ACTION_CONFLICTS_DETECTED, "forge_pair"),
                (pocketflow_core::Action::AWAITING_HUMAN, "nexus"),
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

    // Safety cap against genuine routing cycles. Idle and in-progress states pause
    // the flow pass and are handled by the paced controller loop below.
    // The per-node cycle detector catches tight ping-pong (nexus→forge_pair→nexus→…)
    // within ~40 steps instead of burning all 1000.
    let flow = flow.max_steps(1000).max_visits_per_node(20);

    // ── Run controller poll loop ────────────────────────────────────────
    tracing::info!(
        poll_interval_secs = CONTROLLER_POLL_INTERVAL.as_secs(),
        "Starting Controller poll loop"
    );
    loop {
        match flow.run(&store).await {
            Ok(final_action) => {
                tracing::info!(
                    action = final_action.as_str(),
                    poll_interval_secs = CONTROLLER_POLL_INTERVAL.as_secs(),
                    "Controller flow pass completed; waiting for next poll"
                );
            }
            Err(e) => {
                // Self-healing: never let a flow error kill the controller.
                // Log the error, back off, and retry on the next poll cycle.
                // Transient errors (Redis drops, Coder API timeouts, GitHub
                // rate limits) should not stop the orchestration loop.
                tracing::error!(
                    error = %e,
                    poll_interval_secs = CONTROLLER_POLL_INTERVAL.as_secs(),
                    "Controller flow pass failed — will retry on next poll (self-healing)"
                );
            }
        }
        tokio::time::sleep(CONTROLLER_POLL_INTERVAL).await;
    }
}

async fn run_bootstrap() -> Result<()> {
    let bootstrapper = coder_client::bootstrap::CoderBootstrapper::from_env()
        .context("Failed to create bootstrapper from environment")?;

    let client = bootstrapper.bootstrap().await.context("Bootstrap failed")?;

    // Verify LLM configuration
    if let Err(e) = coder_client::bootstrap::CoderBootstrapper::verify_llm_configured(&client).await
    {
        eprintln!("\n  ⚠ {}", e);
        eprintln!("    Configure at least one model in the Coder dashboard before adding tenants.");
    }

    // Verify GitHub external auth
    if let Err(e) = coder_client::bootstrap::CoderBootstrapper::verify_external_auth_configured() {
        eprintln!("\n  ⚠ {}", e);
    }

    println!("\nBootstrap complete. Run `openflows tenant add <owner/repo>` to add a tenant.");
    Ok(())
}

async fn run_tenant(action: TenantCommands) -> Result<()> {
    let bootstrapper = coder_client::bootstrap::CoderBootstrapper::from_env()
        .context("Failed to create bootstrapper from environment")?;

    let client = bootstrapper
        .bootstrap()
        .await
        .context("Bootstrap required before tenant operations")?;

    match action {
        TenantCommands::Add { repo, name } => {
            let tenant_name =
                name.unwrap_or_else(|| repo.split('/').next().unwrap_or(&repo).to_string());

            println!(
                "Adding tenant '{}' for repository '{}'...",
                tenant_name, repo
            );
            let workspace_id = bootstrapper
                .ensure_tenant(&client, &tenant_name, &repo)
                .await
                .context("Tenant setup failed")?;

            println!("\n  ✓ Tenant '{}' added", tenant_name);
            println!("  ✓ Nexus workspace: {}", workspace_id);
            println!("  → Complete the GitHub OAuth link in the Coder dashboard for this tenant");
        }
        TenantCommands::List => {
            println!("Tenants (from Redis namespaces):");
            // Read all ns:* keys from Redis and list unique tenants
            let redis_url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
            match pocketflow_core::SharedStore::new_redis(&redis_url).await {
                Ok(store) => {
                    let keys: Vec<String> = store.keys("ns:*").await;
                    let mut tenants = std::collections::HashSet::new();
                    for key in keys {
                        if let Some(ns) = key.strip_prefix("ns:") {
                            if let Some(tenant) = ns.split(':').next() {
                                tenants.insert(tenant.to_string());
                            }
                        }
                    }
                    if tenants.is_empty() {
                        println!("  (no tenants found)");
                    } else {
                        for t in tenants {
                            println!("  - {}", t);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  ✗ Redis error: {}", e);
                }
            }
        }
        TenantCommands::Remove { name, purge } => {
            println!("Removing tenant '{}'...", name);

            if purge {
                let redis_url = std::env::var("REDIS_URL")
                    .unwrap_or_else(|_| "redis://localhost:6379".to_string());
                match pocketflow_core::SharedStore::new_redis(&redis_url).await {
                    Ok(store) => {
                        let pattern = format!("ns:{}:*", name);
                        let keys: Vec<String> = store.keys(&pattern).await;
                        if !keys.is_empty() {
                            for key in &keys {
                                store.del(key).await;
                            }
                            println!("  ✓ Purged {} keys from Redis", keys.len());
                        }
                    }
                    Err(e) => {
                        eprintln!("  ⚠ Could not purge Redis: {}", e);
                    }
                }
            }

            println!("  ✓ Tenant '{}' removed", name);
            println!("  (Workspaces and chats must be cleaned up manually in the Coder dashboard)");
        }
    }

    Ok(())
}

async fn run_status(tenant: Option<String>, json: bool) -> Result<()> {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let store = pocketflow_core::SharedStore::new_redis(&redis_url)
        .await
        .context("Redis not reachable")?;

    let tenants: Vec<String> = match tenant {
        Some(t) => vec![t],
        None => {
            let keys: Vec<String> = store.keys("ns:*").await;
            let mut set = std::collections::HashSet::new();
            for key in keys {
                if let Some(ns) = key.strip_prefix("ns:") {
                    if let Some(t) = ns.split(':').next() {
                        set.insert(t.to_string());
                    }
                }
            }
            let mut v: Vec<String> = set.into_iter().collect();
            v.sort();
            v
        }
    };

    let mut all_data = Vec::new();

    for t in &tenants {
        let tickets: Vec<config::Ticket> = store
            .get_typed(&format!("ns:{}:tickets", t))
            .await
            .unwrap_or_default();
        let slots: std::collections::HashMap<String, config::WorkerSlot> = store
            .get_typed(&format!("ns:{}:worker_slots", t))
            .await
            .unwrap_or_default();
        let pending_prs: Vec<serde_json::Value> = store
            .get_typed(&format!("ns:{}:pending_prs", t))
            .await
            .unwrap_or_default();

        let data = serde_json::json!({
            "tenant": t,
            "tickets": tickets,
            "worker_slots": slots,
            "pending_prs": pending_prs,
        });
        all_data.push(data);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all_data)?);
    } else {
        for data in &all_data {
            println!("Tenant: {}", data["tenant"].as_str().unwrap_or("?"));
            println!(
                "  Tickets: {}",
                data["tickets"].as_array().map(|v| v.len()).unwrap_or(0)
            );
            println!(
                "  Worker slots: {}",
                data["worker_slots"]
                    .as_object()
                    .map(|v| v.len())
                    .unwrap_or(0)
            );
            println!(
                "  Pending PRs: {}",
                data["pending_prs"].as_array().map(|v| v.len()).unwrap_or(0)
            );
            println!();
        }
    }

    Ok(())
}

async fn run_reset() -> Result<()> {
    let resolver = openflows::orchestration::OrchestrationResolver::new()?;
    let orch_dir = resolver.reset_orchestration_dir()?;
    println!(
        "Orchestration files reset to bundled defaults at: {}",
        orch_dir.display()
    );
    Ok(())
}
