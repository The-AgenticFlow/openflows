use agent_forge::ForgePairNode;
use agent_lore::LoreNode;
use agent_nexus::NexusNode;
use agent_vessel::VesselNode;
use anyhow::Result;
use config::{
    ACTION_CI_FIX_NEEDED, ACTION_CONFLICTS_DETECTED, ACTION_DEPLOYED, ACTION_DEPLOY_FAILED,
    ACTION_DOCS_COMPLETE, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK, ACTION_PR_OPENED,
    ACTION_WORK_ASSIGNED, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS,
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

    // Startup diagnostics
    let default_cli = std::env::var("DEFAULT_CLI").unwrap_or_else(|_| "claude".to_string());
    let has_fireworks = std::env::var("FIREWORKS_API_KEY").is_ok();
    let has_openai = std::env::var("OPENAI_API_KEY").is_ok();
    let has_proxy = std::env::var("PROXY_URL").is_ok();
    let has_anthropic = std::env::var("ANTHROPIC_API_KEY").is_ok();

    info!("═══ AgentFlow Configuration ═══");
    info!("  CLI Backend: {}", default_cli);
    if default_cli == "codex" && has_fireworks {
        info!("  LLM Mode: Codex + Fireworks (direct — no proxy needed)");
    } else if default_cli == "codex" && has_openai {
        let base_url = std::env::var("OPENAI_BASE_URL").ok();
        if let Some(url) = base_url {
            info!("  LLM Mode: Codex + OpenAI-compatible provider at {} (endpoint mode probed at startup)", url);
        } else {
            info!("  LLM Mode: Codex + OpenAI (direct — no proxy needed)");
        }
    } else if default_cli == "claude" && has_anthropic && !has_proxy {
        info!("  LLM Mode: Claude + Direct Anthropic Key");
    } else if has_proxy {
        info!(
            "  LLM Mode: Claude + Proxy at {}",
            std::env::var("PROXY_URL").unwrap_or_default()
        );
    } else {
        warn!("  No valid LLM configuration detected!");
        warn!("    For Codex: set FIREWORKS_API_KEY or OPENAI_API_KEY");
        warn!("    For Claude: set ANTHROPIC_API_KEY or PROXY_URL");
        anyhow::bail!("Invalid LLM configuration — see warnings above");
    }

    let cli_path = match default_cli.as_str() {
        "codex" => std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string()),
        _ => std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string()),
    };
    if which::which(&cli_path).is_err() {
        anyhow::bail!(
            "{} CLI not found at '{}' — install it or set {}_PATH in .env",
            default_cli.to_uppercase(),
            cli_path,
            default_cli.to_uppercase()
        );
    }
    info!("  CLI Binary: {} ({})", default_cli, cli_path);

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
    let vessel = Arc::new(VesselNode::from_env());
    let lore = if registry.get("lore").is_some() {
        let lore_persona = resolver.persona_path("lore.agent.md");
        Some(Arc::new(LoreNode::new_with_registry(
            &workspace_dir,
            lore_persona,
            registry_path.clone(),
        )?))
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
