//! Real Coder, Terraform provisioning, worker SSH and Redis. The runner owns
//! the disposable stack; this test deliberately never reads production tokens.
use anyhow::{ensure, Context, Result};
use coder_client::{CoderClient, CreateWorkspaceRequest};
use serde_json::json;
use std::time::Duration;

async fn command(client: &CoderClient, workspace: &str, script: &str) -> Result<String> {
    let output = client.workspace_exec(workspace, script).await?;
    ensure!(
        output.exit_code == 0,
        "workspace command failed: {}\n{}",
        output.stdout,
        output.stderr
    );
    Ok(output.stdout)
}

#[tokio::test]
#[ignore = "requires disposable Coder/Redis stack; run bash tests/e2e/run.sh"]
async fn real_worker_planning_gates_isolation_restart_and_cleanup() -> Result<()> {
    let url = std::env::var("OPENFLOWS_E2E_CODER_URL")?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let anonymous = CoderClient::new(&url, "");
    anonymous.wait_for_healthy(Duration::from_secs(120)).await?;
    // A first-user creation must succeed: pointing at an existing deployment
    // fails instead of silently borrowing an operator's account or token.
    let credentials = json!({
        "email": "ci@example.test", "username": "ci",
        "password": "Disposable-CI-password-729!", "trial": false
    });
    http.post(format!("{url}/api/v2/users/first"))
        .json(&credentials)
        .send()
        .await?
        .error_for_status()
        .context("first user creation failed; this must be a disposable server")?;
    let login: serde_json::Value = http
        .post(format!("{url}/api/v2/users/login"))
        .json(&credentials)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["session_token"]
        .as_str()
        .context("login did not return a session token")?;
    let client = CoderClient::new(&url, token);
    ensure!(client.get_me().await?.username == "ci");
    let archive = std::fs::read(std::env::var("OPENFLOWS_E2E_TEMPLATE_ARCHIVE")?)?;
    client.push_template("ci-worker", &archive).await?;
    let ws = client
        .create_workspace(&CreateWorkspaceRequest {
            template_name: "ci-worker".into(),
            name: "ci-forge".into(),
            parameters: json!({"tenant": "ci-alpha"}),
        })
        .await?;
    let worker = client.clone().with_workspace_name(&ws.name);
    worker
        .wait_for_workspace_ready(&ws.id, Duration::from_secs(180))
        .await?;
    worker
        .wait_for_workspace_ssh(&ws.id, Duration::from_secs(120))
        .await?;
    println!("Real Coder provisioned a connected worker; exercising harness over SSH");
    command(
        &worker,
        &ws.id,
        "set -euo pipefail\n\
         openflows-harness plan write --file /tmp/plan.md\n\
         openflows-harness status set plan_ready\n\
         if openflows-harness status set building; then exit 1; fi\n\
         if openflows-harness gate decide --phase plan_ready --verdict approve --revision 1 --round 1 --report /tmp/review.md; then exit 1; fi\n\
         OPENFLOWS_ROLE=sentinel openflows-harness gate decide --phase plan_ready --verdict reject --revision 1 --round 1 --report /tmp/review.md\n\
         if openflows-harness status set building; then exit 1; fi\n\
         openflows-harness status set planning\n\
         openflows-harness plan write --file /tmp/plan.md\n\
         openflows-harness status set plan_ready\n\
         if OPENFLOWS_ROLE=sentinel openflows-harness gate decide --phase plan_ready --verdict approve --revision 1 --round 1 --report /tmp/review.md; then exit 1; fi\n\
         OPENFLOWS_ROLE=sentinel openflows-harness gate decide --phase plan_ready --verdict approve --revision 2 --round 2 --report /tmp/review.md",
    )
    .await?;
    let status = command(&worker, &ws.id, "openflows-harness status get").await?;
    let state: serde_json::Value = serde_json::from_str(status.trim())?;
    ensure!(
        state["phase"] == "building",
        "approval must permit building"
    );
    let other = command(
        &worker,
        &ws.id,
        "OPENFLOWS_TENANT=ci-beta openflows-harness status get",
    )
    .await?;
    let other: serde_json::Value = serde_json::from_str(other.trim())?;
    ensure!(
        other["phase"] == "planning" && other["version"] == 0 && other["plan"] == "",
        "tenant state leaked"
    );

    // Restart through the actual Coder CLI, wait for the rebuild, and prove
    // the separately stored lifecycle survives worker-container replacement.
    for action in ["stop", "start"] {
        let output = tokio::process::Command::new("coder")
            .args([action, "ci-forge", "--yes"])
            .env("CODER_URL", &url)
            .env("CODER_SESSION_TOKEN", token)
            .kill_on_drop(true)
            .output()
            .await?;
        ensure!(
            output.status.success(),
            "Coder {action} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    worker
        .wait_for_workspace_ready(&ws.id, Duration::from_secs(180))
        .await?;
    worker
        .wait_for_workspace_ssh(&ws.id, Duration::from_secs(120))
        .await?;
    let resumed = command(&worker, &ws.id, "openflows-harness status get").await?;
    let resumed: serde_json::Value = serde_json::from_str(resumed.trim())?;
    ensure!(resumed == state, "worker restart changed lifecycle state");
    worker.delete_workspace(&ws.id).await?;
    worker
        .wait_for_workspace_deleted(&ws.id, Duration::from_secs(120))
        .await?;
    println!("PASS: real provisioning, SSH, plan rejection/approval, tenant separation, restart, deletion");
    Ok(())
}
