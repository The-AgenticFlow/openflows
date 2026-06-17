use agent_nexus::NexusNode;
use anyhow::Result;
use mockito::Server;
use pocketflow_core::{Node, SharedStore};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn test_nexus_e2e_mocked() -> Result<()> {
    // 1. Start Mockito server to mock Anthropic API
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/messages", server.url());

    // Mock Anthropic response
    // First turn: Tool use (list_issues)
    let _m1 = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "id": "msg_123",
                "type": "message",
                "role": "assistant",
                "content": [{
                    "id": "call_1",
                    "type": "tool_use",
                    "name": "list_issues",
                    "input": {}
                }],
                "stop_reason": "tool_use"
            })
            .to_string(),
        )
        .create_async()
        .await;

    // Second turn: Final decision
    let _m2 = server.mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({
            "id": "msg_456",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "I see the issues. Assigning T-001.\n{\"action\": \"work_assigned\", \"notes\": \"Assigned T-001\"}"
            }],
            "stop_reason": "end_turn"
        }).to_string())
        .create_async().await;

    // 2. Setup environment variables for AgentRunner
    std::env::set_var("ANTHROPIC_API_KEY", "test-key");
    std::env::set_var("ANTHROPIC_API_URL", &url);
    std::env::set_var("GITHUB_MCP_CMD", "python3 ../scripts/mock_mcp.py");
    std::env::set_var("AGENT_NEXUS_GITHUB_TOKEN", "ghp_test_token_for_e2e");

    // 3. Create a temporary registry.json so the test doesn't depend on a
    //    workspace file that may not exist in CI.
    let tmp_dir = tempfile::tempdir()?;
    let registry_path = tmp_dir.path().join("registry.json");
    std::fs::write(
        &registry_path,
        json!({
            "default_cli": "claude",
            "team": [{
                "id": "nexus",
                "cli": "claude",
                "active": true,
                "instances": 1,
                "model_backend": "anthropic/claude-haiku-4-5-20251001",
                "routing_key": "nexus-key",
                "github_token_env": "AGENT_NEXUS_GITHUB_TOKEN"
            }]
        })
        .to_string(),
    )?;

    // 4. Initialize SharedStore
    let store = SharedStore::new_in_memory();

    // Inject initial worker slots
    let slots = json!({
        "forge-1": { "id": "forge-1", "status": { "type": "idle" } }
    });
    store.set("worker_slots", slots).await;

    // 5. Run NexusNode
    // Resolve persona path relative to the workspace root so the test works from any CWD.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest dir should have parent")
        .to_path_buf();
    let nexus = Arc::new(NexusNode::new(
        workspace_root.join("orchestration/agent/agents/nexus.agent.md"),
        registry_path,
    ));

    let action = nexus.run(&store).await?;
    assert_eq!(action.as_str(), "work_assigned");

    Ok(())
}
