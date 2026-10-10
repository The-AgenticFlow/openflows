//! SENTINEL -> HTTP relay -> FORGE -> disposable Git checkout -> exact-head gate.
//! Only initial credentials are seeded; outcomes come from worker CLI commands.
use anyhow::{ensure, Context, Result};
use coder_client::{CoderClient, CreateWorkspaceRequest};
use serde_json::{json, Value};
use std::time::Duration;

async fn exec(client: &CoderClient, workspace: &str, script: &str) -> Result<String> {
    let output = client.workspace_exec(workspace, script).await?;
    ensure!(
        output.exit_code == 0,
        "command failed ({workspace}):\n{script}\n{}\n{}",
        output.stdout,
        output.stderr
    );
    Ok(output.stdout)
}

async fn status(client: &CoderClient, workspace: &str) -> Result<Value> {
    Ok(serde_json::from_str(
        &exec(client, workspace, "openflows-harness status get").await?,
    )?)
}

#[tokio::test]
#[ignore = "requires disposable containers; OPENFLOWS_E2E_SUITE=verification bash tests/e2e/run.sh"]
async fn failed_acceptance_cannot_pass_and_new_head_requires_new_verification() -> Result<()> {
    let url = std::env::var("OPENFLOWS_E2E_CODER_URL")?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    CoderClient::new(&url, "")
        .wait_for_healthy(Duration::from_secs(120))
        .await?;
    let credentials = json!({"email":"ci@example.test", "username":"ci",
        "password":"Disposable-CI-password-729!", "trial":false});
    http.post(format!("{url}/api/v2/users/first"))
        .json(&credentials)
        .send()
        .await?
        .error_for_status()
        .context("requires a fresh disposable Coder server")?;
    let login: Value = http
        .post(format!("{url}/api/v2/users/login"))
        .json(&credentials)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["session_token"]
        .as_str()
        .context("missing session token")?;
    let client = CoderClient::new(&url, token);
    client
        .push_template(
            "ci-worker",
            &std::fs::read(std::env::var("OPENFLOWS_E2E_TEMPLATE_ARCHIVE")?)?,
        )
        .await?;
    let mut workspaces = Vec::new();
    for role in ["forge", "sentinel"] {
        let ws = client
            .create_workspace(&CreateWorkspaceRequest {
                template_name: "ci-worker".into(),
                name: format!("ci-{role}"),
                parameters: json!({"tenant":"ci-verify", "role":role,
                "a2a_relay_addr":"a2a-relay:3000",
                "a2a_pair_token":"disposable-ci-pair-token-not-a-production-secret"}),
            })
            .await?;
        client
            .wait_for_workspace_ready(&ws.id, Duration::from_secs(180))
            .await?;
        client
            .clone()
            .with_workspace_name(&ws.name)
            .wait_for_workspace_ssh(&ws.id, Duration::from_secs(120))
            .await?;
        workspaces.push(ws);
    }
    let forge = client.clone().with_workspace_name("ci-forge");
    let sentinel = client.clone().with_workspace_name("ci-sentinel");
    let f = &workspaces[0].id;
    let s = &workspaces[1].id;
    exec(&forge, f, "set -euo pipefail\nopenflows-harness plan write --file /tmp/plan.md\nopenflows-harness status set plan_ready").await?;
    exec(&sentinel, s, "openflows-harness gate decide --phase plan_ready --revision 1 --round 1 --verdict approve --report /tmp/review.md").await?;
    // The acceptance oracle is a literal requirement (42), committed separately
    // from its deliberately broken input. Verification must actually execute it.
    let original_checkout = exec(&forge, f, "pwd -P").await?;
    let original_checkout = original_checkout.trim();
    exec(
        &forge,
        f,
        r#"set -euo pipefail
cd /home/coder/workspace
printf '41\n' > answer.txt
cat > acceptance.sh <<'ACCEPTANCE'
#!/bin/sh
set -eu
printf 'CHECKOUT_CWD=%s\n' "$(pwd -P)"
test "$(cat answer.txt)" = 42
printf 'ACCEPTANCE_OK\n'
ACCEPTANCE
chmod +x acceptance.sh
git add answer.txt acceptance.sh
git commit -m 'deliberately broken candidate'
openflows-harness status set testing
nohup openflows-harness verify serve >/tmp/verify.log 2>&1 </dev/null &"#,
    )
    .await?;
    let broken = status(&forge, f).await?;
    ensure!(broken["phase"] == "testing");
    exec(&sentinel, s, "set -euo pipefail\nif openflows-harness verify request --timeout-secs 30 --expect-exit 0 -- ./acceptance.sh > /tmp/failed.json 2>/tmp/failed.err; then exit 1; fi\ngrep 'expected exit 0' /tmp/failed.err").await?;
    let failed = status(&sentinel, s).await?;
    ensure!(
        failed["verified_head"].is_null() && failed["verification_task"].is_null(),
        "failed tests fabricated successful evidence"
    );
    exec(&sentinel, s, &format!("if openflows-harness gate decide --phase testing --revision 1 --round {} --head {} --verdict approve --report /tmp/review.md; then exit 1; fi", broken["review_round"], broken["head"].as_str().context("missing candidate head")?)).await?;
    exec(&forge, f, "set -euo pipefail\nopenflows-harness status set building\nprintf '42\\n' > answer.txt\ngit add answer.txt\ngit commit -m 'fix answer'\nopenflows-harness status set testing").await?;
    let fixed = status(&forge, f).await?;
    let head = fixed["head"].as_str().context("missing fixed head")?;
    ensure!(fixed["head"] != broken["head"] && fixed["verified_head"].is_null());
    exec(&sentinel, s, "openflows-harness verify request --timeout-secs 30 --expect-exit 0 -- ./acceptance.sh > /tmp/verified.json").await?;
    let evidence: Value =
        serde_json::from_str(&exec(&sentinel, s, "cat /tmp/verified.json").await?)?;
    ensure!(evidence["exit_code"] == 0 && evidence["head_sha"] == head);
    ensure!(
        evidence["stdout"]
            .as_str()
            .is_some_and(|v| v.contains("ACCEPTANCE_OK")),
        "acceptance command did not execute"
    );
    let verification_checkout = evidence["stdout"]
        .as_str()
        .context("verification stdout is missing")?
        .lines()
        .find_map(|line| line.strip_prefix("CHECKOUT_CWD="))
        .context("acceptance command did not report its working directory")?;
    let verification_checkout = std::path::Path::new(verification_checkout);
    ensure!(
        verification_checkout.is_absolute()
            && !verification_checkout.starts_with(original_checkout),
        "verification ran in FORGE's original checkout: {}",
        verification_checkout.display()
    );
    ensure!(
        evidence["executor"]["workspace"] == *f,
        "verification ran outside FORGE"
    );
    std::fs::write(
        std::path::Path::new(&std::env::var("OPENFLOWS_E2E_ARTIFACTS")?).join("verification.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    let stale_head = broken["head"].as_str().context("missing old head")?;
    // Change only one review identity component per attempt so each guard is
    // independently exercised instead of the round check masking the head check.
    exec(&sentinel, s, &format!(
        "set -euo pipefail\nif openflows-harness gate decide --phase testing --revision 1 --round {} --head {stale_head} --verdict approve --report /tmp/review.md > /tmp/stale-head.log 2>&1; then exit 1; fi\ngrep -F 'Review must identify the tested head' /tmp/stale-head.log",
        fixed["review_round"]
    )).await?;
    exec(&sentinel, s, &format!(
        "set -euo pipefail\nif openflows-harness gate decide --phase testing --revision 1 --round {} --head {head} --verdict approve --report /tmp/review.md > /tmp/stale-round.log 2>&1; then exit 1; fi\ngrep -F 'Review phase or round changed' /tmp/stale-round.log",
        broken["review_round"]
    )).await?;
    exec(&sentinel, s, &format!(
        "openflows-harness gate decide --phase testing --revision 1 --round {} --head {head} --verdict approve --report /tmp/review.md",
        fixed["review_round"]
    )).await?;
    exec(&forge, f, "openflows-harness status set submit").await?;
    let submitted = status(&forge, f).await?;
    ensure!(submitted["phase"] == "submit" && submitted["verified_head"] == head);
    exec(&forge, f, &format!("set -euo pipefail\ntest \"$(git rev-parse HEAD)\" = {head}\ntest -z \"$(git status --porcelain)\"\ntest \"$(cat answer.txt)\" = 42")).await?;
    // Workspace deletion removes the executor log before run.sh can collect it.
    // Preserve it over real Coder SSH while FORGE is still available.
    std::fs::write(
        std::path::Path::new(&std::env::var("OPENFLOWS_E2E_ARTIFACTS")?).join("forge-verify.log"),
        exec(&forge, f, "cat /tmp/verify.log").await?,
    )?;
    for ws in workspaces {
        client.delete_workspace(&ws.id).await?;
        client
            .wait_for_workspace_deleted(&ws.id, Duration::from_secs(120))
            .await?;
    }
    println!("PASS: two real workers, HTTP A2A execution, failing acceptance blocked, exact-head verification, stale approval rejected, submit, cleanup");
    Ok(())
}
