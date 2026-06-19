use agent_nexus::NexusNode;
use anyhow::Result;
use pocketflow_core::{Node, SharedStore};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Real E2E Test for Nexus Agent using Codex (OpenAI) backend.
///
/// REQUIRES:
/// - LLM_PROVIDER=openai
/// - OPENAI_API_KEY
/// - OPENAI_MODEL (e.g. gpt-4o-mini, deepseek-chat)
/// - GITHUB_PERSONAL_ACCESS_TOKEN
/// - GITHUB_MCP_TYPE=hosted (or docker)
///
/// To run:
/// LLM_PROVIDER=openai OPENAI_API_KEY=... cargo test -p agent-team --test nexus_real_e2e_codex -- --ignored
#[tokio::test]
#[ignore]
async fn test_nexus_real_e2e_codex() -> Result<()> {
    // 1. Initialize Tracing with a clean format
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,agent_client=debug,agent_nexus=debug")
        .with_target(false)
        .try_init();

    println!("\n=== Starting Real Nexus E2E Codex Test ===");

    // 2. Initialize SharedStore with real-world targets
    let store = SharedStore::new_in_memory();

    // Inject worker slots so Nexus has someone to assign to
    println!("Setting up worker slots...");
    let slots = json!({
        "forge-1": {
            "id": "forge-1",
            "status": { "type": "idle" }
        }
    });
    store.set("worker_slots", slots).await;

    // Inject the target repository into the context
    let repo = "Christiantyemele/Soft-Dev";
    println!("Target Repository: {}", repo);
    store.set("repository", json!(repo)).await;

    // 3. Create a temporary registry.json with codex CLI backend
    let tmp_dir = tempfile::tempdir()?;
    let registry_path = tmp_dir.path().join("registry.json");
    std::fs::write(
        &registry_path,
        json!({
            "default_cli": "codex",
            "team": [{
                "id": "nexus",
                "cli": "codex",
                "active": true,
                "instances": 1,
                "model_backend": "openai/gpt-4o-mini",
                "routing_key": "nexus-key",
                "github_token_env": "AGENT_NEXUS_GITHUB_TOKEN"
            }]
        })
        .to_string(),
    )?;

    // 4. Initialize Nexus
    println!("Loading Nexus agent persona (codex backend)...");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest dir should have parent")
        .to_path_buf();
    let nexus = Arc::new(NexusNode::new(
        workspace_root.join("orchestration/agent/agents/nexus.agent.md"),
        registry_path,
    ));

    // 5. Run NexusNode
    println!("Context injected. Entering Nexus orchestration loop (codex backend)...");
    let action = nexus.run(&store).await?;

    println!("\n=== Nexus Decision Reached (codex) ===");
    println!("Action: {}", action.as_str());

    // We expect Nexus to return a valid action.
    assert!(
        !action.as_str().is_empty(),
        "Nexus returned an empty action"
    );

    println!("=== Test Finished Successfully (codex) ===\n");

    Ok(())
}
