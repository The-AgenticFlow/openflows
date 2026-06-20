// crates/github/src/rest.rs
//
// Direct GitHub REST API client for operations that require low-latency
// or precise control (CI polling, merge execution).
//
// Separation of concerns: McpGithubClient handles high-level operations
// via MCP subprocess; this handles direct REST calls for VESSEL's needs.

use anyhow::{Context, Result};
use pocketflow_core::{CiStatus, MergeMethod, MergeResult, PrInfo, PrState};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

const GITHUB_API_BASE: &str = "https://api.github.com";

/// Direct GitHub REST API client for CI status polling and merge operations.
#[derive(Clone)]
pub struct GithubRestClient {
    client: reqwest::Client,
    token: String,
}

impl GithubRestClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("AgentFlow-VESSEL/0.1")
                .build()
                .expect("Failed to build reqwest client"),
            token: token.into(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn build_get(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    fn build_put(&self, url: &str, body: &[u8]) -> reqwest::RequestBuilder {
        self.client
            .put(url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Content-Type", "application/json")
            .body(body.to_vec())
    }

    fn build_patch(&self, url: &str, body: &[u8]) -> reqwest::RequestBuilder {
        self.client
            .patch(url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Content-Type", "application/json")
            .body(body.to_vec())
    }

    fn build_post(&self, url: &str, body: &[u8]) -> reqwest::RequestBuilder {
        self.client
            .post(url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Content-Type", "application/json")
            .body(body.to_vec())
    }

    async fn send_with_retry<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            match build().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_server_error() && attempt < MAX_RETRIES {
                        warn!(status = %status, attempt, "GitHub API server error, retrying");
                        let jitter = rand::thread_rng().gen_range(0..500);
                        let delay =
                            RETRY_BASE_DELAY * 2u32.pow(attempt) + Duration::from_millis(jitter);
                        sleep(delay).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let is_connect = e.is_connect() || e.is_timeout() || e.is_request();
                    if is_connect && attempt < MAX_RETRIES {
                        warn!(error = %e, attempt, "GitHub API network error, retrying");
                        let jitter = rand::thread_rng().gen_range(0..500);
                        let delay =
                            RETRY_BASE_DELAY * 2u32.pow(attempt) + Duration::from_millis(jitter);
                        sleep(delay).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e).context("GitHub API request failed after retries");
                }
            }
        }
        Err(last_err
            .map(|e| {
                anyhow::anyhow!(
                    "GitHub API request failed after {} retries: {}",
                    MAX_RETRIES,
                    e
                )
            })
            .unwrap_or_else(|| {
                anyhow::anyhow!("GitHub API request failed after {} retries", MAX_RETRIES)
            }))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        debug!(url, "GitHub API GET");
        let resp = self.send_with_retry(|| self.build_get(url)).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.json::<T>()
            .await
            .with_context(|| format!("Failed to parse GitHub response from {}", url))
    }

    /// Get raw JSON value from GitHub API. Used when response format may vary.
    async fn get_json_raw(&self, url: &str) -> Result<serde_json::Value> {
        debug!(url, "GitHub API GET (raw)");
        let resp = self.send_with_retry(|| self.build_get(url)).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.json::<serde_json::Value>()
            .await
            .with_context(|| format!("Failed to parse GitHub response from {}", url))
    }

    async fn get_text(&self, url: &str) -> Result<String> {
        debug!(url, "GitHub API GET (text)");
        let resp = self
            .send_with_retry(|| {
                self.client
                    .get(url)
                    .header("Authorization", self.auth_header())
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
            })
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.text()
            .await
            .with_context(|| format!("Failed to read GitHub text response from {}", url))
    }

    async fn put_json<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url, "GitHub API PUT");
        let payload = serde_json::to_vec(body)?;
        let resp = self
            .send_with_retry(|| self.build_put(url, &payload))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.json::<T>()
            .await
            .context("Failed to parse GitHub response")
    }

    async fn post_json<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url, "GitHub API POST");
        let payload = serde_json::to_vec(body)?;
        let resp = self
            .send_with_retry(|| self.build_post(url, &payload))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.json::<T>()
            .await
            .context("Failed to parse GitHub response")
    }

    async fn patch_json<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url, "GitHub API PATCH");
        let payload = serde_json::to_vec(body)?;
        let resp = self
            .send_with_retry(|| self.build_patch(url, &payload))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        resp.json::<T>()
            .await
            .context("Failed to parse GitHub response")
    }

    // ── CI Status Polling ────────────────────────────────────────────────

    /// Get combined CI status for a commit ref.
    /// Returns the aggregated status across all status contexts.
    pub async fn get_combined_status(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<CiStatus> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}/status",
            GITHUB_API_BASE, owner, repo, ref_sha
        );
        let resp: CombinedStatusResponse = self.get_json(&url).await?;
        Ok(map_status_state(&resp.state))
    }

    /// Get check suites for a commit ref.
    /// Returns the aggregated status across all check runs.
    pub async fn get_check_suites_status(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<CiStatus> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}/check-suites",
            GITHUB_API_BASE, owner, repo, ref_sha
        );
        let resp: CheckSuitesResponse = self.get_json(&url).await?;

        if resp.check_suites.is_empty() {
            return Ok(CiStatus::Success);
        }

        let mut has_pending = false;
        for suite in &resp.check_suites {
            match suite.status.as_str() {
                "queued" | "in_progress" | "pending" => has_pending = true,
                "completed"
                    if suite.conclusion.as_deref() == Some("failure")
                        || suite.conclusion.as_deref() == Some("timed_out")
                        || suite.conclusion.as_deref() == Some("cancelled") =>
                {
                    return Ok(CiStatus::Failure);
                }
                _ => {}
            }
        }

        if has_pending {
            Ok(CiStatus::Pending)
        } else {
            Ok(CiStatus::Success)
        }
    }

    /// Get the overall CI status (combines check suites and status API).
    /// Optimized: fetches both status types concurrently using tokio::join!
    pub async fn get_ci_status(&self, owner: &str, repo: &str, ref_sha: &str) -> Result<CiStatus> {
        // Parallelize CI status checks - reduces latency by ~50%
        let (combined_result, checks_result) = tokio::join!(
            self.get_combined_status(owner, repo, ref_sha),
            self.get_check_suites_status(owner, repo, ref_sha)
        );

        let combined = combined_result?;
        if combined.is_terminal() {
            return Ok(combined);
        }

        let checks = checks_result?;
        if checks.is_terminal() {
            return Ok(checks);
        }

        if combined == CiStatus::Pending || checks == CiStatus::Pending {
            Ok(CiStatus::Pending)
        } else {
            Ok(CiStatus::Success)
        }
    }

    /// Get detailed information about failed CI checks for a commit ref.
    /// Returns a human-readable summary of which checks failed, their conclusions,
    /// and the error details from the check output and annotations.
    pub async fn get_failed_checks_detail(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<String> {
        let detail = self
            .get_failed_checks_detail_structured(owner, repo, ref_sha)
            .await?;
        Ok(detail.to_string())
    }

    /// Structured version — returns `CiFailureDetail` so callers can
    /// generate targeted instructions (e.g., local reproduce commands).
    pub async fn get_failed_checks_detail_structured(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<CiFailureDetail> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}/check-runs",
            GITHUB_API_BASE, owner, repo, ref_sha
        );
        let resp: CheckRunsResponse = match self.get_json(&url).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "check-runs API failed — falling back to check-suites for failure detail");
                let fallback = self.get_failed_suites_detail(owner, repo, ref_sha).await?;
                return Ok(CiFailureDetail {
                    failed_checks: vec![FailedCheck {
                        name: fallback.clone(),
                        conclusion: "failure".to_string(),
                    }],
                    still_running: vec![],
                    job_logs: vec![],
                    annotations: vec![],
                });
            }
        };

        if resp.check_runs.is_empty() {
            return Ok(CiFailureDetail {
                failed_checks: vec![],
                still_running: vec![],
                job_logs: vec![],
                annotations: vec![],
            });
        }

        let mut failed_checks: Vec<FailedCheck> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut failed_run_ids: Vec<(String, u64)> = Vec::new();

        for run in &resp.check_runs {
            let name = run.name.as_deref().unwrap_or("unknown-check");
            let status = run.status.as_deref().unwrap_or("unknown");
            match status {
                "queued" | "in_progress" => {
                    pending.push(format!("{} (running)", name));
                }
                "completed" => {
                    if let Some(conclusion) = &run.conclusion {
                        match conclusion.as_str() {
                            "failure" | "timed_out" | "cancelled" | "action_required" => {
                                failed_checks.push(FailedCheck {
                                    name: name.to_string(),
                                    conclusion: conclusion.clone(),
                                });
                                if let Some(id) = run.id {
                                    failed_run_ids.push((name.to_string(), id));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let job_logs = match self.get_failed_job_logs(owner, repo, ref_sha).await {
            Ok(logs) if !logs.is_empty() => logs,
            Ok(_) => vec![],
            Err(e) => {
                debug!(error = %e, "Failed to fetch job logs — skipping");
                vec![]
            }
        };

        // Fetch annotations for each failed check run to provide exact
        // file:line error details to the CI fix agent.
        let mut annotations: Vec<CheckAnnotationDetail> = Vec::new();
        for (name, id) in &failed_run_ids {
            match self.get_check_annotations(owner, repo, *id).await {
                Ok(anns) => {
                    for ann in anns {
                        if let (Some(path), Some(start_line), Some(message)) =
                            (ann.path, ann.start_line, ann.message)
                        {
                            annotations.push(CheckAnnotationDetail {
                                check_name: name.clone(),
                                path,
                                start_line,
                                message,
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, check_run_id = id, "Failed to fetch annotations for check run");
                }
            }
        }

        Ok(CiFailureDetail {
            failed_checks,
            still_running: pending,
            job_logs,
            annotations,
        })
    }

    /// Get annotations for a specific check run.
    /// Annotations contain exact file:line references and error messages.
    pub async fn get_check_annotations(
        &self,
        owner: &str,
        repo: &str,
        check_run_id: u64,
    ) -> Result<Vec<CheckAnnotation>> {
        let url = format!(
            "{}/repos/{}/{}/check-runs/{}/annotations",
            GITHUB_API_BASE, owner, repo, check_run_id
        );
        let annotations: Vec<CheckAnnotation> = match self.get_json(&url).await {
            Ok(a) => a,
            Err(e) => {
                debug!(error = %e, check_run_id, "Failed to fetch check annotations — skipping");
                Vec::new()
            }
        };
        Ok(annotations)
    }

    /// Get failure detail from check-suites API as fallback.
    /// Less detailed than check-runs but works with broader token scopes.
    async fn get_failed_suites_detail(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<String> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}/check-suites",
            GITHUB_API_BASE, owner, repo, ref_sha
        );
        let resp: serde_json::Value = self.get_json_raw(&url).await?;

        let mut failed: Vec<String> = Vec::new();
        if let Some(suites) = resp["check_suites"].as_array() {
            for suite in suites {
                let status = suite["status"].as_str().unwrap_or("unknown");
                if status == "completed" {
                    let conclusion = suite["conclusion"].as_str().unwrap_or("");
                    match conclusion {
                        "failure" | "timed_out" | "cancelled" | "action_required" => {
                            let app_name = suite["app"]["name"].as_str().unwrap_or("unknown");
                            failed.push(format!("{} ({}) — {}", app_name, conclusion, status));
                        }
                        _ => {}
                    }
                }
            }
        }

        if failed.is_empty() {
            Ok("CI failed but could not retrieve detailed check names".to_string())
        } else {
            Ok(format!("Failed checks suites:\n{}", failed.join("\n")))
        }
    }

    // ── PR Operations ─────────────────────────────────────────────────────

    /// Get PR details including head SHA and state.
    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<PrInfo> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, owner, repo, pr_number
        );
        let resp: PullRequestResponse = self.get_json(&url).await?;

        Ok(PrInfo {
            number: resp.number,
            head_sha: resp.head.sha,
            ticket_id: extract_ticket_id(&resp.title, &resp.body, &resp.head.ref_field),
            head_branch: resp.head.ref_field,
            base_branch: resp.base.ref_field,
            title: resp.title,
            body: resp.body,
            state: match resp.state.as_str() {
                "open" => PrState::Open,
                "closed" if resp.merged.unwrap_or(false) => PrState::Merged,
                _ => PrState::Closed,
            },
            mergeable: resp.mergeable,
        })
    }

    /// Merge a pull request.
    pub async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        commit_title: &str,
        merge_method: MergeMethod,
    ) -> Result<MergeResult> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/merge",
            GITHUB_API_BASE, owner, repo, pr_number
        );

        let body = MergeRequestBody {
            commit_title: Some(commit_title.to_string()),
            merge_method,
        };

        let resp: MergeResponse = self.put_json(&url, &body).await?;

        Ok(MergeResult {
            merged: resp.merged,
            sha: resp.sha,
            message: resp.message,
        })
    }

    /// Close a pull request with an optional comment.
    /// Optimized: runs comment and close operations concurrently for reduced latency.
    pub async fn close_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        comment: Option<&str>,
    ) -> Result<()> {
        let close_url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, owner, repo, pr_number
        );
        let close_body = serde_json::json!({ "state": "closed" });

        // Run comment and close operations concurrently
        match comment {
            Some(text) => {
                let comment_url = format!(
                    "{}/repos/{}/{}/issues/{}/comments",
                    GITHUB_API_BASE, owner, repo, pr_number
                );
                let comment_body = serde_json::json!({ "body": text });

                let (comment_result, close_result) = tokio::join!(
                    self.post_json::<serde_json::Value, _>(&comment_url, &comment_body),
                    self.patch_json::<serde_json::Value, _>(&close_url, &close_body)
                );

                // Log comment errors but don't fail the close operation
                if let Err(e) = comment_result {
                    warn!(pr_number, error = %e, "Failed to add comment while closing PR");
                }

                close_result?;
            }
            None => {
                let _: serde_json::Value = self.patch_json(&close_url, &close_body).await?;
            }
        }

        info!(pr_number, owner, repo, "Closed pull request");
        Ok(())
    }

    /// Check if the repository has any GitHub Actions workflow files.
    /// Probes the `.github/workflows/` directory via the Contents API.
    /// Returns `true` if at least one workflow file exists, `false` otherwise.
    pub async fn has_workflows(&self, owner: &str, repo: &str) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/contents/.github/workflows",
            GITHUB_API_BASE, owner, repo
        );

        let resp = self.send_with_retry(|| self.build_get(&url)).await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(false);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body, "Failed to check workflows directory");
            return Ok(false);
        }

        let entries: Vec<ContentEntry> = resp
            .json()
            .await
            .context("Failed to parse contents response")?;
        let has_yml = entries
            .iter()
            .any(|e| e.name.ends_with(".yml") || e.name.ends_with(".yaml"));
        Ok(has_yml)
    }

    /// Check if a PR is already merged (for startup reconciliation).
    pub async fn is_pr_merged(&self, owner: &str, repo: &str, pr_number: u64) -> Result<bool> {
        match self.get_pull_request(owner, repo, pr_number).await {
            Ok(info) => Ok(info.state == PrState::Merged),
            Err(e) => {
                warn!(error = %e, pr = pr_number, "Failed to check PR merge status");
                Ok(false)
            }
        }
    }

    /// List open pull requests for a repository.
    pub async fn list_open_prs(&self, owner: &str, repo: &str) -> Result<Vec<PrInfo>> {
        let url = format!(
            "{}/repos/{}/{}/pulls?state=open&per_page=100",
            GITHUB_API_BASE, owner, repo
        );
        let resp: Vec<PullRequestResponse> = self.get_json(&url).await?;

        Ok(resp
            .into_iter()
            .map(|pr| PrInfo {
                number: pr.number,
                head_sha: pr.head.sha,
                ticket_id: extract_ticket_id(&pr.title, &pr.body, &pr.head.ref_field),
                head_branch: pr.head.ref_field,
                base_branch: pr.base.ref_field,
                title: pr.title,
                body: pr.body,
                state: PrState::Open,
                mergeable: pr.mergeable,
            })
            .collect())
    }

    /// Create a new pull request.
    /// Returns the PR number on success.
    pub async fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        head: &str,
        base: &str,
        body: Option<&str>,
    ) -> Result<u64> {
        let url = format!("{}/repos/{}/{}/pulls", GITHUB_API_BASE, owner, repo);

        let request_body = serde_json::json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body.unwrap_or(""),
        });

        let body_bytes = serde_json::to_vec(&request_body)?;
        let resp = self
            .send_with_retry(|| self.build_post(&url, &body_bytes))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub create PR error {}: {}", status, body);
        }

        let pr: PullRequestResponse = resp
            .json()
            .await
            .context("Failed to parse PR creation response")?;

        info!(pr_number = pr.number, "Created pull request");
        Ok(pr.number)
    }

    pub async fn list_open_issues(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitHubIssueResponse>> {
        let url = format!(
            "{}/repos/{}/{}/issues?state=open&per_page=100",
            GITHUB_API_BASE, owner, repo
        );
        self.get_json(&url).await
    }

    /// Assign a GitHub issue to a user.
    /// The assignee should be a GitHub username (e.g., "forge-bot").
    /// Returns Ok(()) on success, or an error with the HTTP status code on failure.
    pub async fn assign_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        assignee: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, owner, repo, issue_number
        );
        let body = serde_json::json!({ "assignees": [assignee] });

        debug!(url, assignee, "GitHub API PATCH issue assignment");
        let payload = serde_json::to_vec(&body)?;
        let resp = self
            .send_with_retry(|| self.build_patch(&url, &payload))
            .await?;

        let status = resp.status();
        if status.is_success() {
            let resp_json: serde_json::Value = resp.json().await?;
            let assignees = resp_json["assignees"].as_array();
            let assigned = assignees
                .map(|a| a.iter().any(|u| u["login"].as_str() == Some(assignee)))
                .unwrap_or(false);
            if assigned {
                info!(
                    issue = issue_number,
                    assignee, "GitHub issue assigned successfully"
                );
            } else {
                warn!(
                    issue = issue_number,
                    assignee,
                    "GitHub issue assignment may not have succeeded (assignee not in response)"
                );
            }
            Ok(())
        } else if status.as_u16() == 422 {
            // 422 Unprocessable Entity - typically means invalid assignee
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Validation failed (422): {}", body_text)
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body_text)
        }
    }

    /// Close a GitHub issue by setting its state to "closed".
    pub async fn close_issue(&self, owner: &str, repo: &str, issue_number: u64) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, owner, repo, issue_number
        );
        let body = serde_json::json!({ "state": "closed" });
        let resp: serde_json::Value = self.patch_json(&url, &body).await?;
        let state = resp["state"].as_str().unwrap_or("open");
        if state == "closed" {
            info!(issue = issue_number, "GitHub issue closed successfully");
            Ok(())
        } else {
            warn!(
                issue = issue_number,
                state, "GitHub issue close may not have succeeded"
            );
            Ok(())
        }
    }

    /// Update a PR branch with the latest changes from the base branch.
    /// Uses GitHub's built-in "Update branch" feature.
    /// Returns `Ok(())` if successful, `Err` if conflicts exist or API unavailable.
    pub async fn update_branch(&self, owner: &str, repo: &str, pr_number: u64) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/update-branch",
            GITHUB_API_BASE, owner, repo, pr_number
        );

        let resp = self.send_with_retry(|| self.build_put(&url, &[])).await?;
        let status = resp.status();
        if status.is_success() {
            debug!(pr = pr_number, "Branch updated successfully via GitHub API");
            Ok(())
        } else if status.as_u16() == 409 {
            anyhow::bail!("Merge conflict when updating branch for PR {}", pr_number)
        } else if status.as_u16() == 422 {
            anyhow::bail!(
                "Update branch not available for PR {} — may require admin access",
                pr_number
            )
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub update-branch error {}: {}", status, body)
        }
    }

    pub async fn list_conflicted_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<String>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/files",
            GITHUB_API_BASE, owner, repo, pr_number
        );

        let resp: Vec<PrFileResponse> = self.get_json(&url).await?;
        let conflicted: Vec<String> = resp
            .into_iter()
            .filter(|f| f.status == "modified" || f.status == "added" || f.status == "renamed")
            .map(|f| f.filename)
            .collect();

        Ok(conflicted)
    }

    // ── Actions Job Log Fetching ──────────────────────────────────────────

    /// Fetch job logs for failed workflow runs associated with a commit.
    /// Returns the last portion of each failed job's log, which typically
    /// contains the actual error output (e.g., ruff/lint/test failures).
    pub async fn get_failed_job_logs(
        &self,
        owner: &str,
        repo: &str,
        head_sha: &str,
    ) -> Result<Vec<(String, String)>> {
        let url = format!(
            "{}/repos/{}/{}/actions/runs?head_sha={}&status=failure&per_page=10",
            GITHUB_API_BASE, owner, repo, head_sha
        );

        let runs_resp: WorkflowRunsResponse = match self.get_json(&url).await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, "Failed to fetch workflow runs for job logs — skipping");
                return Ok(Vec::new());
            }
        };

        let mut result = Vec::new();

        for run in runs_resp.workflow_runs {
            let run_name = run.name.as_deref().unwrap_or("unknown");

            let jobs_url = format!(
                "{}/repos/{}/{}/actions/runs/{}/jobs?per_page=50",
                GITHUB_API_BASE, owner, repo, run.id
            );

            let jobs_resp: WorkflowJobsResponse = match self.get_json(&jobs_url).await {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = %e, run_id = run.id, "Failed to fetch jobs for workflow run — skipping");
                    continue;
                }
            };

            for job in jobs_resp.jobs {
                if job.conclusion.as_deref() != Some("failure") {
                    continue;
                }

                let job_name = job.name.as_deref().unwrap_or("unknown-job");

                let log_url = format!(
                    "{}/repos/{}/{}/actions/jobs/{}/logs",
                    GITHUB_API_BASE, owner, repo, job.id
                );

                match self.get_text(&log_url).await {
                    Ok(log_text) => {
                        let tail = tail_log(&log_text, 150);
                        result.push((format!("{}/{}", run_name, job_name), tail));
                    }
                    Err(e) => {
                        debug!(error = %e, job_id = job.id, "Failed to fetch job log — skipping");
                    }
                }
            }
        }

        Ok(result)
    }
}

