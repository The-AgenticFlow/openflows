use agent_forge::ForgeNode;
use anyhow::Result;
use config::{WorkerSlot, WorkerStatus, WorkspaceProvider};
use pocketflow_core::{BatchNode, SharedStore};
use serde_json::json;
use std::collections::HashMap;

#[tokio::test]
async fn test_forge_dangerous_command_suspends() -> Result<()> {
    // 1. Setup SharedStore
    let store = SharedStore::new_in_memory();

    // Inject a worker slot with "danger" in the ticket ID (our mock uses this)
    let worker_id = "forge-1";
    let ticket_id = "T-DANGER-001";
    let slots = HashMap::from([(
        worker_id.to_string(),
        WorkerSlot {
            id: worker_id.to_string(),
            status: WorkerStatus::Working {
                ticket_id: ticket_id.to_string(),
                issue_url: None,
            },
            workspace_id: None,
            workspace_provider: WorkspaceProvider::Local,
        },
    )]);
    store.set("worker_slots", json!(slots)).await;

    // 2. Setup ForgeNode with a mock claude
    // We'll point the PATH to our mock script
    let mut repo_root = std::env::current_dir()?;
    if repo_root.ends_with("crates/agent-forge") {
        repo_root = repo_root.parent().unwrap().parent().unwrap().to_path_buf();
    }
    let scripts_dir = repo_root.join("scripts");

    // Create a temporary PATH with our mock script as 'claude'
    let mock_claude_path = scripts_dir.join("mock_claude.py");
    // Make it executable
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&mock_claude_path)
        .spawn()?
        .wait()?;

    // We'll use a hack: set an env var CLAUDE_CMD for our node to use if we update it
    // Or just symlink it in a temp dir. Let's update ForgeNode to use an env var for the binary name.

    // Actually, I'll just use the mock script path directly in a wrapper if needed,
    // but for now let's assume we can mock it via PATH.
    let temp_dir = tempfile::tempdir()?;
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir)?;
    std::fs::copy(&mock_claude_path, bin_dir.join("claude"))?;

    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.display(), old_path);
    std::env::set_var("PATH", new_path);

    // 3. Run ForgeNode with persona path
    let persona_path = repo_root.join("orchestration/agent/agents/forge.agent.md");
    let forge = ForgeNode::new(&repo_root, &persona_path, "dummy-token");

    // Prep
    let items = forge.prep_batch(&store).await?;
    assert_eq!(items.len(), 1);

    // Exec
    let result = forge.exec_one(items[0].clone()).await?;
    assert_eq!(result["outcome"], "suspended");
    assert_eq!(result["reason"], "dangerous_command");

    // Post
    let action = forge.post_batch(&store, vec![Ok(result)]).await?;
    assert_eq!(action.as_str(), "suspended");

    // 4. Verify Store
    let final_slots: HashMap<String, WorkerSlot> = store
        .get_typed("worker_slots")
        .await
        .ok_or_else(|| anyhow::anyhow!("No worker_slots in store"))?;
    let slot = final_slots.get(worker_id).unwrap();
    assert!(matches!(slot.status, WorkerStatus::Suspended { .. }));

    // Cleanup PATH
    std::env::set_var("PATH", old_path);

    Ok(())
}
