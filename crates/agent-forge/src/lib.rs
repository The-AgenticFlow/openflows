// crates/agent-forge/src/lib.rs
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use config::{
    state::{
        ACTION_EMPTY, ACTION_FAILED, ACTION_PR_OPENED, KEY_COMMAND_GATE, KEY_PENDING_PRS,
        KEY_TICKETS, KEY_WORKER_SLOTS,
    },
    Ticket, TicketStatus, WorkerSlot, WorkerStatus,
};
use pair_harness::{
    worktree::WorktreeManager, ForgeSentinelPair, PairConfig, PairOutcome, Ticket as PairTicket,
};
use pocketflow_core::{Action, BatchNode, SharedStore};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeStatus {
    /// Outcome status - can be "outcome" or "status" in STATUS.json
    #[serde(alias = "status")]
    pub outcome: String,
    /// Ticket ID - can be "ticket" or "ticket_id" in STATUS.json
    /// FORGE may omit this field; fall back to the known ticket_id.
    #[serde(alias = "ticket", default)]
    pub ticket_id: Option<String>,
    /// Branch name (optional - may not be present in all STATUS.json formats)
    #[serde(default)]
    pub branch: Option<String>,
    /// PR URL if a PR was opened
    #[serde(alias = "pr")]
    pub pr_url: Option<String>,
    /// PR number if a PR was opened
    pub pr_number: Option<u32>,
    /// Notes about the work done
    pub notes: Option<String>,
    /// Summary of changes (optional)
    pub summary: Option<String>,
    /// List of changes made (optional)
    #[serde(default)]
    pub changes: Option<Vec<String>>,
    /// List of commits made (optional)
    #[serde(default)]
    pub commits: Option<Vec<String>>,
    /// List of artifacts created (optional)
    #[serde(default)]
    pub artifacts: Option<Vec<String>>,
    /// Issue URL (optional)
    pub issue: Option<String>,
    /// Reason for failure or suspension (optional)
    pub reason: Option<String>,
}

pub struct ForgeNode {
    pub workspace_root: PathBuf,
    pub persona_path: PathBuf,
    pub github_token: String,
    pub registry_path: Option<PathBuf>,
}

impl ForgeNode {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        persona_path: impl Into<PathBuf>,
        github_token: &str,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            persona_path: persona_path.into(),
            github_token: github_token.to_string(),
            registry_path: None,
        }
    }

    /// Create with registry support for per-worker token resolution.
    pub fn new_with_registry(
        workspace_root: impl Into<PathBuf>,
        persona_path: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            persona_path: persona_path.into(),
            github_token: String::new(),
            registry_path: Some(registry_path.into()),
        }
    }

    /// Resolve GitHub token for a specific worker.
    fn resolve_token_for_worker(&self, worker_id: &str) -> Result<String> {
        if let Some(registry_path) = &self.registry_path {
            let registry = config::Registry::load(registry_path)?;
            registry.resolve_github_token(worker_id)
        } else {
            Ok(self.github_token.clone())
        }
    }

    async fn load_persona(&self) -> Result<String> {
        let content = tokio::fs::read_to_string(&self.persona_path)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to load forge persona from {:?}: {}",
                    self.persona_path,
                    e
                )
            })?;
        Ok(content)
    }
}

#[async_trait]
impl BatchNode for ForgeNode {
    fn name(&self) -> &str {
        "forge"
    }

    async fn prep_batch(&self, store: &SharedStore) -> Result<Vec<Value>> {
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let active_workers: Vec<Value> = slots
            .values()
            .filter(|s| {
                matches!(
                    s.status,
                    WorkerStatus::Assigned { .. } | WorkerStatus::Working { .. }
                )
            })
            .map(|s| json!(s))
            .collect();

        Ok(active_workers)
    }

