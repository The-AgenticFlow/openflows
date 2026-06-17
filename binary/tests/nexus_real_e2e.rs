use agent_nexus::NexusNode;
use anyhow::Result;
use pocketflow_core::{Node, SharedStore};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Real E2E Test for Nexus Agent (No Mocks)
///
/// REQUIRES (Anthropic):
/// - ANTHROPIC_API_KEY
/// - GITHUB_PERSONAL_ACCESS_TOKEN
/// - GITHUB_MCP_TYPE=hosted (or docker)
///
/// REQUIRES (OpenAI / LiteLLM / DeepSeek):
/// - LLM_PROVIDER=openai
/// - OPENAI_API_KEY
/// - OPENAI_MODEL (e.g. gpt-4o, deepseek-chat, or your litellm model)
/// - OPENAI_API_URL (optional, set to http://localhost:4000/v1/chat/completions for LiteLLM)
/// - GITHUB_PERSONAL_ACCESS_TOKEN
/// - GITHUB_MCP_TYPE=hosted (or docker)
///
/// REQUIRES (Gemini):
/// - LLM_PROVIDER=gemini
/// - GEMINI_API_KEY
/// - GEMINI_MODEL (optional, defaults to gemini-2.5-flash)
/// - GEMINI_API_URL or GEMINI_API_BASE (optional, for proxies/custom endpoints)
/// - GITHUB_PERSONAL_ACCESS_TOKEN
/// - GITHUB_MCP_TYPE=hosted (or docker)
///
/// To run:
/// LLM_PROVIDER=openai OPENAI_API_KEY=... cargo test -p agent-team --test nexus_real_e2e -- --ignored
#[tokio::test]
#[ignore] // Ignored by default to avoid failing in CI without keys
async fn test_nexus_real_e2e() -> Result<()> {
    // 1. Initialize Tracing with a clean format
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,agent_client=debug,agent_nexus=debug")
        .with_target(false)
        .try_init();

    println!("\n=== Starting Real Nexus E2E Test ===");

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

    // 3. Initialize Nexus
    println!("Loading Nexus agent persona...");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest dir should have parent")
        .to_path_buf();
    let nexus = Arc::new(NexusNode::new(
        workspace_root.join("orchestration/agent/agents/nexus.agent.md"),
        workspace_root.join("orchestration/agent/registry.json"),
    ));

    // 4. Run NexusNode
    println!("Context injected. Entering Nexus orchestration loop...");
    let action = nexus.run(&store).await?;

    println!("\n=== Nexus Decision Reached ===");
    println!("Action: {}", action.as_str());

    // We expect Nexus to return a valid action.
    // In a real-world test, the model might choose various actions depending on the repo state.
    assert!(
        !action.as_str().is_empty(),
        "Nexus returned an empty action"
    );

    println!("=== Test Finished Successfully ===\n");

    Ok(())
}
