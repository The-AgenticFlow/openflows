// binary/src/main.rs
mod nodes;
mod state;

use anyhow::Result;
use pocketflow_core::{Action, Flow, SharedStore};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use crate::nodes::{ForgeNode, LoreNode, NexusNode, VesselConfig, VesselNode};
use crate::state::{
    Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_CI_FIX_NEEDED,
    ACTION_CONFLICTS_DETECTED, ACTION_DEPLOYED, ACTION_DEPLOY_FAILED, ACTION_DOCS_COMPLETE,
    ACTION_EMPTY, ACTION_FAILED, ACTION_MERGE_PRS, ACTION_NO_WORK, ACTION_PR_OPENED,
    ACTION_WORK_ASSIGNED, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS,
};

#[tokio::main]
async fn main() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("Loaded environment from {}", path.display()),
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    tracing_subscriber::fmt::init();

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
            },
        ),
        (
            "forge-2".to_string(),
            WorkerSlot {
                id: "forge-2".to_string(),
                status: WorkerStatus::Idle,
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

    // 4. Build Flow - use orchestration/agent directory for personas
    // Resolve orchestrator_dir: first try relative to the binary itself (npm install),
    // then fall back to current directory (dev mode).
    let orchestrator_dir = {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        if let Some(ref dir) = exe_dir {
            if dir.join("orchestration/agent/registry.json").exists() {
                dir.clone()
            } else {
                std::env::current_dir()?
            }
        } else {
            std::env::current_dir()?
        }
    };
    let registry_path = orchestrator_dir.join("orchestration/agent/registry.json");
    let nexus = Arc::new(NexusNode::new(
        orchestrator_dir.join("orchestration/agent/agents/nexus.agent.md"),
        registry_path.clone(),
    ));
    let forge = Arc::new(ForgeNode::new_with_registry(
        &workspace_dir,
        orchestrator_dir.join("orchestration/agent/agents/forge.agent.md"),
        registry_path.clone(),
    ));
    let vessel = Arc::new(VesselNode::new(
        VesselConfig::from_registry(&registry_path).unwrap_or_else(|e| {
            warn!(error = %e, "Failed to load vessel config from registry, using fallback");
            VesselConfig::from_env()
        }),
    ));
    let lore = Arc::new(LoreNode::new_with_registry(
        &workspace_dir,
        orchestrator_dir.join("orchestration/agent/agents/lore.agent.md"),
        orchestrator_dir.join("orchestration/agent/registry.json"),
    )?);

    let flow = Flow::new("nexus")
        .add_node(
            "nexus",
            nexus,
            vec![
                (ACTION_WORK_ASSIGNED, "forge"),
                (ACTION_MERGE_PRS, "vessel"),
                (ACTION_NO_WORK, "nexus"),
                ("approve_command", "forge"),
                ("reject_command", "nexus"),
            ],
        )
        .add_node(
            "forge",
            forge,
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
            vec![
                (ACTION_DEPLOYED, "lore"),
                (ACTION_DEPLOY_FAILED, "nexus"),
                (ACTION_CI_FIX_NEEDED, "forge"),
                ("merge_blocked", "nexus"),
                (ACTION_CONFLICTS_DETECTED, "forge"),
                (Action::AWAITING_HUMAN, "nexus"),
                ("no_work", "nexus"),
            ],
        )
        .add_node(
            "lore",
            lore,
            vec![(ACTION_DOCS_COMPLETE, "nexus"), (ACTION_NO_WORK, "nexus")],
        )
        .max_steps(20);

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
