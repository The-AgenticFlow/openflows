use agent_nexus::NexusNode;
use anyhow::Result;
use mockito::Server;
use pocketflow_core::{Node, SharedStore};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn test_nexus_e2e_codex_mocked() -> Result<()> {
    // 1. Start Mockito server to mock OpenAI API
    let mut server = Server::new_async().await;
    let base_url = server.url();

    // Mock OpenAI chat completion response (first turn: tool_use / function_call)
    // OpenAiClient appends /chat/completions to OPENAI_BASE_URL
    let _m1 = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "id": "chatcmpl-codex-1",
                "object": "chat.completion",
                "model": "gpt-4o-mini",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_codex_1",
                            "type": "function",
                            "function": {
                                "name": "list_issues",
                                "arguments": "{}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })
            .to_string(),
        )
        .create_async()
        .await;

    // Second turn: final decision
    let _m2 = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "id": "chatcmpl-codex-2",
                "object": "chat.completion",
                "model": "gpt-4o-mini",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "I see the issues. Assigning T-001.\n{\"action\": \"work_assigned\", \"notes\": \"Assigned T-001\"}",
                        "tool_calls": null
                    },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35 }
            })
            .to_string(),
        )
        .create_async()
        .await;

    // 2. Setup environment variables for OpenAI (Codex) backend
    std::env::set_var("OPENAI_API_KEY", "test-key");
    std::env::set_var("OPENAI_BASE_URL", &base_url);
    std::env::set_var("OPENAI_MODEL", "gpt-4o-mini");
    std::env::set_var("GITHUB_MCP_CMD", "python3 ../scripts/mock_mcp.py");
    std::env::set_var("AGENT_NEXUS_GITHUB_TOKEN", "ghp_test_token_for_e2e");
    std::env::set_var("LLM_PROVIDER", "openai");
    std::env::set_var("DEFAULT_CLI", "codex");

    // Remove Anthropic env vars to prevent fallback
    std::env::remove_var("ANTHROPIC_API_KEY");
    std::env::remove_var("ANTHROPIC_API_URL");

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

    // 4. Initialize SharedStore
    let store = SharedStore::new_in_memory();

    // Inject initial worker slots
    let slots = json!({
        "forge-1": { "id": "forge-1", "status": { "type": "idle" } }
    });
    store.set("worker_slots", slots).await;

    // 5. Run NexusNode
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

    // Cleanup env vars
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("OPENAI_BASE_URL");
    std::env::remove_var("OPENAI_MODEL");
    std::env::remove_var("GITHUB_MCP_CMD");
    std::env::remove_var("AGENT_NEXUS_GITHUB_TOKEN");
    std::env::remove_var("LLM_PROVIDER");
    std::env::remove_var("DEFAULT_CLI");

    Ok(())
}