// ── Helper Functions ──────────────────────────────────────────────────────

fn map_status_state(state: &str) -> CiStatus {
    match state.to_lowercase().as_str() {
        "pending" => CiStatus::Pending,
        "success" => CiStatus::Success,
        "failure" => CiStatus::Failure,
        "error" => CiStatus::Error,
        _ => CiStatus::Pending,
    }
}

fn extract_ticket_id(title: &str, body: &Option<String>, branch: &str) -> Option<String> {
    let patterns = [
        regex::Regex::new(r"T-(\d+)").ok(),
        regex::Regex::new(r"#(\d+)").ok(),
    ];

    for pattern in patterns.iter().flatten() {
        if let Some(caps) = pattern.captures(title) {
            return Some(format!("T-{}", &caps[1]));
        }
        if let Some(body) = body {
            if let Some(caps) = pattern.captures(body) {
                return Some(format!("T-{}", &caps[1]));
            }
        }
        if let Some(caps) = pattern.captures(branch) {
            return Some(format!("T-{}", &caps[1]));
        }
    }
    None
}

fn tail_log(log: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = log.lines().collect();
    if lines.len() <= max_lines {
        log.to_string()
    } else {
        let skip = lines.len() - max_lines;
        format!(
            "... (truncated {} lines)\n{}",
            skip,
            lines[skip..].join("\n")
        )
    }
}