    async fn exec_one(&self, item: Value) -> Result<Value> {
        let slot: WorkerSlot = serde_json::from_value(item)?;
        let worker_id = slot.id.clone();

        let (ticket_id, issue_url) = match &slot.status {
            WorkerStatus::Assigned {
                ticket_id,
                issue_url,
            } => (ticket_id.clone(), issue_url.clone()),
            WorkerStatus::Working {
                ticket_id,
                issue_url,
            } => (ticket_id.clone(), issue_url.clone()),
            _ => return Ok(json!({"outcome": "idle", "worker_id": worker_id})),
        };

        // Create worktree manager
        let worktree_mgr = WorktreeManager::new(&self.workspace_root);

        // Resolve token for this specific worker
        let worker_token = self.resolve_token_for_worker(&worker_id)?;

        // Create worktree for this worker
        let setup_result = worktree_mgr
            .create_worktree(&worker_id, &ticket_id, &worker_token)
            .await
            .map_err(|e| anyhow!("Failed to create worktree: {:#}", e))?;
        let worktree_path = setup_result.path;

        info!(worker = worker_id, ticket = ticket_id, path = ?worktree_path, "Worktree created");

        // Create log directory to persist logs even after worktree cleanup
        let log_dir = self
            .workspace_root
            .join("forge")
            .join("workers")
            .join(&worker_id);
        tokio::fs::create_dir_all(&log_dir).await?;

        let status_path = worktree_path.join("STATUS.json");
        let log_path = log_dir.join("worker.log");
        let log_file = std::fs::File::create(&log_path)?;
        let log_file_err = log_file.try_clone()?;

        info!(worker = worker_id, ticket = ticket_id, issue_url = ?issue_url, "Spawning Claude Code...");

        // Load the persona from the agent definition file (source of truth)
        let persona_content = self.load_persona().await?;

        // 1. Prepare command - build prompt from persona + task context
        let issue_context = if let Some(url) = &issue_url {
            format!("Issue URL: {}. Use your MCP tools (e.g. `get_issue` or `read_url`) to fetch the full description.", url)
        } else {
            "".to_string()
        };

        let branch_name = WorktreeManager::branch_name(&worker_id, &ticket_id);

        // Combine persona with task-specific context
        let prompt = format!(
            "{}\n\n---\n\n# Current Task\n\nYou are FORGE agent {} (worker slot).\nImplement ticket {}.\n{}\nBranch: {}.\nWhen done, open a PR and write STATUS.json.",
            persona_content, worker_id, ticket_id, issue_context, branch_name
        );

        // Resolve CLI backend from registry (respects DEFAULT_CLI env var)
        let cli_backend = if let Some(registry_path) = &self.registry_path {
            let registry = config::Registry::load(registry_path)?;
            registry.resolve_cli_backend(&worker_id)
        } else {
            std::env::var(config::DEFAULT_CLI_ENV_VAR)
                .ok()
                .map(|s| config::CliBackend::parse(&s))
                .unwrap_or_default()
        };

        let cli_binary = match cli_backend.path_env_var() {
            "CODEX_PATH" => std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string()),
            _ => std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string()),
        };

        match cli_backend {
            config::CliBackend::Codex => {
                let mut child = tokio::process::Command::new(&cli_binary)
                    .args(["exec", "--full-auto"])
                    .arg("-m")
                    .arg(std::env::var("OPENAI_MODEL")
                        .or_else(|_| std::env::var("FIREWORKS_MODEL"))
                        .unwrap_or_else(|_| "gpt-4o-mini".to_string()))
                    .current_dir(&worktree_path)
                    .env("OPENAI_API_KEY", std::env::var("OPENAI_API_KEY").unwrap_or_default())
                    .env("OPENAI_BASE_URL", std::env::var("OPENAI_BASE_URL").unwrap_or_default())
                    .stdin(std::process::Stdio::piped())
                    .stdout(log_file)
                    .stderr(log_file_err)
                    .spawn()
                    .map_err(|e| anyhow!("Failed to spawn Codex CLI: {:#}", e))?;

                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    stdin
                        .write_all(prompt.as_bytes())
                        .await
                        .map_err(|e| anyhow!("Failed to write prompt to stdin: {:#}", e))?;
                }

                let timeout_dur = std::time::Duration::from_secs(1800);
                let result = tokio::time::timeout(timeout_dur, child.wait()).await;

                match result {
                    Err(_) => {
                        warn!(worker = worker_id, "Codex CLI timed out after 30m");
                        return Ok(json!({
                            "worker_id": worker_id,
                            "ticket_id": ticket_id,
                            "outcome": "fuel_exhausted",
                            "reason": "timeout"
                        }));
                    }
                    Ok(Ok(status)) if !status.success() => {
                        warn!(worker = worker_id, exit = ?status.code(), "Codex CLI failed");
                    }
                    _ => {}
                }
            }
            config::CliBackend::Claude => {
                let mut child = tokio::process::Command::new(&cli_binary)
                    .args(["--print", "--output-format", "json"])
                    .arg("--dangerously-skip-permissions")
                    .args(["--allowedTools", "Read,Write,Edit,Bash,WebFetch"])
                    .current_dir(&worktree_path)
                    .env(
                        "ANTHROPIC_API_KEY",
                        std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
                    )
                    .stdin(std::process::Stdio::piped())
                    .stdout(log_file)
                    .stderr(log_file_err)
                    .spawn()
                    .map_err(|e| anyhow!("Failed to spawn Claude Code: {:#}", e))?;

                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    stdin
                        .write_all(prompt.as_bytes())
                        .await
                        .map_err(|e| anyhow!("Failed to write prompt to stdin: {:#}", e))?;
                }

                let timeout_dur = std::time::Duration::from_secs(1800);
                let result = tokio::time::timeout(timeout_dur, child.wait()).await;

                match result {
                    Err(_) => {
                        warn!(worker = worker_id, "Claude Code timed out after 30m");
                        return Ok(json!({
                            "worker_id": worker_id,
                            "ticket_id": ticket_id,
                            "outcome": "fuel_exhausted",
                            "reason": "timeout"
                        }));
                    }
                    Ok(Ok(status)) if !status.success() => {
                        warn!(worker = worker_id, exit = ?status.code(), "Claude Code failed");
                    }
                    _ => {}
                }
            }
        }

        // 3. Read STATUS.json
        if tokio::fs::try_exists(&status_path).await? {
            let content = tokio::fs::read_to_string(&status_path).await?;
            match serde_json::from_str::<ForgeStatus>(&content) {
                Ok(forge_status) => {
                    let outcome = match forge_status.outcome.as_str() {
                        "complete" | "completed" => "success",
                        other => other,
                    };

                    return Ok(json!({
                        "worker_id": worker_id,
                        "ticket_id": ticket_id,
                        "outcome": outcome,
                        "branch": forge_status.branch,
                        "pr_url": forge_status.pr_url,
                        "pr_number": forge_status.pr_number,
                        "notes": forge_status.notes,
                        "summary": forge_status.summary,
                        "commits": forge_status.commits,
                        "artifacts": forge_status.artifacts,
                        "reason": forge_status.reason,
                    }));
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse STATUS.json - treating as missing");
                }
            }
        }

        Ok(json!({
            "worker_id": worker_id,
            "ticket_id": ticket_id,
            "outcome": "failed",
            "reason": "STATUS.json not written"
        }))
    }

    async fn post_batch(&self, store: &SharedStore, results: Vec<Result<Value>>) -> Result<Action> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut command_gate: HashMap<String, Value> =
            store.get_typed(KEY_COMMAND_GATE).await.unwrap_or_default();

        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        let mut all_success = true;
        let worktree_mgr = WorktreeManager::new(&self.workspace_root);

        let mut ticket_updates: Vec<(String, TicketStatus)> = Vec::new();
        let mut opened_prs: Vec<Value> = Vec::new();

        for res_opt in &results {
            let res = match res_opt {
                Ok(v) => v,
                Err(e) => {
                    warn!("Batch item failed: {}", e);
                    all_success = false;
                    continue;
                }
            };
            let worker_id = res["worker_id"].as_str().unwrap_or("");
            let ticket_id = res["ticket_id"].as_str().unwrap_or("");
            let outcome = res["outcome"].as_str().unwrap_or("failed");

            if let Some(slot) = slots.get_mut(worker_id) {
                match outcome {
                    "success" | "pr_opened" => {
                        info!(
                            worker = worker_id,
                            ticket = ticket_id,
                            outcome,
                            "Work completed successfully"
                        );
                        slot.status = WorkerStatus::Done {
                            ticket_id: ticket_id.to_string(),
                            outcome: outcome.to_string(),
                        };
                        ticket_updates.push((
                            ticket_id.to_string(),
                            TicketStatus::Completed {
                                worker_id: worker_id.to_string(),
                                outcome: outcome.to_string(),
                            },
                        ));

                        let pr_number = res["pr_number"].as_u64().unwrap_or(0);
                        let branch = res["branch"].as_str().unwrap_or("");
                        if pr_number > 0 {
                            opened_prs.push(json!({
                                "number": pr_number,
                                "ticket_id": ticket_id,
                                "head_branch": branch,
                                "worker_id": worker_id,
                            }));
                        }

                        if let Err(e) =
                            worktree_mgr.remove_worktree_for_ticket(worker_id, ticket_id)
                        {
                            warn!(worker = worker_id, error = %e, "Failed to cleanup worktree");
                        } else {
                            info!(worker = worker_id, "Worktree cleaned up");
                        }
                    }
                    "suspended" | "blocked" => {
                        let reason = res["reason"].as_str().unwrap_or("unknown");
                        info!(
                            worker = worker_id,
                            ticket = ticket_id,
                            reason,
                            "Work suspended for approval"
                        );
                        slot.status = WorkerStatus::Suspended {
                            ticket_id: ticket_id.to_string(),
                            reason: reason.to_string(),
                            issue_url: res["issue_url"].as_str().map(|s| s.to_string()),
                        };
                        command_gate.insert(worker_id.to_string(), res.clone());
                    }
                    "idle" => {}
                    _ => {
                        warn!(
                            worker = worker_id,
                            ticket = ticket_id,
                            outcome,
                            "Work failed"
                        );
                        slot.status = WorkerStatus::Idle;
                        all_success = false;
                        let prev_attempts = tickets
                            .iter()
                            .find(|t| t.id == ticket_id)
                            .map(|t| t.attempts)
                            .unwrap_or(0)
                            + 1;
                        if prev_attempts >= Ticket::MAX_ATTEMPTS {
                            ticket_updates.push((
                                ticket_id.to_string(),
                                TicketStatus::Exhausted {
                                    worker_id: worker_id.to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        } else {
                            ticket_updates.push((
                                ticket_id.to_string(),
                                TicketStatus::Failed {
                                    worker_id: worker_id.to_string(),
                                    reason: outcome.to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        }
                        if let Err(e) =
                            worktree_mgr.remove_worktree_for_ticket(worker_id, ticket_id)
                        {
                            warn!(worker = worker_id, error = %e, "Failed to cleanup worktree");
                        } else {
                            info!(worker = worker_id, "Worktree cleaned up");
                        }
                    }
                }
            }
        }

        for (ticket_id, new_status) in ticket_updates {
            if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                if let TicketStatus::Failed { attempts, .. } = &new_status {
                    ticket.attempts = *attempts;
                } else if let TicketStatus::Exhausted { attempts, .. } = &new_status {
                    ticket.attempts = *attempts;
                }
                ticket.status = new_status;
            }
        }

        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        store.set(KEY_COMMAND_GATE, json!(command_gate)).await;
        store.set(KEY_TICKETS, json!(tickets)).await;

        let has_prs = !opened_prs.is_empty();
        if has_prs {
            let mut pending_prs: Vec<Value> =
                store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
            pending_prs.extend(opened_prs);
            store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
            info!("Updated pending_prs for VESSEL processing");
        }

        let has_suspended = slots
            .values()
            .any(|s| matches!(s.status, WorkerStatus::Suspended { .. }));

        if has_suspended {
            Ok(Action::new("suspended"))
        } else if (has_prs || all_success) && !results.is_empty() {
            Ok(Action::new(ACTION_PR_OPENED))
        } else if results.is_empty() {
            Ok(Action::new(ACTION_EMPTY))
        } else {
            Ok(Action::new(ACTION_FAILED))
        }
    }
}

/// ForgePairNode - integrates the full event-driven FORGE-SENTINEL lifecycle.
///
/// This node uses the ForgeSentinelPair from pair-harness to manage:
/// - FORGE as a long-running process
/// - SENTINEL spawned ephemeral for evaluations
/// - Event-driven lifecycle based on filesystem watches
/// - Automatic context resets via HANDOFF.md
///
/// Uses filesystem-based state by default (no Redis required).
pub struct ForgePairNode {
    pub workspace_root: PathBuf,
    pub github_token: String,
    pub registry_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    html_url: String,
}

impl ForgePairNode {
    /// Create a new ForgePairNode with filesystem-based state.
    pub fn new(workspace_root: impl Into<PathBuf>, github_token: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            github_token: github_token.into(),
            registry_path: None,
        }
    }

    /// Create with registry support for per-worker token resolution.
    pub fn new_with_registry(
        workspace_root: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            github_token: String::new(),
            registry_path: Some(registry_path.into()),
        }
    }

    /// Resolve GitHub token for a specific worker.
    fn resolve_token_for_worker(&self, worker_id: &str) -> Result<String> {
        if let Some(registry_path) = &self.registry_path {
            let registry = config::Registry::load(registry_path)?;
            registry.resolve_github_token(worker_id)
        } else {
            Ok(self.github_token.clone())
        }
    }

    fn parse_github_issue_url(issue_url: &str) -> Option<(String, String, u64)> {
        let trimmed = issue_url.trim_end_matches('/');
        let parts: Vec<_> = trimmed.split('/').collect();
        let issue_idx = parts.iter().position(|part| *part == "issues")?;
        if issue_idx < 2 || issue_idx + 1 >= parts.len() {
            return None;
        }

        let owner = parts.get(issue_idx - 2)?.to_string();
        let repo = parts.get(issue_idx - 1)?.to_string();
        let number = parts.get(issue_idx + 1)?.parse().ok()?;

        Some((owner, repo, number))
    }

    fn extract_acceptance_criteria(body: &str) -> Vec<String> {
        fn normalize_bullet(line: &str) -> Option<String> {
            let trimmed = line.trim();
            let stripped = trimmed
                .strip_prefix("- [ ] ")
                .or_else(|| trimmed.strip_prefix("- [x] "))
                .or_else(|| trimmed.strip_prefix("- "))
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("1. "))
                .or_else(|| trimmed.strip_prefix("2. "))
                .or_else(|| trimmed.strip_prefix("3. "))
                .or_else(|| trimmed.strip_prefix("4. "))
                .or_else(|| trimmed.strip_prefix("5. "))?;
            let value = stripped.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        }

        let mut in_acceptance_section = false;
        let mut criteria = Vec::new();

        for line in body.lines() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();

            if trimmed.starts_with('#') {
                in_acceptance_section = lower.contains("acceptance criteria");
                continue;
            }

            if in_acceptance_section {
                if let Some(item) = normalize_bullet(trimmed) {
                    criteria.push(item);
                    continue;
                }

                if !trimmed.is_empty() {
                    in_acceptance_section = false;
                }
            }
        }

        if criteria.is_empty() {
            for line in body.lines() {
                if let Some(item) = normalize_bullet(line) {
                    criteria.push(item);
                }
            }
        }

        criteria.dedup();
        criteria
    }

    async fn fetch_issue(&self, owner: &str, repo: &str, number: u64) -> Result<GithubIssue> {
        // Use registry token if available, otherwise use the fallback token
        let token = if let Some(registry_path) = &self.registry_path {
            config::Registry::load(registry_path)?
                .resolve_github_token("forge")
                .unwrap_or_else(|_| self.github_token.clone())
        } else {
            self.github_token.clone()
        };

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("agentflow/forge"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        let response = client
            .get(format!(
                "https://api.github.com/repos/{owner}/{repo}/issues/{number}"
            ))
            .send()
            .await?;

        let response = response.error_for_status()?;
        Ok(response.json::<GithubIssue>().await?)
    }

    async fn build_ticket(&self, ticket_id: &str, issue_url: Option<&str>) -> PairTicket {
        let mut ticket = PairTicket {
            id: ticket_id.to_string(),
            issue_number: 0,
            title: format!("Ticket {}", ticket_id),
            body: issue_url.unwrap_or_default().to_string(),
            url: issue_url.unwrap_or_default().to_string(),
            touched_files: vec![],
            acceptance_criteria: vec![],
        };

        let Some(issue_url) = issue_url else {
            return ticket;
        };

        if let Some((owner, repo, number)) = Self::parse_github_issue_url(issue_url) {
            ticket.issue_number = number;

            match self.fetch_issue(&owner, &repo, number).await {
                Ok(issue) => {
                    ticket.issue_number = issue.number;
                    ticket.title = issue.title;
                    ticket.body = issue.body;
                    ticket.url = issue.html_url;
                    ticket.acceptance_criteria = Self::extract_acceptance_criteria(&ticket.body);
                }
                Err(error) => {
                    warn!(
                        ticket = ticket_id,
                        issue_url,
                        error = %error,
                        "Failed to fetch GitHub issue details; falling back to minimal ticket"
                    );
                }
            }
        } else {
            warn!(
                ticket = ticket_id,
                issue_url, "Could not parse GitHub issue URL; falling back to minimal ticket"
            );
        }

        ticket
    }

    async fn check_existing_pr(
        &self,
        worker_id: &str,
        ticket_id: &str,
    ) -> Result<Option<(String, u64, String)>> {
        let repo_str = std::env::var("GITHUB_REPOSITORY").unwrap_or_default();
        let (owner, repo_name) = repo_str
            .split_once('/')
            .unwrap_or(("The-AgenticFlow", "template-counterapp"));

        let branch_name = WorktreeManager::branch_name(worker_id, ticket_id);

        // Resolve token for this worker
        let worker_token = self.resolve_token_for_worker(worker_id)?;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "https://api.github.com/repos/{}/{}/pulls?head={}:{}&state=open",
                owner, repo_name, owner, branch_name
            ))
            .header("Authorization", format!("Bearer {}", worker_token))
            .header("User-Agent", "agentflow-forge")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let prs: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();
        if let Some(pr) = prs.first() {
            let pr_url = pr["html_url"].as_str().unwrap_or_default().to_string();
            let pr_number = pr["number"].as_u64().unwrap_or(0);
            if pr_number > 0 {
                info!(
                    worker = worker_id,
                    pr_number,
                    branch = %branch_name,
                    "Found existing PR on GitHub for fuel-exhausted worker"
                );
                return Ok(Some((pr_url, pr_number, branch_name)));
            }
        }

        Ok(None)
    }

    async fn push_and_create_pr(
        &self,
        worker_id: &str,
        ticket_id: &str,
        ticket_title: &str,
        ticket_body: &str,
    ) -> Result<(String, u64, String)> {
        use anyhow::Context as _;
        use std::process::Command as StdCommand;

        let worktree_path = self.workspace_root.join("worktrees").join(worker_id);
        let branch_name = WorktreeManager::branch_name(worker_id, ticket_id);

        // Resolve token for this worker
        let worker_token = self.resolve_token_for_worker(worker_id)?;

        if !worktree_path.exists() {
            return Err(anyhow!(
                "Worktree does not exist at {}",
                worktree_path.display()
            ));
        }

        Self::scan_and_scrub_secrets(&worktree_path)?;

        // Detect the repository's default branch (e.g., "main" or "master")
        // instead of hardcoding "main" — repos may use "master" as default.
        let default_branch =
            WorktreeManager::detect_default_branch(&self.workspace_root);

        let has_changes = StdCommand::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree_path)
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);

        if has_changes {
            info!(
                worker = worker_id,
                "Committing uncommitted changes before push"
            );
            Self::git_add_safe(&worktree_path)?;

            StdCommand::new("git")
                .args([
                    "commit",
                    "-m",
                    &format!("{}: complete implementation", ticket_id),
                ])
                .current_dir(&worktree_path)
                .output()
                .context("Failed to git commit")?;
        }

        let has_commits = StdCommand::new("git")
            .args(["log", &format!("{}..HEAD", default_branch), "--oneline"])
            .current_dir(&worktree_path)
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);

        if !has_commits {
            return Err(anyhow!(
                "No commits on branch {} beyond {}",
                branch_name,
                default_branch
            ));
        }

        info!(worker = worker_id, branch = %branch_name, "Pushing branch to origin");
        let push_output = StdCommand::new("git")
            .args(["push", "-u", "origin", &branch_name])
            .current_dir(&worktree_path)
            .output()
            .context("Failed to push branch")?;

        if !push_output.status.success() {
            let stderr = String::from_utf8_lossy(&push_output.stderr);

            if stderr.contains("GH013")
                || stderr.contains("Push cannot contain secrets")
                || stderr.contains("secret-scanning")
            {
                info!(
                    worker = worker_id,
                    branch = %branch_name,
                    "Push rejected due to secret scanning — scrubbing secrets and rewriting history"
                );

                Self::scan_and_scrub_secrets(&worktree_path)?;
                Self::git_add_safe(&worktree_path)?;

                let has_fixup = StdCommand::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(&worktree_path)
                    .output()
                    .map(|o| !o.stdout.is_empty())
                    .unwrap_or(false);

                if has_fixup {
                    StdCommand::new("git")
                        .args([
                            "commit",
                            "-m",
                            &format!("{}: scrub secrets from tracked files", ticket_id),
                        ])
                        .current_dir(&worktree_path)
                        .output()
                        .context("Failed to commit secret scrub")?;
                }

                Self::rewrite_secret_commits(&worktree_path)?;

                let retry_push = StdCommand::new("git")
                    .args(["push", "-u", "origin", &branch_name])
                    .current_dir(&worktree_path)
                    .output()
                    .context("Failed to retry push after secret scrub")?;

                if !retry_push.status.success() {
                    let retry_stderr = String::from_utf8_lossy(&retry_push.stderr);
                    return Err(anyhow!(
                        "Push still rejected after secret scrub: {}",
                        retry_stderr
                    ));
                }
            } else if stderr.contains("non-fast-forward") || stderr.contains("fetch first") {
                info!(worker = worker_id, branch = %branch_name, "Normal push rejected — force-pushing with --force-with-lease");

                let _ = StdCommand::new("git")
                    .args(["fetch", "origin"])
                    .current_dir(&worktree_path)
                    .output();

                let force_push = StdCommand::new("git")
                    .args(["push", "-u", "origin", &branch_name, "--force-with-lease"])
                    .current_dir(&worktree_path)
                    .output()
                    .context("Failed to force-push branch")?;

                if !force_push.status.success() {
                    let force_stderr = String::from_utf8_lossy(&force_push.stderr);
                    if force_stderr.contains("GH013")
                        || force_stderr.contains("Push cannot contain secrets")
                    {
                        return Err(anyhow!("Force-push rejected by secret scanning — secrets remain in git history. Error: {}", force_stderr));
                    }
                    if force_stderr.contains("stale info") || force_stderr.contains("rejected") {
                        warn!(worker = worker_id, branch = %branch_name, "force-with-lease rejected — falling back to --force");
                        let bare_force = StdCommand::new("git")
                            .args(["push", "-u", "origin", &branch_name, "--force"])
                            .current_dir(&worktree_path)
                            .output()
                            .context("Failed to bare-force-push branch")?;
                        if bare_force.status.success() {
                            // push succeeded with bare --force — continue to PR creation
                        } else {
                            let bare_stderr = String::from_utf8_lossy(&bare_force.stderr);
                            return Err(anyhow!("Force-push failed: {}", bare_stderr));
                        }
                    } else {
                        return Err(anyhow!("Failed to force-push branch: {}", force_stderr));
                    }
                }
            } else if !stderr.contains("already exists")
                && !stderr.contains("up-to-date")
                && !stderr.contains("rejected")
            {
                return Err(anyhow!("Failed to push branch: {}", stderr));
            } else if stderr.contains("rejected")
                && !stderr.contains("non-fast-forward")
                && !stderr.contains("GH013")
            {
                return Err(anyhow!("Push rejected: {}", stderr));
            }
        }

        let repo_str = std::env::var("GITHUB_REPOSITORY").unwrap_or_default();
        let (owner, repo_name) = repo_str
            .split_once('/')
            .unwrap_or(("The-AgenticFlow", "template-counterapp"));

        let client = reqwest::Client::new();

        let existing_pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls?head={}:{}&state=open",
            owner, repo_name, owner, branch_name
        );
        let list_resp = client
            .get(&existing_pr_url)
            .header("Authorization", format!("Bearer {}", worker_token))
            .header("User-Agent", "agentflow-forge")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;

        if list_resp.status().is_success() {
            let prs: Vec<serde_json::Value> = list_resp.json().await.unwrap_or_default();
            if let Some(pr) = prs.first() {
                let pr_url = pr["html_url"].as_str().unwrap_or_default().to_string();
                let pr_number = pr["number"].as_u64().unwrap_or(0);
                if pr_number > 0 {
                    info!(
                        worker = worker_id,
                        pr_number,
                        branch = %branch_name,
                        "Found existing open PR for branch — updating instead of creating new"
                    );
                    return Ok((pr_url, pr_number, branch_name));
                }
            }
        }

        let pr_title = format!("[{}] {}", ticket_id, ticket_title);
        let pr_body = format!(
            "## {}\n\nResolves #{}\n\n---\n\n### Implementation\n\n{}",
            ticket_title,
            ticket_id.trim_start_matches("T-0").trim_start_matches('0'),
            if ticket_body.is_empty() {
                "See ticket for details.".to_string()
            } else {
                ticket_body.to_string()
            }
        );

        info!(owner, repo_name, branch = %branch_name, "Creating PR via GitHub API");
        let client = reqwest::Client::new();
        let pr_body_json = serde_json::json!({
            "title": pr_title,
            "body": pr_body,
            "head": branch_name,
            "base": default_branch
        });

        let resp = client
            .post(format!(
                "https://api.github.com/repos/{}/{}/pulls",
                owner, repo_name
            ))
            .header("Authorization", format!("Bearer {}", worker_token))
            .header("User-Agent", "agentflow-forge")
            .header("Accept", "application/vnd.github+json")
            .json(&pr_body_json)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if body.contains("already exists") {
                let list_resp = client
                    .get(format!(
                        "https://api.github.com/repos/{}/{}/pulls?head={}:{}&state=open",
                        owner, repo_name, owner, branch_name
                    ))
                    .header("Authorization", format!("Bearer {}", worker_token))
                    .header("User-Agent", "agentflow-forge")
                    .header("Accept", "application/vnd.github+json")
                    .send()
                    .await?;

                if list_resp.status().is_success() {
                    let prs: Vec<serde_json::Value> = list_resp.json().await.unwrap_or_default();
                    if let Some(pr) = prs.first() {
                        let pr_url = pr["html_url"].as_str().unwrap_or_default().to_string();
                        let pr_number = pr["number"].as_u64().unwrap_or(0);
                        return Ok((pr_url, pr_number, branch_name));
                    }
                }
                return Err(anyhow!("PR already exists but could not fetch its details"));
            }
            return Err(anyhow!("GitHub API returned {}: {}", status, body));
        }

        #[derive(Deserialize)]
        struct PrResponse {
            html_url: String,
            number: u64,
        }
        let pr: PrResponse = resp.json().await?;
        info!(pr_url = %pr.html_url, pr_number = pr.number, "PR created via GitHub API");
        Ok((pr.html_url, pr.number, branch_name))
    }

    fn scan_and_scrub_secrets(worktree_path: &std::path::Path) -> Result<()> {
        info!(
            path = %worktree_path.display(),
            "Scanning worktree for secrets before commit"
        );

        let token_env = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN"))
            .unwrap_or_default();

        let mut dirty_files: Vec<std::path::PathBuf> = Vec::new();
        Self::scan_dir_for_secrets(worktree_path, worktree_path, &token_env, &mut dirty_files)?;

        if !dirty_files.is_empty() {
            info!(
                count = dirty_files.len(),
                "Found and redacted secrets in files across worktree"
            );
        }

        Self::ensure_exclusions(worktree_path, &dirty_files)?;

        Ok(())
    }

    fn scan_dir_for_secrets(
        base: &std::path::Path,
        dir: &std::path::Path,
        token_env: &str,
        dirty_files: &mut Vec<std::path::PathBuf>,
    ) -> Result<()> {
        let skip_dirs = [
            ".git",
            "node_modules",
            "target",
            "__pycache__",
            ".next",
            "dist",
            "build",
        ];

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if skip_dirs.contains(&name) {
                            continue;
                        }
                    }
                    Self::scan_dir_for_secrets(base, &path, token_env, dirty_files)?;
                } else if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let is_text = matches!(
                        ext,
                        "json"
                            | "yaml"
                            | "yml"
                            | "toml"
                            | "env"
                            | "ini"
                            | "cfg"
                            | "md"
                            | "txt"
                            | "rs"
                            | "ts"
                            | "js"
                            | "py"
                            | "go"
                            | "rb"
                            | "sh"
                            | "bash"
                            | "zsh"
                            | "fish"
                            | "ps1"
                            | "bat"
                            | "xml"
                            | "html"
                            | "css"
                            | "scss"
                            | "less"
                            | "tf"
                            | "tfvars"
                            | "hcl"
                            | "properties"
                            | "conf"
                    ) || path.file_name().is_some_and(|n| {
                        let n = n.to_str().unwrap_or("");
                        n == ".env"
                            || n == ".env.local"
                            || n.starts_with(".env.")
                            || n == "credentials"
                            || n == "secrets"
                    });

                    if !is_text {
                        continue;
                    }

                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let mut modified = content.clone();
                        if !token_env.is_empty() {
                            modified = modified.replace(token_env, "${REDACTED_SECRET}");
                        }
                        modified = Self::redact_patterns(&modified);
                        if modified != content {
                            std::fs::write(&path, &modified)?;
                            let rel = path.strip_prefix(base).unwrap_or(&path);
                            info!(path = %rel.display(), "Redacted secrets from file");
                            dirty_files
                                .push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn ensure_exclusions(
        worktree_path: &std::path::Path,
        dirty_files: &[std::path::PathBuf],
    ) -> Result<()> {
        let mut entries_to_add: Vec<String> = Vec::new();

        for rel in dirty_files {
            if let Some(parent) = rel.parent() {
                let parent_str = parent.to_str().unwrap_or("");
                if !parent_str.is_empty() && !parent_str.starts_with("..") {
                    let dir_entry = format!("{}/", parent_str);
                    if !entries_to_add.contains(&dir_entry) {
                        entries_to_add.push(dir_entry);
                    }
                }
            }
        }

        let always_exclude = [".claude/", ".env.local"];
        for entry in always_exclude {
            if !entries_to_add.contains(&entry.to_string()) {
                entries_to_add.push(entry.to_string());
            }
        }

        if entries_to_add.is_empty() {
            return Ok(());
        }

        let gitignore_path = worktree_path.join(".gitignore");
        let existing = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
        let mut updated = existing.clone();

        for entry in entries_to_add {
            let entry_variants = [entry.as_str(), entry.trim_end_matches('/')];
            if !updated.lines().any(|l| {
                let trimmed = l.trim();
                entry_variants.contains(&trimmed)
            }) {
                if updated.is_empty() {
                    updated = format!("{}\n", entry);
                } else if updated.ends_with('\n') {
                    updated = format!("{}{}\n", updated, entry);
                } else {
                    updated = format!("{}\n{}\n", updated, entry);
                }
            }
        }

        if updated != existing {
            std::fs::write(&gitignore_path, updated)?;
        }

        Ok(())
    }

    fn redact_patterns(content: &str) -> String {
        let patterns = [
            (
                r#"GITHUB_PERSONAL_ACCESS_TOKEN":\s*"[^"]*""#,
                r#"GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_PERSONAL_ACCESS_TOKEN}""#,
            ),
            (r#"ghp_[A-Za-z0-9]{36}"#, r#"REDACTED_GITHUB_TOKEN"#),
            (r#"gho_[A-Za-z0-9]{36}"#, r#"REDACTED_GITHUB_OAUTH"#),
            (r#"ghu_[A-Za-z0-9]{36}"#, r#"REDACTED_GITHUB_USER"#),
            (r#"ghs_[A-Za-z0-9]{36}"#, r#"REDACTED_GITHUB_SRE"#),
            (
                r#"github_pat_[A-Za-z0-9_]{82}"#,
                r#"REDACTED_GITHUB_FINE_GRAINED_PAT"#,
            ),
            (
                r#"sk-[A-Za-z0-9]{20}T3[A-Za-z0-9]{3}"#,
                r#"REDACTED_OPENAI_KEY"#,
            ),
            (r#"AKIA[0-9A-Z]{16}"#, r#"REDACTED_AWS_ACCESS_KEY"#),
        ];

        let mut result = content.to_string();
        for (pattern, replacement) in patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                result = re.replace_all(&result, replacement).to_string();
            }
        }
        result
    }

    fn git_add_safe(worktree_path: &std::path::Path) -> Result<()> {
        use anyhow::Context as _;
        use std::process::Command as StdCommand;

        Self::untrack_secret_containing_files(worktree_path)?;

        StdCommand::new("git")
            .args(["add", "-A"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to git add")?;

        Ok(())
    }

    fn untrack_secret_containing_files(worktree_path: &std::path::Path) -> Result<()> {
        use std::process::Command as StdCommand;

        let tracked = StdCommand::new("git")
            .args(["ls-files"])
            .current_dir(worktree_path)
            .output();

        if let Ok(output) = tracked {
            if output.status.success() {
                let files = String::from_utf8_lossy(&output.stdout);
                for file in files.lines() {
                    let file_path = worktree_path.join(file);
                    if let Ok(content) = std::fs::read_to_string(&file_path) {
                        if Self::contains_secrets(&content) {
                            warn!(path = file, "Untracking file that contains secrets");
                            let _ = StdCommand::new("git")
                                .args(["rm", "--cached", file])
                                .current_dir(worktree_path)
                                .output();
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn contains_secrets(content: &str) -> bool {
        let secret_indicators = [
            r"ghp_[A-Za-z0-9]{36}",
            r"gho_[A-Za-z0-9]{36}",
            r"ghu_[A-Za-z0-9]{36}",
            r"ghs_[A-Za-z0-9]{36}",
            r"github_pat_[A-Za-z0-9_]{82}",
            r"sk-[A-Za-z0-9]{20}T3[A-Za-z0-9]{3}",
            r"AKIA[0-9A-Z]{16}",
        ];

        let token_env = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN"))
            .unwrap_or_default();

        if !token_env.is_empty() && content.contains(&token_env) {
            return true;
        }

        for pattern in secret_indicators {
            if let Ok(re) = regex::Regex::new(pattern) {
                if re.is_match(content) {
                    return true;
                }
            }
        }
        false
    }

    fn rewrite_secret_commits(worktree_path: &std::path::Path) -> Result<()> {
        use std::process::Command as StdCommand;

        info!(
            path = %worktree_path.display(),
            "Attempting to rewrite commits containing secrets via git filter-repo"
        );

        let secret_files = Self::list_secret_containing_tracked_files(worktree_path);

        if secret_files.is_empty() {
            info!(path = %worktree_path.display(), "No tracked files contain secrets — no rewrite needed");
            return Ok(());
        }

        let paths_arg = secret_files.join(" ");

        let filter = format!(
            r#"git rm --cached --ignore-unmatch {} 2>/dev/null; true"#,
            paths_arg
        );

        let output = StdCommand::new("git")
            .args([
                "filter-branch",
                "--force",
                "--index-filter",
                &filter,
                "--prune-empty",
                "--",
                "HEAD",
            ])
            .current_dir(worktree_path)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                info!(path = %worktree_path.display(), "Successfully rewrote commits to remove secret-containing files from tracking");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                if stderr.contains("no rewrite") || stderr.contains("nothing to rewrite") {
                    info!(path = %worktree_path.display(), "No rewrite needed — no secret-containing files in history");
                } else {
                    warn!(
                        path = %worktree_path.display(),
                        error = %stderr,
                        "git filter-branch produced warnings but may have succeeded"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to run git filter-branch — will try alternative approach");
            }
        }

        Ok(())
    }

    fn list_secret_containing_tracked_files(worktree_path: &std::path::Path) -> Vec<String> {
        use std::process::Command as StdCommand;

        let tracked = StdCommand::new("git")
            .args(["ls-files"])
            .current_dir(worktree_path)
            .output();

        let mut result = Vec::new();
        if let Ok(output) = tracked {
            if output.status.success() {
                let files = String::from_utf8_lossy(&output.stdout);
                for file in files.lines() {
                    let file_path = worktree_path.join(file);
                    if let Ok(content) = std::fs::read_to_string(&file_path) {
                        if Self::contains_secrets(&content) {
                            result.push(file.to_string());
                        }
                    }
                }
            }
        }
        result
    }
}

#[async_trait]
impl BatchNode for ForgePairNode {
    fn name(&self) -> &str {
        "forge_pair"
    }

    async fn prep_batch(&self, store: &SharedStore) -> Result<Vec<Value>> {
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let active_workers: Vec<Value> = slots
            .values()
            .filter(|s| {
                matches!(
                    s.status,
                    WorkerStatus::Assigned { .. } | WorkerStatus::Working { .. }
                )
            })
            .map(|s| json!(s))
            .collect();

        // Store the worker IDs we're about to process so we can handle failures
        let worker_ids: Vec<String> = active_workers
            .iter()
            .filter_map(|v| v["id"].as_str().map(|s| s.to_string()))
            .collect();
        store.set("_forge_batch_workers", json!(worker_ids)).await;

        Ok(active_workers)
    }

    async fn exec_one(&self, item: Value) -> Result<Value> {
        let slot: WorkerSlot = serde_json::from_value(item)?;
        let worker_id = slot.id.clone();

        let (ticket_id, issue_url) = match &slot.status {
            WorkerStatus::Assigned {
                ticket_id,
                issue_url,
            } => (ticket_id.clone(), issue_url.clone()),
            WorkerStatus::Working {
                ticket_id,
                issue_url,
            } => (ticket_id.clone(), issue_url.clone()),
            _ => return Ok(json!({"outcome": "idle", "worker_id": worker_id})),
        };

        info!(
            worker = worker_id,
            ticket = ticket_id,
            "Starting FORGE-SENTINEL pair lifecycle"
        );

        let ticket = self.build_ticket(&ticket_id, issue_url.as_deref()).await;

        // Resolve token for this specific worker
        let worker_token = self.resolve_token_for_worker(&worker_id)?;

        // Resolve CLI backend from registry (respects DEFAULT_CLI env var)
        let cli_backend = if let Some(registry_path) = &self.registry_path {
            let registry = config::Registry::load(registry_path)?;
            let base_id = worker_id
                .rfind('-')
                .map(|i| &worker_id[..i])
                .unwrap_or(&worker_id);
            info!(worker_id, base_id, default_cli = ?registry.default_cli, "Resolving CLI backend from registry");

            // Use the new resolve_cli_backend method which respects:
            // 1. Agent-specific `cli` field (highest priority)
            // 2. DEFAULT_CLI environment variable
            // 3. registry.json default_cli field
            // 4. Hardcoded "claude" fallback
            let backend = registry.resolve_cli_backend(&worker_id);
            info!(worker_id, base_id, ?backend, "CLI backend resolved");

            backend
        } else {
            // No registry - check DEFAULT_CLI env var, then fallback to default
            let backend = std::env::var(config::DEFAULT_CLI_ENV_VAR)
                .ok()
                .map(|s| config::CliBackend::parse(&s))
                .unwrap_or_default();
            info!(
                worker_id,
                ?backend,
                "No registry path, using CLI backend from env or default"
            );
            backend
        };

        let config = PairConfig::new(&worker_id, &ticket_id, &self.workspace_root, &worker_token)
            .with_cli_backend(cli_backend);

        let mut pair = ForgeSentinelPair::new(config);
        let outcome = pair
            .run(&ticket)
            .await
            .map_err(|e| anyhow!("Pair lifecycle failed: {:#}", e))?;

        match outcome {
            PairOutcome::PrOpened {
                pr_url,
                pr_number,
                branch,
            } => {
                info!(
                    worker = worker_id,
                    pr_url = %pr_url,
                    pr_number,
                    "Pair completed - PR opened"
                );
                Ok(json!({
                    "worker_id": worker_id,
                    "ticket_id": ticket_id,
                    "outcome": "pr_opened",
                    "pr_url": pr_url,
                    "pr_number": pr_number,
                    "branch": branch,
                }))
            }
            PairOutcome::Blocked { reason, blockers } => {
                if reason.contains("PR not created") || reason.contains("needs push/PR creation") {
                    info!(
                        worker = worker_id,
                        ticket = ticket_id,
                        "Work complete but no PR - attempting to push and create PR via GitHub API"
                    );
                    match self
                        .push_and_create_pr(&worker_id, &ticket_id, &ticket.title, &ticket.body)
                        .await
                    {
                        Ok((pr_url, pr_number, branch)) => {
                            info!(
                                worker = worker_id,
                                pr_url = %pr_url,
                                pr_number,
                                "PR created successfully via GitHub API"
                            );
                            return Ok(json!({
                                "worker_id": worker_id,
                                "ticket_id": ticket_id,
                                "outcome": "pr_opened",
                                "pr_url": pr_url,
                                "pr_number": pr_number,
                                "branch": branch,
                            }));
                        }
                        Err(e) => {
                            let error_detail = format!("{:#}", e);
                            let enriched_reason = if error_detail.contains("GH013")
                                || error_detail.contains("secret")
                            {
                                format!(
                                    "Push rejected: secrets detected in git history — {}",
                                    error_detail
                                )
                            } else {
                                format!("Push failed: {}", error_detail)
                            };
                            warn!(
                                worker = worker_id,
                                error = %enriched_reason,
                                "Failed to create PR via GitHub API - returning blocked with error detail"
                            );
                            return Ok(json!({
                                "worker_id": worker_id,
                                "ticket_id": ticket_id,
                                "outcome": "blocked",
                                "reason": enriched_reason,
                                "blockers": blockers,
                            }));
                        }
                    }
                }
                warn!(
                    worker = worker_id,
                    reason = %reason,
                    "Pair blocked - needs human intervention"
                );
                Ok(json!({
                    "worker_id": worker_id,
                    "ticket_id": ticket_id,
                    "outcome": "blocked",
                    "reason": reason,
                    "blockers": blockers,
                }))
            }
            PairOutcome::FuelExhausted {
                reason,
                reset_count,
            } => {
                warn!(
                    worker = worker_id,
                    reason = %reason,
                    resets = reset_count,
                    "Pair fuel exhausted"
                );
                Ok(json!({
                    "worker_id": worker_id,
                    "ticket_id": ticket_id,
                    "outcome": "fuel_exhausted",
                    "reason": reason,
                    "reset_count": reset_count,
                }))
            }
        }
    }

    async fn post_batch(&self, store: &SharedStore, results: Vec<Result<Value>>) -> Result<Action> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut command_gate: HashMap<String, Value> =
            store.get_typed(KEY_COMMAND_GATE).await.unwrap_or_default();

        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        let batch_workers: Vec<String> = store
            .get("_forge_batch_workers")
            .await
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let mut successful_workers: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut all_success = true;

        // Collect ticket status updates to apply
        let mut ticket_updates: Vec<(String, TicketStatus)> = Vec::new();

        // Collect PRs for VESSEL to process
        let mut opened_prs: Vec<Value> = Vec::new();

        for res_opt in &results {
            let res = match res_opt {
                Ok(v) => v,
                Err(e) => {
                    warn!("Batch item failed: {}", e);
                    all_success = false;
                    continue;
                }
            };
            let worker_id = res["worker_id"].as_str().unwrap_or("");
            let ticket_id = res["ticket_id"].as_str().unwrap_or("");
            let outcome = res["outcome"].as_str().unwrap_or("failed");

            if !worker_id.is_empty() {
                successful_workers.insert(worker_id.to_string());
            }

            if let Some(slot) = slots.get_mut(worker_id) {
                match outcome {
                    "pr_opened" => {
                        info!(
                            worker = worker_id,
                            ticket = ticket_id,
                            "Pair completed - PR opened"
                        );
                        slot.status = WorkerStatus::Done {
                            ticket_id: ticket_id.to_string(),
                            outcome: "pr_opened".to_string(),
                        };
                        ticket_updates.push((
                            ticket_id.to_string(),
                            TicketStatus::Completed {
                                worker_id: worker_id.to_string(),
                                outcome: "pr_opened".to_string(),
                            },
                        ));

                        // Add PR to pending_prs for VESSEL
                        let pr_number = res["pr_number"].as_u64().unwrap_or(0);
                        let branch = res["branch"].as_str().unwrap_or("");
                        if pr_number > 0 {
                            opened_prs.push(json!({
                                "number": pr_number,
                                "ticket_id": ticket_id,
                                "head_branch": branch,
                                "worker_id": worker_id,
                            }));
                            info!(pr_number, ticket_id, "Added PR to pending_prs for VESSEL");
                        }
                    }
                    "blocked" => {
                        let reason = res["reason"].as_str().unwrap_or("unknown");
                        info!(
                            worker = worker_id,
                            ticket = ticket_id,
                            reason,
                            "Pair blocked - needs intervention"
                        );
                        slot.status = WorkerStatus::Suspended {
                            ticket_id: ticket_id.to_string(),
                            reason: reason.to_string(),
                            issue_url: res["issue_url"].as_str().map(|s| s.to_string()),
                        };
                        command_gate.insert(worker_id.to_string(), res.clone());
                    }
                    "idle" => {}
                    "fuel_exhausted" => {
                        warn!(
                            worker = worker_id,
                            ticket = ticket_id,
                            "Pair fuel exhausted - checking for existing PR on GitHub"
                        );

                        match self.check_existing_pr(worker_id, ticket_id).await {
                            Ok(Some((pr_url, pr_number, branch))) => {
                                info!(
                                    worker = worker_id,
                                    pr_number,
                                    "PR already exists for fuel-exhausted worker - routing to VESSEL"
                                );
                                slot.status = WorkerStatus::Done {
                                    ticket_id: ticket_id.to_string(),
                                    outcome: "pr_opened".to_string(),
                                };
                                ticket_updates.push((
                                    ticket_id.to_string(),
                                    TicketStatus::Completed {
                                        worker_id: worker_id.to_string(),
                                        outcome: "pr_opened".to_string(),
                                    },
                                ));
                                opened_prs.push(json!({
                                    "number": pr_number,
                                    "ticket_id": ticket_id,
                                    "head_branch": branch,
                                    "worker_id": worker_id,
                                    "pr_url": pr_url,
                                }));
                            }
                            _ => {
                                slot.status = WorkerStatus::Idle;
                                all_success = false;
                                let prev_attempts = tickets
                                    .iter()
                                    .find(|t| t.id == ticket_id)
                                    .map(|t| t.attempts)
                                    .unwrap_or(0)
                                    + 1;
                                if prev_attempts >= Ticket::MAX_ATTEMPTS {
                                    ticket_updates.push((
                                        ticket_id.to_string(),
                                        TicketStatus::Exhausted {
                                            worker_id: worker_id.to_string(),
                                            attempts: prev_attempts,
                                        },
                                    ));
                                } else {
                                    ticket_updates.push((
                                        ticket_id.to_string(),
                                        TicketStatus::Failed {
                                            worker_id: worker_id.to_string(),
                                            reason: "fuel_exhausted".to_string(),
                                            attempts: prev_attempts,
                                        },
                                    ));
                                }
                            }
                        }
                    }
                    _ => {
                        warn!(
                            worker = worker_id,
                            ticket = ticket_id,
                            outcome,
                            "Pair failed"
                        );
                        slot.status = WorkerStatus::Idle;
                        all_success = false;
                        let prev_attempts = tickets
                            .iter()
                            .find(|t| t.id == ticket_id)
                            .map(|t| t.attempts)
                            .unwrap_or(0)
                            + 1;
                        if prev_attempts >= Ticket::MAX_ATTEMPTS {
                            ticket_updates.push((
                                ticket_id.to_string(),
                                TicketStatus::Exhausted {
                                    worker_id: worker_id.to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        } else {
                            ticket_updates.push((
                                ticket_id.to_string(),
                                TicketStatus::Failed {
                                    worker_id: worker_id.to_string(),
                                    reason: outcome.to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        }
                    }
                }
            }
        }

        for worker_id in &batch_workers {
            if !successful_workers.contains(worker_id) {
                if let Some(slot) = slots.get_mut(worker_id) {
                    let failed_ticket_id = match &slot.status {
                        WorkerStatus::Assigned { ticket_id, .. } => Some(ticket_id.clone()),
                        WorkerStatus::Working { ticket_id, .. } => Some(ticket_id.clone()),
                        _ => None,
                    };

                    warn!(
                        worker = worker_id,
                        "Resetting worker to Idle due to execution failure"
                    );
                    slot.status = WorkerStatus::Idle;

                    if let Some(ticket_id) = failed_ticket_id {
                        let prev_attempts = tickets
                            .iter()
                            .find(|t| t.id == ticket_id)
                            .map(|t| t.attempts)
                            .unwrap_or(0)
                            + 1;
                        if prev_attempts >= Ticket::MAX_ATTEMPTS {
                            ticket_updates.push((
                                ticket_id,
                                TicketStatus::Exhausted {
                                    worker_id: worker_id.to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        } else {
                            ticket_updates.push((
                                ticket_id,
                                TicketStatus::Failed {
                                    worker_id: worker_id.to_string(),
                                    reason: "spawn_failed".to_string(),
                                    attempts: prev_attempts,
                                },
                            ));
                        }
                    }
                }
            }
        }

        // Apply ticket status updates
        for (ticket_id, new_status) in ticket_updates {
            if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                if let TicketStatus::Failed { attempts, .. } = &new_status {
                    ticket.attempts = *attempts;
                } else if let TicketStatus::Exhausted { attempts, .. } = &new_status {
                    ticket.attempts = *attempts;
                }
                ticket.status = new_status;
                info!(
                    ticket = ticket.id,
                    status = ?ticket.status,
                    "Ticket status updated"
                );
            } else {
                warn!(
                    ticket_id,
                    "Ticket not found in store for status update - adding"
                );
                tickets.push(Ticket {
                    id: ticket_id.clone(),
                    title: String::new(),
                    body: String::new(),
                    priority: 0,
                    branch: None,
                    status: new_status,
                    issue_url: None,
                    attempts: 1,
                });
            }
        }

        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        store.set(KEY_COMMAND_GATE, json!(command_gate)).await;
        store.set(KEY_TICKETS, json!(tickets)).await;

        let has_prs = !opened_prs.is_empty();
        if has_prs {
            let mut pending_prs: Vec<Value> =
                store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
            pending_prs.extend(opened_prs);
            store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
            info!("Updated pending_prs for VESSEL processing");
        }

        let has_suspended = slots
            .values()
            .any(|s| matches!(s.status, WorkerStatus::Suspended { .. }));

        if has_suspended {
            Ok(Action::new("suspended"))
        } else if (has_prs || all_success) && !results.is_empty() {
            Ok(Action::new(ACTION_PR_OPENED))
        } else if results.is_empty() {
            Ok(Action::new(ACTION_EMPTY))
        } else {
            Ok(Action::new(ACTION_FAILED))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ForgePairNode;

    #[test]
    fn parse_github_issue_url_extracts_owner_repo_and_number() {
        let parsed = ForgePairNode::parse_github_issue_url(
            "https://github.com/The-AgenticFlow/template-counterapp/issues/4",
        )
        .unwrap();

        assert_eq!(parsed.0, "The-AgenticFlow");
        assert_eq!(parsed.1, "template-counterapp");
        assert_eq!(parsed.2, 4);
    }

    #[test]
    fn extract_acceptance_criteria_prefers_dedicated_section() {
        let body = r#"
# Counter UI Frontend

## Acceptance Criteria
- Render the current count value
- Increment and decrement controls update the count
- Styling matches the provided design

## Notes
- Mobile responsive
"#;

        let criteria = ForgePairNode::extract_acceptance_criteria(body);
        assert_eq!(
            criteria,
            vec![
                "Render the current count value",
                "Increment and decrement controls update the count",
                "Styling matches the provided design",
            ]
        );
    }

    #[test]
    fn extract_acceptance_criteria_falls_back_to_markdown_tasks() {
        let body = r#"
Implement the counter experience.

- [ ] Add increment action
- [ ] Add decrement action
- [ ] Add reset action
"#;

        let criteria = ForgePairNode::extract_acceptance_criteria(body);
        assert_eq!(
            criteria,
            vec![
                "Add increment action",
                "Add decrement action",
                "Add reset action",
            ]
        );
    }
}
