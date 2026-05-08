use agent_forge::ForgePairNode; // Use the event-driven pair node
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
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("Loaded environment from {}", path.display()),
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    tracing_subscriber::fmt::init();

    info!("Starting REAL End-to-End Orchestration (Event-Driven FORGE-SENTINEL Pairs + VESSEL)");

    // 1. Validate Environment
    // Use registry to resolve per-agent token for FORGE
    let registry_path = std::env::current_dir()?
        .join("orchestration")
        .join("agent")
        .join("registry.json");
    let registry = config::Registry::load(&registry_path)?;
    let github_token = registry
        .resolve_github_token("forge")
        .expect("AGENT_FORGE_GITHUB_TOKEN or GITHUB_PERSONAL_ACCESS_TOKEN must be set");
    let repo = std::env::var("GITHUB_REPOSITORY")
        .expect("GITHUB_REPOSITORY must be set (e.g. owner/repo)");

    // Ensure LLM provider is set for AgentRunner
    if std::env::var("LLM_PROVIDER").is_err() {
        std::env::set_var("LLM_PROVIDER", "openai");
    }

    // 2. Clone/Update the target repository workspace
    // Use ~/.agentflow/workspaces as base directory for all workspaces
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

    // Set ORCHESTRATOR_DIR so pair harness can find the plugin
    // This is needed because the workspace is a separate cloned repo
    let orchestrator_dir = std::env::current_dir()?;
    std::env::set_var("ORCHESTRATOR_DIR", &orchestrator_dir);

    // 3. Initialize Nodes
    // NEXUS: Orchestrator that assigns work
    // ForgePairNode: Event-driven FORGE-SENTINEL pair with full review lifecycle
    // VesselNode: Merge gatekeeper - polls CI, merges PRs, emits ticket_merged events
    let persona_path = orchestrator_dir
        .join("orchestration")
        .join("agent")
        .join("agents")
        .join("nexus.agent.md");
    let registry_path = orchestrator_dir
        .join("orchestration")
        .join("agent")
        .join("registry.json");

    let nexus = Arc::new(NexusNode::new(persona_path, registry_path.clone()));
    let forge_pair = Arc::new(ForgePairNode::new(&workspace_dir, &github_token));
    let vessel = Arc::new(VesselNode::from_env());
    let lore = Arc::new(LoreNode::new_with_registry(
        &workspace_dir,
        orchestrator_dir.join("orchestration/agent/agents/lore.agent.md"),
        registry_path,
    )?);

    // 4. Setup Flow with Routing
    // The ForgePairNode handles the full FORGE-SENTINEL lifecycle:
    // - FORGE writes PLAN.md -> SENTINEL reviews -> CONTRACT.md
    // - FORGE implements segments -> SENTINEL evaluates -> segment-N-eval.md
    // - SENTINEL final review -> final-review.md
    // - FORGE opens PR -> STATUS.json
    //
    // VesselNode handles the merge gate:
    // - Polls CI status until terminal (success/failure/timeout)
    // - Detects merge conflicts early via GitHub's `mergeable` field
    // - Attempts conflict resolution (GitHub update-branch or local rebase)
    // - Routes unresolvable conflicts back to forge_pair for rework
    // - Merges PR if CI green
    // - Emits ticket_merged event for dependency resolution
    let flow = Flow::new("nexus")
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
        .add_node(
            "vessel",
            vessel,
            vec![
                (ACTION_DEPLOYED, "lore"),
                (ACTION_DEPLOY_FAILED, "nexus"),
                (ACTION_CI_FIX_NEEDED, "forge_pair"),
                ("merge_blocked", "nexus"),
                (ACTION_CONFLICTS_DETECTED, "forge_pair"),
                (Action::AWAITING_HUMAN, "nexus"),
                ("no_work", "nexus"),
            ],
        )
        .add_node(
            "lore",
            lore,
            vec![(ACTION_DOCS_COMPLETE, "nexus"), (ACTION_NO_WORK, "nexus")],
        );

    // 5. Initialize Shared Store
    let store = SharedStore::new_in_memory();
    store.set("repository", serde_json::json!(repo)).await;

    // Initial tickets list - Nexus will fetch from GitHub if this is empty
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