// ── API Response Types ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CombinedStatusResponse {
    state: String,
}

#[derive(Deserialize)]
struct CheckSuitesResponse {
    check_suites: Vec<CheckSuite>,
}

#[derive(Deserialize)]
struct CheckSuite {
    status: String,
    conclusion: Option<String>,
}

/// A structured representation of a failed CI check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedCheck {
    pub name: String,
    pub conclusion: String,
}

/// Structured CI failure detail returned by `get_failed_checks_detail_structured`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiFailureDetail {
    pub failed_checks: Vec<FailedCheck>,
    pub still_running: Vec<String>,
    /// Raw job log excerpts for each failed job (job_name, last N lines)
    pub job_logs: Vec<(String, String)>,
    /// Annotations from failed check runs (file, line, message)
    pub annotations: Vec<CheckAnnotationDetail>,
}

/// A single annotation from a failed check run, containing the exact
/// file path, line number, and error message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckAnnotationDetail {
    pub check_name: String,
    pub path: String,
    pub start_line: u64,
    pub message: String,
}

impl CiFailureDetail {
    pub fn failed_check_names(&self) -> Vec<&str> {
        self.failed_checks
            .iter()
            .map(|c| c.name.as_str())
            .chain(self.job_logs.iter().map(|(n, _)| n.as_str()))
            .collect()
    }
}

