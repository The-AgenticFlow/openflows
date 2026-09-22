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
    Run {
        /// Clear this tenant's runtime SharedStore state before starting
        #[arg(long)]
        reset_store: bool,
    },
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
    /// Gate approval for phase transitions
    Gate {
        #[command(subcommand)]
        action: GateCommands,
    },
    /// Coder agent lifecycle hooks (experimental) — simulate/test the consumer
    Hooks {
        #[command(subcommand)]
        action: HooksCommands,
    },
    /// Reset orchestration files to bundled defaults
    ResetOrchestration,
    /// Manually manage/clean the shared Redis store
    Store {
        #[command(subcommand)]
        action: StoreCommands,
    },
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
    /// Clean stale state: reset tickets stuck in awaiting_human/failed back to Open
    Clean {
        /// Tenant name
        name: String,
        /// Reset ALL tickets to Open (not just stale ones)
        #[arg(long)]
        reset_all: bool,
    },
    /// Remove a tenant: archive chats, delete workspaces, optionally purge Redis
    Remove {
        /// Tenant name
        name: String,
        /// Also purge ns:{tenant}:* from Redis
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Subcommand)]
enum StoreCommands {
    /// List tenants in the shared store and how many keys each holds (read-only)
    List,
    /// Purge a tenant's entire keyspace (ns:{tenant}:*) from the shared store
    Purge {
        /// Tenant name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Clear a tenant's runtime orchestration state, including ticket-scoped
    /// worker keys such as status/chat/review/gate/handoff
    Reset {
        /// Tenant name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Purge the ENTIRE shared store — every key across all tenants (global reset)
    Wipe {
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum GateCommands {
    /// Approve a phase transition gate
    Approve {
        /// Tenant name
        #[arg(long)]
        tenant: String,
        /// Ticket ID
        #[arg(long)]
        ticket: String,
        /// Phase to approve (e.g., planning, building)
        #[arg(long)]
        phase: String,
        /// Approver role (e.g., SENTINEL)
        #[arg(long)]
        approver: Option<String>,
        /// Optional approval notes
        #[arg(long)]
        notes: Option<String>,
    },
    /// Check gate approval status
    Status {
        /// Tenant name
        #[arg(long)]
        tenant: String,
        /// Ticket ID
        #[arg(long)]
        ticket: String,
        /// Phase to check (e.g., planning, building)
        #[arg(long)]
        phase: String,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Simulate Coder dispatching a lifecycle hook event to the consumer.
    ///
    /// Signs a JWT exactly like Coder's `chatd` (HS256, iss=coder,
    /// aud=CODER_CHAT_HOOK_URL, jti=dispatch_id, body_sha256) and POSTs it to
    /// the configured URL. Lets you exercise the consumer end-to-end without a
    /// Coder server.
    Simulate {
        /// Event to dispatch (session_start, user_prompt_submit, pre_tool_use,
        /// post_tool_use, pre_compact, post_compact, stop)
        #[arg(long, default_value = "session_start")]
        event: String,
        /// Chat ID to tag the event with (defaults to a generated id)
        #[arg(long)]
        chat_id: Option<String>,
        /// Optional dispatch ID (defaults to a generated one)
        #[arg(long)]
        dispatch_id: Option<String>,
    },
    /// Run the hook consumer standalone (dev/test). Uses an in-memory store so
    /// it needs no Redis or Coder. Requires CODER_CHAT_HOOK_SECRET at startup.
    Serve,
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

    match cli.command.unwrap_or(Commands::Run { reset_store: false }) {
        Commands::Run { reset_store } => run_controller(reset_store).await,
        Commands::Bootstrap => run_bootstrap().await,
        Commands::Tenant { action } => run_tenant(action).await,
        Commands::Status { tenant, json } => run_status(tenant, json).await,
        Commands::Doctor => openflows::doctor::run_checks().await,
        Commands::Gate { action } => run_gate(action).await,
        Commands::Hooks { action } => run_hooks(action).await,
        Commands::ResetOrchestration => run_reset().await,
        Commands::Store { action } => run_store(action).await,
    }
}

async fn run_controller(reset_store: bool) -> Result<()> {
    let cfg = config::EnvConfig::from_env().context("failed to load environment configuration")?;
    cfg.validate_controller()?;
    let coder_url = cfg.coder.url.clone();
    let _coder_token = cfg.coder.effective_token();
    let redis_url = cfg.infra.effective_redis_url();
    let tenant = cfg.tenant.effective_tenant().to_string();

    // ── Initialize SharedStore (Redis required — no in-memory fallback) ─
    // Tenant-aware: all keys are prefixed with ns:{tenant}: for isolation
    let store =
        pocketflow_core::SharedStore::new_redis_with_tenant(&redis_url, Some(tenant.clone()))
            .await?;
    if reset_store {
        let cleared = reset_tenant_runtime_state(&store).await;
        tracing::info!(
            tenant,
            cleared,
            "Reset tenant runtime SharedStore state before controller start"
        );
    }

    // Resolve the target repository: prefer the environment (injected per-tenant
    // by the nexus template as GITHUB_REPOSITORY), then fall back to the tenant's
    // Redis `repository` key. Fail fast if neither is available so the controller
    // never runs silently without a repo to process.
    let github_repo = match cfg.github.repository.clone() {
        Some(repo) if !repo.is_empty() => repo,
        _ => store
            .get("repository")
            .await
            .and_then(|v| v.as_str().map(String::from))
            .filter(|s| !s.is_empty())
            .context(
                "No target repository configured: set GITHUB_REPOSITORY (host/dev run) or add a \
                 tenant first with `openflows tenant add <owner/repo>`.",
            )?,
    };

    tracing::info!(
        coder_url,
        redis_url,
        tenant,
        github_repo,
        "OpenFlows Controller starting (Coder-only mode)"
    );

    // The relay runs as a background HTTP server, handling A2A JSON-RPC
    // requests from Sentinel/Forge workspaces (verify requests, streaming
    // progress, result mirroring to Redis).
    let a2a_relay = match agent_nexus::a2a::start_a2a_relay(std::sync::Arc::new(store.clone()))
        .await
    {
        Ok(relay) => {
            tracing::info!("A2A relay started successfully");
            Some(relay)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to start A2A relay; verify requests will not be available");
            None
        }
    };

    // ── Resolve orchestration directory ─────────────────────────────────
    // Do this before the hook server so Slice A can hand the consumer the
    // persona/skills/command paths resolved here.
    let resolver = openflows::orchestration::OrchestrationResolver::new()?;
    let orch_dir = resolver.ensure_orchestration_dir()?;
    resolver.validate()?;

    let registry_path = resolver.registry_path();
    let registry = config::Registry::load(&registry_path)?;
    let registry_json = serde_json::to_string_pretty(&registry)?;
    std::env::set_var("OPENFLOWS_REGISTRY_PATH", &registry_path);
    std::env::set_var("OPENFLOWS_REGISTRY_JSON", &registry_json);
    std::env::set_var("ARTIFACTS_DIR", resolver.orchestrator_dir());

    store
        .set("registry_json", serde_json::json!(registry_json))
        .await;

    // Build the experimental hook-driver context: the reactive kick channel
    // (Slice B/D, Redis pub/sub) and the bootstrap context (Slice A).
    let hook_kick_bus = pocketflow_core::build_kick_bus(Some(&redis_url), &tenant)
        .await
        .ok();
    let (hook_publisher, mut hook_kick_rx) = match hook_kick_bus {
        Some((p, r)) => (Some(p), Some(r)),
        None => {
            tracing::warn!("Failed to build hook kick bus; reactive wake disabled");
            (None, None)
        }
    };

    let hook_bootstrap = {
        let mut persona_by_role = std::collections::HashMap::new();
        for role in ["forge", "sentinel", "vessel", "lore", "nexus"] {
            let path = resolver.persona_path(&format!("{role}.agent.md"));
            persona_by_role.insert(role.to_string(), path);
        }
        Some(agent_nexus::hooks::HookBootstrapContext {
            persona_by_role,
            skills_dir: Some(orch_dir.join("plugin/skills")),
            commands_dir: Some(orch_dir.join("plugin/commands")),
        })
    };

    // ── Coder Agent Lifecycle Hooks (experimental) ──────────────────────
    // OpenFlows consumes Coder's deployment-wide lifecycle webhook so we can
    // observe and centrally policy agent events. Coder's experiment flag and URL
    // are wired by compose; this process starts the consumer when it has the
    // shared hook secret.
    match agent_nexus::hooks::start_lifecycle_hook_server(
        std::sync::Arc::new(store.clone()),
        cfg.hooks.clone(),
        hook_publisher,
        hook_bootstrap,
    )
    .await
    {
        Ok(Some(())) => {
            if cfg.hooks.hook_logs {
                tracing::info!("Coder lifecycle hook consumer started");
            }
        }
        Ok(None) => {
            if cfg.hooks.hook_logs {
                tracing::debug!("Coder lifecycle hook consumer disabled");
            }
        }
        Err(e) => {
            // Experimental: warn but do not fail the controller boot.
            tracing::warn!(
                error = %e,
                "Failed to start Coder lifecycle hook consumer; continuing without hooks"
            );
        }
    }

    // ── Build flow nodes ────────────────────────────────────────────────
    let nexus_persona = resolver.persona_path("nexus.agent.md");
    let mut nexus_node = openflows::nodes::NexusNode::new(nexus_persona, registry_path.clone());
    if let Some(ref relay) = a2a_relay {
        nexus_node = nexus_node.with_a2a_relay(relay.clone());
    }
    let nexus = std::sync::Arc::new(nexus_node);
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
        ACTION_DOCS_COMPLETE, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK,
        ACTION_PLANNING_GATE, ACTION_PR_OPENED, ACTION_WORK_ASSIGNED,
    };

    let review_approve = "review_approve";
    let review_reject = "review_reject";
    let review_ready = "review_ready"; // Forge signals work is ready for PR review

    let mut flow = pocketflow_core::Flow::new("nexus")
        .add_node(
            "nexus",
            nexus,
            vec![
                (ACTION_WORK_ASSIGNED, "forge_pair"),
                (ACTION_MERGE_PRS, "vessel"),
                ("approve_command", "forge_pair"),
                ("reject_command", "nexus"),
                ("sentinel_spawned", "sentinel"), // After spawning Sentinel, route to it
            ],
        )
        .add_node(
            "forge_pair",
            forge_pair,
            vec![
                (ACTION_PR_OPENED, "sentinel"),
                (ACTION_PLANNING_GATE, "nexus"), // Forge at planning gate → NEXUS spawns SENTINEL
                (review_ready, "nexus"),         // Forge review_ready → NEXUS spawns SENTINEL
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
    let mut last_pass;
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
        last_pass = std::time::Instant::now();

        // Wait either for the full poll interval or for a hook kick to wake us
        // early (Slice B/D). The reconciliation pass is idempotent, so an early
        // wake only shortens latency. Guard against a kick-in-every-pass
        // livelock by enforcing a small minimum delay after each pass.
        if let Some(rx) = &mut hook_kick_rx {
            let kick = tokio::select! {
                _ = tokio::time::sleep(CONTROLLER_POLL_INTERVAL) => None,
                k = rx.recv() => k,
            };
            if let Some(k) = kick {
                let elapsed = last_pass.elapsed();
                const MIN_HOOK_WAKE_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
                if elapsed < MIN_HOOK_WAKE_DELAY {
                    tokio::time::sleep(MIN_HOOK_WAKE_DELAY - elapsed).await;
                }
                if cfg.hooks.hook_logs {
                    tracing::info!(
                        event = %k.event,
                        hint = %k.hint,
                        "Hook kick woke the Controller early"
                    );
                }
                continue; // re-run the reconciliation pass immediately
            }
            // poll interval elapsed; fall through to next pass
        } else {
            tokio::time::sleep(CONTROLLER_POLL_INTERVAL).await;
        }
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

    println!("\nBootstrap complete.");
    Ok(())
}

fn validate_tenant_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.is_empty(), "tenant name must not be empty");
    anyhow::ensure!(
        name.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "tenant name '{name}' contains characters that are not allowed in Redis namespace operations; use only ASCII letters, numbers, '.', '_' and '-'"
    );
    Ok(())
}

async fn run_tenant_clean(action: &TenantCommands) -> Result<()> {
    let TenantCommands::Clean { name, reset_all } = action else {
        unreachable!("run_tenant_clean called with non-clean action");
    };
    validate_tenant_name(name)?;

    let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();
    // Scope the store to the tenant we are cleaning: SharedStore namespaces
    // every key as `ns:{tenant}:{key}`, so we pass it the tenant and use plain
    // keys below. (We must NOT also hand-format `ns:{name}:` prefixes here,
    // which would double the namespace to `ns:{name}:ns:{name}:...` and fail
    // to touch the real ticket keys.)
    let store =
        match pocketflow_core::SharedStore::new_redis_with_tenant(&redis_url, Some(name.clone()))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  ✗ Redis error: {}", e);
                return Ok(());
            }
        };

    let mut tickets: Vec<serde_json::Value> = store.get_typed("tickets").await.unwrap_or_default();

    let mut reset_count = 0;

    for ticket in tickets.iter_mut() {
        let status = ticket.get("status");
        let is_stale = status
            .map(|s| {
                let stype = match s {
                    serde_json::Value::Object(obj) => {
                        obj.get("type").and_then(|v| v.as_str()).unwrap_or("")
                    }
                    serde_json::Value::String(s) => s.as_str(),
                    _ => "",
                };
                stype == "awaiting_human" || stype == "failed"
            })
            .unwrap_or(false);

        let should_reset = is_stale || *reset_all;

        if should_reset {
            // Reset the status object to Open format
            ticket["status"] = serde_json::json!({
                "type": "open"
            });
            if let Some(obj) = ticket.as_object_mut() {
                obj.insert("attempts".to_string(), serde_json::json!(0));
            }
            reset_count += 1;
        }
    }

    if reset_count > 0 {
        store.set("tickets", serde_json::to_value(&tickets)?).await;
        println!("  ✓ Reset {} ticket(s) to Open", reset_count);
    } else {
        println!("  (no stale tickets found)");
    }

    // Also clear recovery attempt counters.
    // keys()/raw_keys() return full Redis keys (ns:{tenant}:...), so delete
    // them with raw_del to avoid re-applying the tenant namespace.
    let recovery_pattern = format!("ns:{}:ticket:*:recovery_attempts", name);
    let recovery_keys: Vec<String> = store.raw_keys(&recovery_pattern).await;
    let recovery_count = recovery_keys.len();
    for key in &recovery_keys {
        store.raw_del(key).await;
    }
    if recovery_count > 0 {
        println!("  ✓ Cleared {} recovery counters", recovery_count);
    }

    // Clear worker_slots to prevent stale workspace IDs triggering premature provisioning
    store.set("worker_slots", serde_json::json!({})).await;
    println!("  ✓ Cleared worker slots (stale workspace references)");

    println!("  ✓ Tenant '{}' cleaned", name);
    println!("  (Restart the controller to pick up changes)");

    Ok(())
}

async fn run_tenant(action: TenantCommands) -> Result<()> {
    // Clean command doesn't need bootstrap - handle it completely separately
    if matches!(action, TenantCommands::Clean { .. }) {
        return run_tenant_clean(&action).await;
    }

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
            validate_tenant_name(&tenant_name)?;

            println!(
                "Adding tenant '{}' for repository '{}'...",
                tenant_name, repo
            );
            let workspace_id = bootstrapper
                .ensure_tenant(&client, &tenant_name, &repo)
                .await
                .context("Tenant setup failed")?;

            // Persist the tenant's repository into its Redis namespace so a host /
            // dev controller run without GITHUB_REPOSITORY can resolve the repo
            // from the tenant store (the same `repository` key the Nexus node
            // writes once its flow starts running). Without this, a controller
            // started right after `tenant add` aborts telling the operator to add
            // a tenant even though one was just added.
            let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();
            match pocketflow_core::SharedStore::new_redis_with_tenant(
                &redis_url,
                Some(tenant_name.clone()),
            )
            .await
            {
                Ok(store) => {
                    store.set("repository", serde_json::json!(&repo)).await;
                    println!(
                        "  ✓ Repository '{}' persisted for tenant '{}'",
                        repo, tenant_name
                    );
                }
                Err(e) => {
                    eprintln!(
                        "  ⚠ Could not persist repository '{}' to Redis for tenant '{}': {}",
                        repo, tenant_name, e
                    );
                }
            }

            println!("\n  ✓ Tenant '{}' added", tenant_name);
            println!("  ✓ Nexus workspace: {}", workspace_id);
            println!("  → Complete the GitHub OAuth link in the Coder dashboard for this tenant");
        }
        TenantCommands::List => {
            println!("Tenants (from Redis namespaces):");
            // Read all ns:* keys from Redis and list unique tenants
            let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();
            match pocketflow_core::SharedStore::new_redis(&redis_url).await {
                Ok(store) => {
                    // raw_keys scans full Redis keys (`ns:*`) without re-applying
                    // the tenant namespace, so we can enumerate all tenants.
                    let keys: Vec<String> = store.raw_keys("ns:*").await;
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
            validate_tenant_name(&name)?;
            println!("Removing tenant '{}'...", name);

            if purge {
                let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();
                match pocketflow_core::SharedStore::new_redis(&redis_url).await {
                    Ok(store) => {
                        // raw_keys + raw_del operate on full keys, so we purge the
                        // real `ns:{name}:*` namespace instead of re-prefixing it.
                        let pattern = format!("ns:{}:*", name);
                        let keys: Vec<String> = store.raw_keys(&pattern).await;
                        if !keys.is_empty() {
                            for key in &keys {
                                store.raw_del(key).await;
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
        TenantCommands::Clean { .. } => {
            // Already handled above, this arm exists only to satisfy exhaustive match
        }
    }

    Ok(())
}

async fn runtime_state_keys(
    store: &pocketflow_core::SharedStore,
) -> std::collections::BTreeSet<String> {
    let exact_keys = [
        "tickets",
        "worker_slots",
        "pending_prs",
        "open_prs",
        "command_gate",
        "_no_work_count",
        "_hook_events_tail",
        "registry_json",
        "ci_readiness",
        "repository",
        "documentation_queue",
    ];
    let patterns = ["ticket:*", "pair:*", "heartbeat:*", "audit:*"];

    let mut keys = std::collections::BTreeSet::new();
    for key in exact_keys {
        keys.extend(store.keys(key).await);
    }
    for pattern in patterns {
        keys.extend(store.keys(pattern).await);
    }
    keys
}

async fn reset_tenant_runtime_state(store: &pocketflow_core::SharedStore) -> usize {
    let keys = runtime_state_keys(store).await;
    let count = keys.len();
    for key in keys {
        store.raw_del(&key).await;
    }
    count
}

async fn run_store(action: StoreCommands) -> Result<()> {
    let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();
    let store = pocketflow_core::SharedStore::new_redis(&redis_url)
        .await
        .context("Redis not reachable")?;

    match action {
        StoreCommands::List => {
            // raw_keys scans full Redis keys (`ns:*`) without re-applying the
            // tenant namespace, so we can enumerate every tenant and count keys.
            let keys: Vec<String> = store.raw_keys("ns:*").await;
            let mut counts: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for key in keys {
                if let Some(ns) = key.strip_prefix("ns:") {
                    if let Some(tenant) = ns.split(':').next() {
                        *counts.entry(tenant.to_string()).or_insert(0) += 1;
                    }
                }
            }
            if counts.is_empty() {
                println!("  (shared store is empty — no tenants found)");
            } else {
                for (tenant, count) in &counts {
                    println!("  - {}: {} key(s)", tenant, count);
                }
            }
        }
        StoreCommands::Purge { name, yes } => {
            validate_tenant_name(&name)?;
            let pattern = format!("ns:{}:*", name);
            let keys: Vec<String> = store.raw_keys(&pattern).await;
            if keys.is_empty() {
                println!("  ✗ No keys found for tenant '{}'", name);
                return Ok(());
            }
            println!(
                "  Purging {} key(s) for tenant '{}' from the shared store...",
                keys.len(),
                name
            );
            if !yes && !confirm()? {
                println!("  Aborted — no keys were removed.");
                return Ok(());
            }
            for key in &keys {
                store.raw_del(key).await;
            }
            println!("  ✓ Purged {} key(s) for tenant '{}'", keys.len(), name);
            println!("  (Restart the controller to pick up changes)");
        }
        StoreCommands::Reset { name, yes } => {
            validate_tenant_name(&name)?;
            let tenant_store = match pocketflow_core::SharedStore::new_redis_with_tenant(
                &redis_url,
                Some(name.clone()),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  ✗ Redis error: {}", e);
                    return Ok(());
                }
            };

            let matching = runtime_state_keys(&tenant_store).await;
            if matching.is_empty() {
                println!(
                    "  (no runtime orchestration state found for tenant '{}')",
                    name
                );
                println!("  (Restart the controller to pick up changes)");
                return Ok(());
            }

            println!(
                "  Resetting {} runtime orchestration key(s) for tenant '{}'",
                matching.len(),
                name
            );
            if !yes && !confirm()? {
                println!("  Aborted — no keys were removed.");
                return Ok(());
            }

            let cleared = reset_tenant_runtime_state(&tenant_store).await;
            if cleared > 0 {
                println!(
                    "  ✓ Cleared {} orchestration key(s) for tenant '{}'",
                    cleared, name
                );
            } else {
                println!("  (no orchestration state found for tenant '{}')", name);
            }
            println!("  (Restart the controller to pick up changes)");
        }
        StoreCommands::Wipe { yes } => {
            // raw_keys("ns:*") matches every tenant namespace plus any other
            // shared keys, so this is a true whole-store global reset.
            let keys: Vec<String> = store.raw_keys("ns:*").await;
            if keys.is_empty() {
                println!("  (shared store is already empty — nothing to wipe)");
                return Ok(());
            }
            println!(
                "  Wiping the ENTIRE shared store: {} key(s) across all tenants / namespaces.",
                keys.len()
            );
            println!("  This is destructive and cannot be undone. All tenants' state is removed.");
            if !yes && !confirm()? {
                println!("  Aborted — no keys were removed.");
                return Ok(());
            }
            for key in &keys {
                store.raw_del(key).await;
            }
            println!("  ✓ Wiped {} key(s) from the shared store", keys.len());
            println!("  (Restart any controller to pick up the clean state)");
        }
    }

    Ok(())
}

fn confirm() -> Result<bool> {
    use std::io::Write;
    print!("  Confirm: type 'yes' to continue: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("yes"))
}

async fn run_status(tenant: Option<String>, json: bool) -> Result<()> {
    let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();

    // raw_keys scans full Redis keys without namespacing, so we can enumerate
    // all `ns:{tenant}:` namespaces from a single store.
    let scan_store = pocketflow_core::SharedStore::new_redis(&redis_url)
        .await
        .context("Redis not reachable")?;

    let tenants: Vec<String> = match tenant {
        Some(t) => vec![t],
        None => {
            let keys: Vec<String> = scan_store.raw_keys("ns:*").await;
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
        // Per-tenant scoped store: SharedStore namespaces keys with the
        // tenant, so read plain keys rather than hand-formatted `ns:{t}:` ones.
        let store =
            match pocketflow_core::SharedStore::new_redis_with_tenant(&redis_url, Some(t.clone()))
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  ⚠ Could not read tenant '{}': {}", t, e);
                    continue;
                }
            };
        let tickets: Vec<config::Ticket> = store.get_typed("tickets").await.unwrap_or_default();
        let slots: std::collections::HashMap<String, config::WorkerSlot> =
            store.get_typed("worker_slots").await.unwrap_or_default();
        let pending_prs: Vec<serde_json::Value> =
            store.get_typed("pending_prs").await.unwrap_or_default();

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

async fn run_gate(action: GateCommands) -> Result<()> {
    use openflows_harness::Harness;

    let redis_url = config::EnvConfig::from_env()?.infra.effective_redis_url();

    match action {
        GateCommands::Approve {
            tenant,
            ticket,
            phase,
            approver,
            notes,
        } => {
            let store = Harness::new(&redis_url, &tenant).await?;
            let approver_role = approver.as_deref().unwrap_or("SENTINEL");
            // Do NOT prepend the tenant to the ticket id: Harness::new already
            // applies the `ns:{tenant}:` namespace, and gate_approve builds its
            // keys from `ticket:{}:status` / `ticket:{}:gate:{phase}`. Pre-joining
            // `<tenant>:<ticket>` here would double the tenant in the key
            // (`ns:{tenant}:ticket:{tenant}:{ticket}:...`) and diverge from the
            // worker harness (which writes `ns:{tenant}:ticket:{ticket}:...`),
            // so the approval would land in a key FORGE never reads.
            store
                .gate_approve(&ticket, approver_role, &phase, notes.as_deref())
                .await?;
            println!("✓ Gate approval recorded: {} phase for {}", phase, ticket);
        }
        GateCommands::Status {
            tenant,
            ticket,
            phase,
        } => {
            let store = Harness::new(&redis_url, &tenant).await?;
            store.gate_status(&ticket, &phase).await?;
        }
    }

    Ok(())
}

async fn run_hooks(action: HooksCommands) -> Result<()> {
    match action {
        HooksCommands::Simulate {
            event,
            chat_id,
            dispatch_id,
        } => {
            let cfg = config::EnvConfig::from_env()
                .context("failed to load environment configuration")?;
            let hook_url = hook_simulate_url(&cfg.hooks)?;
            let secret = cfg.hooks.chat_hook_secret.clone().context(
                "CODER_CHAT_HOOK_SECRET is not set. It must match the consumer's.\n\
                 Generate one with: openssl rand -hex 32",
            )?;
            // Generate stable-ish test ids from the process id + timestamp (no
            // extra dependency needed in the binary crate).
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let pid = std::process::id();
            let dispatch_id =
                dispatch_id.unwrap_or_else(|| format!("sim-{}-{}", event.replace('_', "-"), pid));
            let chat_id = chat_id.unwrap_or_else(|| format!("chat-sim-{}-{}", pid, ts));

            println!(
                "Simulating Coder dispatch → `{}` (event={event}, dispatch_id={dispatch_id})",
                hook_url
            );
            let (status, body) = agent_nexus::hooks::dispatch_simulated_event(
                &hook_url,
                &secret,
                &event,
                &chat_id,
                &dispatch_id,
            )
            .await?;
            println!("Consumer responded with HTTP {status}");
            if !body.trim().is_empty() {
                println!("Response body: {body}");
            }
            if status == 200 || status == 204 {
                println!("✓ Event observed by the OpenFlows hook consumer.");
            } else {
                println!("✗ Hook consumer rejected the event. Check the consumer logs.");
            }
        }
        HooksCommands::Serve => {
            // Standalone consumer for local testing: in-memory store (no Redis),
            // so `openflows hooks simulate` can fire events at it end-to-end.
            let mut cfg = config::EnvConfig::from_env().context(
                "failed to load environment configuration (CODER_CHAT_HOOK_SECRET must be exported)",
            )?;
            if std::env::var("OPENFLOWS_HOOK_HOST")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .is_none()
            {
                cfg.hooks.hook_host = "127.0.0.1".to_string();
            }
            let store = std::sync::Arc::new(pocketflow_core::SharedStore::new_in_memory());
            // In-memory kick bus so slice B/D wake behaviour can be exercised
            // locally even without Redis.
            let kick_publisher = pocketflow_core::build_kick_bus(None, "default")
                .await
                .map(|(p, _r)| p)
                .ok();
            tracing::info!(
                hook_url = %cfg.hooks.hook_public_url().unwrap_or_default(),
                "Starting standalone lifecycle hook consumer (in-memory store)"
            );
            match agent_nexus::hooks::start_lifecycle_hook_server(
                store,
                cfg.hooks.clone(),
                kick_publisher,
                None,
            )
            .await
            {
                Ok(Some(())) => {
                    println!("Listening on {}", cfg.hooks.hook_addr);
                    println!(
                        "Run `openflows hooks simulate` with the same CODER_CHAT_HOOK_SECRET."
                    );
                    // Keep the process alive serving requests.
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    }
                }
                Ok(None) => {
                    anyhow::bail!("consumer not started: set CODER_CHAT_HOOK_SECRET");
                }
                Err(e) => anyhow::bail!("failed to start consumer: {e:#}"),
            }
        }
    }

    Ok(())
}

fn hook_simulate_url(hooks: &config::env::CoderHooksConfig) -> Result<String> {
    if let Ok(url) = std::env::var("CODER_CHAT_HOOK_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }

    let port = hooks
        .port()
        .context("OPENFLOWS_HOOK_ADDR has no usable port for hook simulation")?;
    Ok(format!("http://127.0.0.1:{port}/experimental/hooks/chat"))
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