impl fmt::Display for CiFailureDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.failed_checks.is_empty() {
            writeln!(f, "Failed checks:")?;
            for check in &self.failed_checks {
                writeln!(f, "  {} ({})", check.name, check.conclusion)?;
            }
        }
        if !self.still_running.is_empty() {
            writeln!(f, "\nStill running:")?;
            for name in &self.still_running {
                writeln!(f, "  {}", name)?;
            }
        }
        if !self.annotations.is_empty() {
            writeln!(f, "\nAnnotations (exact errors with file & line):")?;
            for ann in &self.annotations {
                writeln!(f, "  {}:{} {}", ann.path, ann.start_line, ann.message)?;
            }
        }
        if !self.job_logs.is_empty() {
            writeln!(f, "\nJob logs (last 150 lines per failed job):")?;
            for (i, (name, log)) in self.job_logs.iter().enumerate() {
                if i > 0 {
                    writeln!(f, "\n---\n")?;
                }
                writeln!(f, "Job: {}", name)?;
                write!(f, "{}", log)?;
            }
        }
        if self.failed_checks.is_empty()
            && self.still_running.is_empty()
            && self.job_logs.is_empty()
            && self.annotations.is_empty()
        {
            write!(f, "No check runs found for this commit")?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct CheckRunsResponse {
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CheckRun {
    id: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    conclusion: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    output: Option<CheckRunOutput>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct CheckRunOutput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CheckAnnotation {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub start_line: Option<u64>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Deserialize)]
struct PullRequestResponse {
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    merged: Option<bool>,
    mergeable: Option<bool>,
    head: PrBranch,
    base: PrBranch,
}

#[derive(Deserialize)]
struct PrBranch {
    sha: String,
    #[serde(rename = "ref")]
    ref_field: String,
}

#[derive(Serialize)]
struct MergeRequestBody {
    #[serde(rename = "commit_title")]
    commit_title: Option<String>,
    merge_method: MergeMethod,
}

#[derive(Deserialize)]
struct MergeResponse {
    merged: bool,
    sha: Option<String>,
    message: String,
}

#[derive(Deserialize)]
struct ContentEntry {
    name: String,
}

#[derive(Deserialize)]
struct PrFileResponse {
    filename: String,
    status: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubIssueResponse {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct WorkflowRunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    status: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct WorkflowJobsResponse {
    jobs: Vec<WorkflowJob>,
}

#[derive(Deserialize)]
struct WorkflowJob {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}
