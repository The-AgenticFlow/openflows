// crates/github/src/rest.rs
//
// Direct GitHub REST API client for operations that require low-latency
// or precise control (CI polling, merge execution).
//
// Separation of concerns: McpGithubClient handles high-level operations
// via MCP subprocess; this handles direct REST calls for VESSEL's needs.

use anyhow::{Context, Result};
use config::Envconfig;
use pocketflow_core::{CiStatus, MergeMethod, MergeResult, PrInfo, PrState};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// Direct GitHub REST API client for CI status polling and merge operations.
#[derive(Clone)]
pub struct GithubRestClient {
    client: reqwest::Client,
    token: String,
    api_base: String,
}

impl GithubRestClient {
    pub fn new(token: impl Into<String>) -> Self {
        // The GitHub REST API base is configured via `GITHUB_API_BASE`, whose
        // default (`https://api.github.com`) lives in `config::GithubConfig`.
        let api_base = config::GithubConfig::init_from_env()
            .map(|cfg| cfg.api_base)
            .unwrap_or_else(|_| "https://api.github.com".to_string());
        Self::with_api_base(token, api_base)
    }

    /// Construct a client for an explicit GitHub or GitHub Enterprise API endpoint.
    pub fn with_api_base(token: impl Into<String>, api_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("AgentFlow-VESSEL/0.1")
                .build()
                .expect("Failed to build reqwest client"),
            token: token.into(),
            api_base: api_base.into(),
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
            self.api_base, owner, repo, ref_sha
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
            self.api_base, owner, repo, ref_sha
        );
        let resp: CheckSuitesResponse = self.get_json(&url).await?;

        // Ignore ghost check suites from unconfigured/non-CI apps that have 0 check runs.
        let active_suites: Vec<&CheckSuite> = resp
            .check_suites
            .iter()
            .filter(|s| !is_ghost_check_suite(s))
            .collect();

        if active_suites.is_empty() {
            return Ok(CiStatus::Success);
        }

        let mut has_pending = false;
        for suite in active_suites {
            match suite.status.as_str() {
                "queued" | "in_progress" | "pending" => has_pending = true,
                "completed"
                    if suite.conclusion.as_deref() == Some("failure")
                        || suite.conclusion.as_deref() == Some("timed_out")
                        || suite.conclusion.as_deref() == Some("cancelled")
                        || suite.conclusion.as_deref() == Some("action_required")
                        || suite.conclusion.as_deref() == Some("startup_failure") =>
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
        let status_url = format!(
            "{}/repos/{}/{}/commits/{}/status",
            self.api_base, owner, repo, ref_sha
        );
        let combined = match self.get_json::<CombinedStatusResponse>(&status_url).await {
            Ok(response) if response.total_count > 0 => Some(map_status_state(&response.state)),
            Ok(_) => None,
            Err(e) if is_status_api_forbidden(&e) => None,
            Err(e) => return Err(e),
        };
        let mut suites = Vec::new();
        let mut page = 1;
        loop {
            let url = format!(
                "{}/repos/{}/{}/commits/{}/check-suites?per_page=100&page={}",
                self.api_base, owner, repo, ref_sha, page
            );
            let response: CheckSuitesResponse = self.get_json(&url).await?;
            let len = response.check_suites.len();
            suites.extend(response.check_suites);
            if len < 100 {
                break;
            }
            page += 1;
        }
        // Ignore ghost check suites from unconfigured/non-CI apps that have 0 check runs.
        let active_suites: Vec<CheckSuite> = suites
            .into_iter()
            .filter(|s| !is_ghost_check_suite(s))
            .collect();

        let checks = if active_suites.is_empty() {
            None
        } else {
            let mut status = CiStatus::Success;
            for suite in active_suites {
                let observed = if suite.status != "completed" {
                    CiStatus::Pending
                } else {
                    match suite.conclusion.as_deref() {
                        Some("success" | "neutral" | "skipped") => CiStatus::Success,
                        Some(
                            "failure" | "timed_out" | "cancelled" | "action_required"
                            | "startup_failure",
                        ) => CiStatus::Failure,
                        _ => CiStatus::Pending,
                    }
                };
                status = aggregate_ci_sources(Some(status), Some(observed));
            }
            Some(status)
        };
        Ok(aggregate_ci_sources(combined, checks))
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
            self.api_base, owner, repo, ref_sha
        );
        let resp: CheckRunsResponse = match self.get_json(&url).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "check-runs API failed — falling back to check-suites for failure detail");
                let failed_suites = self
                    .get_failed_suites(owner, repo, ref_sha)
                    .await
                    .unwrap_or_default();
                let failed_checks = if !failed_suites.is_empty() {
                    failed_suites
                } else {
                    vec![FailedCheck {
                        name: "CI failed but could not retrieve detailed check names".to_string(),
                        conclusion: "failure".to_string(),
                    }]
                };
                return Ok(CiFailureDetail {
                    failed_checks,
                    still_running: vec![],
                    job_logs: vec![],
                    annotations: vec![],
                });
            }
        };

        if resp.check_runs.is_empty() {
            let failed_suites = self
                .get_failed_suites(owner, repo, ref_sha)
                .await
                .unwrap_or_default();
            return Ok(CiFailureDetail {
                failed_checks: failed_suites,
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
                            "failure" | "timed_out" | "cancelled" | "action_required"
                            | "startup_failure" => {
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

        if failed_checks.is_empty() {
            if let Ok(failed_suites) = self.get_failed_suites(owner, repo, ref_sha).await {
                failed_checks.extend(failed_suites);
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
            self.api_base, owner, repo, check_run_id
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

    /// Get structured failure detail from check-suites API.
    async fn get_failed_suites(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<Vec<FailedCheck>> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}/check-suites",
            self.api_base, owner, repo, ref_sha
        );
        let resp: serde_json::Value = self.get_json_raw(&url).await?;

        let mut failed: Vec<FailedCheck> = Vec::new();
        if let Some(suites) = resp["check_suites"].as_array() {
            for suite in suites {
                let status = suite["status"].as_str().unwrap_or("unknown");
                if status == "completed" {
                    let conclusion = suite["conclusion"].as_str().unwrap_or("");
                    match conclusion {
                        "failure" | "timed_out" | "cancelled" | "action_required"
                        | "startup_failure" => {
                            let app_name = suite["app"]["name"].as_str().unwrap_or("unknown");
                            failed.push(FailedCheck {
                                name: app_name.to_string(),
                                conclusion: conclusion.to_string(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(failed)
    }

    /// Get failure detail from check-suites API as fallback.
    /// Less detailed than check-runs but works with broader token scopes.
    pub async fn get_failed_suites_detail(
        &self,
        owner: &str,
        repo: &str,
        ref_sha: &str,
    ) -> Result<String> {
        let failed = self.get_failed_suites(owner, repo, ref_sha).await?;
        if failed.is_empty() {
            Ok("CI failed but could not retrieve detailed check names".to_string())
        } else {
            let lines: Vec<String> = failed
                .into_iter()
                .map(|f| format!("{} ({}) — completed", f.name, f.conclusion))
                .collect();
            Ok(format!("Failed checks suites:\n{}", lines.join("\n")))
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
            self.api_base, owner, repo, pr_number
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
    /// Confirm merge using GitHub's merged flag and actual merge commit SHA.
    pub async fn confirmed_merge_sha(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Option<String>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.api_base, owner, repo, number
        );
        let value: serde_json::Value = self.get_json(&url).await?;
        if value["merged"].as_bool() != Some(true) {
            return Ok(None);
        }
        Ok(Some(
            value["merge_commit_sha"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Merged PR has no merge commit SHA"))?
                .to_owned(),
        ))
    }

    pub async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        commit_title: &str,
        merge_method: MergeMethod,
        expected_sha: &str,
    ) -> Result<MergeResult> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/merge",
            self.api_base, owner, repo, pr_number
        );

        let body = MergeRequestBody {
            sha: expected_sha.to_owned(),
            commit_title: Some(commit_title.to_string()),
            merge_method,
        };

        // A merge is not safely retryable: after a timeout or server error, a
        // later rejection cannot prove that the first attempt did not merge.
        let payload = serde_json::to_vec(&body)?;
        let response = self.build_put(&url, &payload).send().await?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            // Only known rejection statuses prove that this attempt did not merge.
            // Timeout-like gateway statuses (408/499) and unfamiliar responses stay unknown.
            if matches!(
                status.as_u16(),
                400 | 401 | 403 | 404 | 405 | 409 | 422 | 429
            ) {
                return Ok(MergeResult {
                    merged: false,
                    sha: None,
                    message: format!("GitHub rejected merge ({status}): {message}"),
                });
            }
            anyhow::bail!("GitHub merge outcome unknown ({status}): {message}");
        }
        let resp: MergeResponse = response
            .json()
            .await
            .context("Failed to parse merge response")?;

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
            self.api_base, owner, repo, pr_number
        );
        let close_body = serde_json::json!({ "state": "closed" });

        // Run comment and close operations concurrently
        match comment {
            Some(text) => {
                let comment_url = format!(
                    "{}/repos/{}/{}/issues/{}/comments",
                    self.api_base, owner, repo, pr_number
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
        const OTHER_CI_CONFIGS: &[&str] = &[
            ".circleci/config.yml",
            ".circleci/config.yaml",
            ".gitlab-ci.yml",
            ".gitlab-ci.yaml",
            "Jenkinsfile",
            "azure-pipelines.yml",
            "azure-pipelines.yaml",
            "bitbucket-pipelines.yml",
            "bitbucket-pipelines.yaml",
            ".buildkite/pipeline.yml",
            ".buildkite/pipeline.yaml",
        ];

        let url = format!(
            "{}/repos/{}/{}/contents/.github/workflows",
            self.api_base, owner, repo
        );

        let resp = self.send_with_retry(|| self.build_get(&url)).await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            for path in OTHER_CI_CONFIGS {
                if self.content_path_exists(owner, repo, path).await? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body, "Failed to check workflows directory");
            anyhow::bail!("Failed to check workflows directory: {}", status);
        }

        let entries: Vec<ContentEntry> = resp
            .json()
            .await
            .context("Failed to parse contents response")?;
        let has_yml = entries
            .iter()
            .any(|e| e.name.ends_with(".yml") || e.name.ends_with(".yaml"));
        if has_yml {
            return Ok(true);
        }

        for path in OTHER_CI_CONFIGS {
            if self.content_path_exists(owner, repo, path).await? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn content_path_exists(&self, owner: &str, repo: &str, path: &str) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_base, owner, repo, path
        );
        let resp = self.send_with_retry(|| self.build_get(&url)).await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(false);
        }
        if status.is_success() {
            return Ok(true);
        }
        let body = resp.text().await.unwrap_or_default();
        warn!(status = %status, body, path, "Failed to check CI config path");
        anyhow::bail!("Failed to check CI config path {}: {}", path, status)
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
            self.api_base, owner, repo
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
        let url = format!("{}/repos/{}/{}/pulls", self.api_base, owner, repo);

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
            self.api_base, owner, repo
        );
        self.get_json(&url).await
    }

    /// Get the authenticated user's login name using the current token.
    /// Calls GET /user and returns the "login" field.
    pub async fn get_authenticated_user_login(&self) -> Result<String> {
        let url = format!("{}/user", self.api_base);
        let resp = self.send_with_retry(|| self.build_get(&url)).await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to get authenticated user (status {}): {}",
                status,
                body_text
            );
        }

        let user: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse GitHub user response")?;

        let login = user["login"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("GitHub user response missing 'login' field"))?;

        Ok(login.to_string())
    }

    /// Check whether a diagnostic comment with the given marker tag already exists on an issue.
    /// The marker is an HTML comment like `<!-- openflows-assignment-failure -->` embedded in the body.
    pub async fn issue_has_comment_with_marker(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        marker: &str,
    ) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}/comments?per_page=100",
            self.api_base, owner, repo, issue_number
        );
        let comments: Vec<serde_json::Value> = self.get_json(&url).await?;
        Ok(comments.iter().any(|c| {
            c["body"]
                .as_str()
                .map(|b| b.contains(marker))
                .unwrap_or(false)
        }))
    }

    /// Post a comment on a GitHub issue.
    pub async fn comment_on_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        comment_body: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}/comments",
            self.api_base, owner, repo, issue_number
        );
        let payload = serde_json::json!({ "body": comment_body });
        let body_bytes = serde_json::to_vec(&payload)?;

        let resp = self
            .send_with_retry(|| self.build_post(&url, &body_bytes))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to post comment on issue #{} (status {}): {}",
                issue_number,
                status,
                body_text
            );
        }

        info!(
            owner,
            repo,
            issue = issue_number,
            "Comment posted on GitHub issue"
        );
        Ok(())
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
            self.api_base, owner, repo, issue_number
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
            self.api_base, owner, repo, issue_number
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
            self.api_base, owner, repo, pr_number
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
            self.api_base, owner, repo, pr_number
        );

        let resp: Vec<PrFileResponse> = self.get_json(&url).await?;
        let conflicted: Vec<String> = resp
            .into_iter()
            .filter(|f| f.status == "modified" || f.status == "added" || f.status == "renamed")
            .map(|f| f.filename)
            .collect();

        Ok(conflicted)
    }

    // ── PR Review & Comment API ──────────────────────────────────────────

    /// List the reviews submitted on a pull request.
    ///
    /// GitHub returns *all* reviews for a PR, including superseded ones (a
    /// reviewer may Approve and later request changes). Use
    /// [`effective_review_state`] to compute the current state from this list.
    pub async fn list_pr_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PrReview>> {
        // Fetch every page: a verdict-changing review (e.g. a later
        // CHANGES_REQUESTED) must never be missed because it fell past page 1.
        let mut reviews = Vec::new();
        let mut page = 1;
        loop {
            let url = format!(
                "{}/repos/{}/{}/pulls/{}/reviews?per_page=100&page={}",
                self.api_base, owner, repo, pr_number, page
            );
            let batch: Vec<PrReview> = self.get_json(&url).await?;
            let len = batch.len();
            reviews.extend(batch);
            if len < 100 {
                break;
            }
            page += 1;
        }
        Ok(reviews)
    }

    /// List the inline review comments on a pull request.
    ///
    /// Uses the `/pulls/{n}/comments` endpoint (diff/line comments), which is
    /// distinct from the issue-comments endpoint.
    pub async fn list_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ReviewComment>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/comments?per_page=100",
            self.api_base, owner, repo, pr_number
        );
        self.get_json(&url).await
    }

    /// Submit a review on a pull request (approve, request changes, or comment).
    ///
    /// `event` must be one of `APPROVE`, `REQUEST_CHANGES`, or `COMMENT`.
    /// Errors are surfaced to the caller, which is expected to handle them
    /// non-fatally (e.g. a token without `pull_request` write scope yields a 403).
    #[allow(clippy::too_many_arguments)] // Mirrors the GitHub review API, including exact commit identity.
    pub async fn submit_pull_request_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        event: &str,
        body: &str,
        comments: Vec<ReviewCommentInput>,
        commit_id: Option<&str>,
    ) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/reviews",
            self.api_base, owner, repo, pr_number
        );
        let payload = serde_json::json!({
            "event": event,
            "body": body,
            "comments": comments,
            "commit_id": commit_id,
        });
        let body_bytes = serde_json::to_vec(&payload)?;

        let resp = self
            .send_with_retry(|| self.build_post(&url, &body_bytes))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to submit pull request review on PR #{} (status {}): {}",
                pr_number,
                status,
                text
            );
        }

        info!(
            owner,
            repo,
            pr = pr_number,
            event,
            "Submitted pull request review"
        );
        Ok(())
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
            self.api_base, owner, repo, head_sha
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
                self.api_base, owner, repo, run.id
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
                    self.api_base, owner, repo, job.id
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

/// Whether a GitHub API error indicates the legacy combined-status (Statuses)
/// endpoint is not accessible to the authenticated integration (GitHub App).
///
/// The error is surfaced as a plain string of the form
/// `GitHub API error 403 Forbidden: {"message":"Resource not accessible by integration",...}`.
/// We detect a 403 that is scoped to a permission/access denial so the CI gate can
/// fall back to the modern Checks API instead of aborting.
fn is_status_api_forbidden(err: &anyhow::Error) -> bool {
    let msg = format!("{err}");
    msg.contains("GitHub API error 403")
        && (msg.contains("not accessible") || msg.contains("forbidden"))
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
    #[serde(default)]
    total_count: u64,
}

#[derive(Deserialize)]
struct CheckSuitesResponse {
    check_suites: Vec<CheckSuite>,
}

#[derive(Deserialize)]
struct CheckSuite {
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    latest_check_runs_count: Option<u32>,
    #[serde(default)]
    app: Option<CheckSuiteApp>,
}

#[derive(Deserialize, Debug, Clone)]
struct CheckSuiteApp {
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    name: Option<String>,
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
    sha: String,
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

// ── PR Review & Comment Types ──────────────────────────────────────────────

/// Deserialize a GitHub `user` payload into an optional login string.
///
/// GitHub returns `user` as an object (`{"login": "...", ...}`) on review and
/// review-comment responses, but some callers and fixtures pass a plain login
/// string. This accepts both shapes, plus `null`/absent, mapping the result to
/// the reviewer's login.
fn deserialize_optional_login<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Object(map) => Ok(map
            .get("login")
            .and_then(|v| v.as_str())
            .map(str::to_string)),
        _ => Err(D::Error::custom("expected a user object or login string")),
    }
}

/// A review submitted on a pull request.
///
/// GitHub returns every review ever submitted, including superseded ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
    /// `APPROVED`, `CHANGES_REQUESTED`, or `COMMENTED`.
    pub state: String,
    /// Reviewer's GitHub login (from the nested `user` object).
    #[serde(default, deserialize_with = "deserialize_optional_login")]
    pub user: Option<String>,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// SHA of the commit the review was submitted against.
    #[serde(default)]
    pub commit_id: Option<String>,
    /// Reviewer's relationship to the repository (`OWNER`, `MEMBER`,
    /// `COLLABORATOR`, `CONTRIBUTOR`, `NONE`, ...).
    #[serde(default)]
    pub author_association: Option<String>,
}

impl PrReview {
    pub fn state_enum(&self) -> PrReviewState {
        PrReviewState::from_api_state(&self.state)
    }

    /// Whether the reviewer holds maintainer/collaborator authority on the
    /// repository. Drive-by reviews from outside accounts do not qualify.
    pub fn is_authorized_reviewer(&self) -> bool {
        matches!(
            self.author_association.as_deref(),
            Some("OWNER" | "MEMBER" | "COLLABORATOR")
        )
    }
}

/// The computed, current review state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrReviewState {
    Approved,
    ChangesRequested,
    Commented,
    None,
}

impl PrReviewState {
    /// Parse a raw GitHub review `state` string into the typed state.
    pub fn from_api_state(state: &str) -> Self {
        match state {
            "APPROVED" => PrReviewState::Approved,
            "CHANGES_REQUESTED" => PrReviewState::ChangesRequested,
            "COMMENTED" => PrReviewState::Commented,
            _ => PrReviewState::None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PrReviewState::Approved => "approved",
            PrReviewState::ChangesRequested => "changes_requested",
            PrReviewState::Commented => "commented",
            PrReviewState::None => "none",
        }
    }
}

/// An inline review comment on a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub body: String,
    #[serde(default, deserialize_with = "deserialize_optional_login")]
    pub user: Option<String>,
    #[serde(default)]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Input for an inline comment attached to a submitted review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommentInput {
    /// The path of the file to comment on.
    pub path: String,
    /// The line of the blob in the pull request diff to comment on.
    pub line: u64,
    /// The text of the comment.
    pub body: String,
}

/// Reduce a PR's review history to the latest review per reviewer.
///
/// Reduce a PR's review history to the latest review per reviewer.
///
/// Rules:
/// - A review with an approving or change-request verdict (`APPROVED` or
///   `CHANGES_REQUESTED`) takes precedence over a plain comment (`COMMENTED`).
/// - Between two reviews that both carry a verdict, the newer one wins
///   (later `submitted_at`, falling back to insertion order).
/// - A `COMMENTED` review carries no verdict and never supersedes or withdraws
///   an existing `APPROVED` or `CHANGES_REQUESTED` review.
/// - Between reviews without a verdict, the newer one wins.
/// - Reviews without a reviewer login are ignored.
pub fn latest_review_per_user(reviews: &[PrReview]) -> Vec<&PrReview> {
    let mut latest: Vec<&PrReview> = Vec::new();
    for review in reviews {
        if review.user.as_deref().is_none() || review.user.as_deref() == Some("") {
            continue;
        }
        let user = review.user.as_deref().unwrap_or("");
        let idx = latest.iter().position(|r| r.user.as_deref() == Some(user));
        let replace = match idx {
            Some(i) => {
                let existing = latest[i];
                let is_newer = match (&existing.submitted_at, &review.submitted_at) {
                    (Some(a), Some(b)) => b > a,
                    (None, Some(_)) => true,
                    (Some(_), None) => false,
                    (None, None) => true,
                };
                let existing_has_verdict = matches!(
                    existing.state_enum(),
                    PrReviewState::Approved | PrReviewState::ChangesRequested
                );
                let new_has_verdict = matches!(
                    review.state_enum(),
                    PrReviewState::Approved | PrReviewState::ChangesRequested
                );

                match (existing_has_verdict, new_has_verdict) {
                    // Both have verdicts: newer verdict wins.
                    (true, true) => is_newer,
                    // New review has a verdict while existing does not: verdict wins.
                    (false, true) => true,
                    // Existing has a verdict while new review is a comment:
                    // comment never overwrites a verdict.
                    (true, false) => false,
                    // Neither has a verdict: newer comment wins.
                    (false, false) => is_newer,
                }
            }
            None => false,
        };
        match idx {
            Some(i) if replace => {
                latest[i] = review;
            }
            None => {
                latest.push(review);
            }
            _ => {}
        }
    }
    latest
}

/// Compute the effective review state of a PR from the (possibly superseded)
/// list of reviews returned by `GET /pulls/{n}/reviews`.
///
/// Rules:
/// - Consider only the latest review per reviewer.
/// - A current `CHANGES_REQUESTED` beats an older `APPROVED` (a reviewer can
///   approve then re-request changes).
/// - `APPROVED` only counts if no reviewer currently has `CHANGES_REQUESTED`.
/// - `COMMENTED` reviews do not carry an approving/rejecting verdict.
pub fn effective_review_state(reviews: &[PrReview]) -> PrReviewState {
    let latest = latest_review_per_user(reviews);

    let mut any_approved = false;
    for review in latest {
        match review.state_enum() {
            PrReviewState::ChangesRequested => return PrReviewState::ChangesRequested,
            PrReviewState::Approved => any_approved = true,
            _ => {}
        }
    }

    if any_approved {
        PrReviewState::Approved
    } else {
        PrReviewState::None
    }
}

fn aggregate_ci_sources(status: Option<CiStatus>, checks: Option<CiStatus>) -> CiStatus {
    let sources = [status, checks];
    if sources
        .iter()
        .any(|s| matches!(s, Some(CiStatus::Failure | CiStatus::Error)))
    {
        CiStatus::Failure
    } else if sources.contains(&Some(CiStatus::Pending)) || sources.iter().all(Option::is_none) {
        CiStatus::Pending
    } else {
        CiStatus::Success
    }
}

/// Determines if a check suite is an unconfigured ghost suite from an installed app that should be ignored.
///
/// GitHub automatically creates a `queued` check suite for every installed GitHub App
/// with checks write permissions on every commit. If the app is not a CI provider (e.g. OpenFlows itself,
/// Devin) or is not configured for the repository (e.g. SonarQube Cloud), it never creates check runs
/// and remains in `queued` status with 0 runs indefinitely.
///
/// A suite is ONLY treated as a ghost suite if:
/// 1. Its status is `queued` (or incomplete with no check runs started).
/// 2. It has explicitly 0 check runs (`latest_check_runs_count == Some(0)`).
/// 3. It belongs to a known non-CI app or unconfigured app (e.g. OpenFlows, Devin, SonarCloud,
///    or any app slug configured in `GITHUB_IGNORED_CHECK_APPS`).
///
/// Active suites from real CI providers (like GitHub Actions, CircleCI, etc.) and completed
/// suites (including failed zero-run suites) are NEVER ignored.
fn is_ghost_check_suite(suite: &CheckSuite) -> bool {
    // Completed suites must ALWAYS be evaluated — never ignore a completed suite
    // (even if it has 0 runs, e.g. a startup failure).
    if suite.status == "completed" {
        return false;
    }

    // If the suite has created check runs or is actively in progress, it is not a ghost.
    if suite.latest_check_runs_count.unwrap_or(0) > 0 || suite.status == "in_progress" {
        return false;
    }

    // Only suites with explicitly 0 runs can be ghost suites.
    if suite.latest_check_runs_count != Some(0) {
        return false;
    }

    // Check the app slug / name against known non-CI or ghost app patterns.
    let app = match &suite.app {
        Some(a) => a,
        None => return false,
    };

    let slug = app.slug.as_deref().unwrap_or("").to_lowercase();
    let name = app.name.as_deref().unwrap_or("").to_lowercase();

    // GitHub Actions is the official GitHub workflow runner — never ignore it.
    if slug == "github-actions" || name == "github actions" {
        return false;
    }

    // Known non-CI or idle apps that spawn ghost suites with 0 runs:
    // - OpenFlows's own GitHub App (orchestrator, not a CI runner)
    // - Devin AI integration (coding assistant, not a CI runner)
    // - SonarQube Cloud / SonarCloud (when unconfigured on the repo, sits idle in queued)
    let is_known_ghost = slug.contains("openflows")
        || name.contains("openflows")
        || slug.contains("devin")
        || name.contains("devin")
        || slug.contains("sonarqubecloud")
        || slug.contains("sonarcloud")
        || name.contains("sonar");

    if is_known_ghost {
        return true;
    }

    // Allow user-configured ignored apps via environment variable (comma-separated slugs)
    if let Ok(ignored_env) = std::env::var("GITHUB_IGNORED_CHECK_APPS") {
        for pattern in ignored_env.split(',') {
            let p = pattern.trim().to_lowercase();
            if !p.is_empty() && (slug.contains(&p) || name.contains(&p)) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod pr_review_tests {
    use super::*;

    fn review(state: &str, user: &str, ts: &str) -> PrReview {
        PrReview {
            state: state.to_string(),
            user: Some(user.to_string()),
            submitted_at: Some(ts.to_string()),
            body: None,
            commit_id: None,
            author_association: None,
        }
    }

    #[test]
    fn test_effective_state_none() {
        assert_eq!(effective_review_state(&[]), PrReviewState::None);
    }

    #[test]
    fn test_effective_state_approved() {
        let reviews = vec![
            review("COMMENTED", "alice", "2026-01-01T00:00:00Z"),
            review("APPROVED", "bob", "2026-01-02T00:00:00Z"),
        ];
        assert_eq!(effective_review_state(&reviews), PrReviewState::Approved);
    }

    #[test]
    fn test_effective_state_changes_requested_beats_approve() {
        let reviews = vec![
            review("APPROVED", "alice", "2026-01-01T00:00:00Z"),
            review("CHANGES_REQUESTED", "alice", "2026-01-03T00:00:00Z"),
        ];
        assert_eq!(
            effective_review_state(&reviews),
            PrReviewState::ChangesRequested
        );
    }

    #[test]
    fn test_effective_state_latest_approve_wins() {
        let reviews = vec![
            review("CHANGES_REQUESTED", "alice", "2026-01-01T00:00:00Z"),
            review("APPROVED", "alice", "2026-01-03T00:00:00Z"),
        ];
        assert_eq!(effective_review_state(&reviews), PrReviewState::Approved);
    }

    #[test]
    fn test_effective_state_multi_reviewer_any_changes_wins() {
        let reviews = vec![
            review("APPROVED", "bob", "2026-01-02T00:00:00Z"),
            review("CHANGES_REQUESTED", "carol", "2026-01-02T00:00:00Z"),
        ];
        assert_eq!(
            effective_review_state(&reviews),
            PrReviewState::ChangesRequested
        );
    }

    #[test]
    fn test_effective_state_ignores_superseded_approve() {
        let reviews = vec![
            review("APPROVED", "alice", "2026-01-01T00:00:00Z"),
            review("CHANGES_REQUESTED", "alice", "2026-01-02T00:00:00Z"),
            review("APPROVED", "alice", "2026-01-03T00:00:00Z"),
        ];
        assert_eq!(effective_review_state(&reviews), PrReviewState::Approved);
    }

    #[test]
    fn test_effective_state_approved_not_superseded_by_later_comment() {
        let reviews = vec![
            review("APPROVED", "alice", "2026-01-01T00:00:00Z"),
            review("COMMENTED", "alice", "2026-01-02T00:00:00Z"),
        ];
        assert_eq!(effective_review_state(&reviews), PrReviewState::Approved);
        let latest = latest_review_per_user(&reviews);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].state_enum(), PrReviewState::Approved);
    }

    #[test]
    fn test_effective_state_changes_requested_not_cleared_by_later_comment() {
        let reviews = vec![
            review("CHANGES_REQUESTED", "alice", "2026-01-01T00:00:00Z"),
            review("COMMENTED", "alice", "2026-01-02T00:00:00Z"),
        ];
        assert_eq!(
            effective_review_state(&reviews),
            PrReviewState::ChangesRequested
        );
        let latest = latest_review_per_user(&reviews);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].state_enum(), PrReviewState::ChangesRequested);
    }

    #[test]
    fn test_effective_state_comment_followed_by_approval_wins() {
        let reviews = vec![
            review("COMMENTED", "alice", "2026-01-01T00:00:00Z"),
            review("APPROVED", "alice", "2026-01-02T00:00:00Z"),
        ];
        assert_eq!(effective_review_state(&reviews), PrReviewState::Approved);
        let latest = latest_review_per_user(&reviews);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].state_enum(), PrReviewState::Approved);
    }

    #[test]
    fn test_state_enum_parsing() {
        assert_eq!(
            PrReviewState::from_api_state("APPROVED"),
            PrReviewState::Approved
        );
        assert_eq!(
            PrReviewState::from_api_state("CHANGES_REQUESTED"),
            PrReviewState::ChangesRequested
        );
        assert_eq!(
            PrReviewState::from_api_state("COMMENTED"),
            PrReviewState::Commented
        );
        assert_eq!(
            PrReviewState::from_api_state("DISMISSED"),
            PrReviewState::None
        );
    }
}

#[cfg(test)]
mod status_api_tests {
    use super::*;

    #[test]
    fn detects_status_api_forbidden() {
        let body = r#"GitHub API error 403 Forbidden: {"message":"Resource not accessible by integration","documentation_url":"https://docs.github.com/rest/commits/statuses#get-the-combined-status-for-a-specific-reference","status":"403"}"#;
        let err = anyhow::anyhow!("{}", body);
        assert!(is_status_api_forbidden(&err));
    }

    #[test]
    fn rejects_other_errors() {
        // 404 missing / different non-403 errors must NOT be treated as forbidden.
        let not_found = anyhow::anyhow!("{}", r#"GitHub API error 404: {"message":"Not Found"}"#);
        assert!(!is_status_api_forbidden(&not_found));

        let network =
            anyhow::anyhow!("GitHub API request failed after 3 retries: connection refused");
        assert!(!is_status_api_forbidden(&network));

        let empty = anyhow::anyhow!("something else");
        assert!(!is_status_api_forbidden(&empty));
    }
}

#[cfg(test)]
mod lifecycle_ci_tests {
    use super::*;
    #[tokio::test]
    async fn list_pr_reviews_follows_pagination_so_later_veto_is_seen() {
        let mut server = mockito::Server::new_async().await;
        let approvals: Vec<_> = (0..100)
            .map(|i| {
                serde_json::json!({"state":"APPROVED","user":{"login":format!("u{i}")},
                    "submitted_at":"2026-10-05T00:00:00Z"})
            })
            .collect();
        let page1 = server
            .mock("GET", "/repos/org/repo/pulls/7/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(serde_json::to_string(&approvals).unwrap())
            .create_async()
            .await;
        let page2 = server
            .mock("GET", "/repos/org/repo/pulls/7/reviews?per_page=100&page=2")
            .with_status(200)
            .with_body(r#"[{"state":"CHANGES_REQUESTED","user":{"login":"veto"},"submitted_at":"2026-10-06T00:00:00Z"}]"#)
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        let reviews = client.list_pr_reviews("org", "repo", 7).await.unwrap();
        assert_eq!(reviews.len(), 101);
        assert_eq!(
            effective_review_state(&reviews),
            PrReviewState::ChangesRequested
        );
        page1.assert_async().await;
        page2.assert_async().await;
    }
    #[tokio::test]
    async fn ci_api_does_not_mask_failed_checks_with_successful_status() {
        let mut server = mockito::Server::new_async().await;
        let status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .create_async()
            .await;
        let checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(r#"{"check_suites":[{"status":"completed","conclusion":"failure"}]}"#)
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        assert_eq!(
            client.get_ci_status("org", "repo", "head").await.unwrap(),
            CiStatus::Failure
        );
        status.assert_async().await;
        checks.assert_async().await;
    }
    #[tokio::test]
    async fn ghost_check_suites_with_zero_runs_are_ignored_by_ci_status() {
        let mut server = mockito::Server::new_async().await;
        let status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .create_async()
            .await;
        let checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(
                r#"{"check_suites":[
                {"status":"queued","conclusion":null,"latest_check_runs_count":0,"app":{"slug":"my-openflows-app"}},
                {"status":"completed","conclusion":"success","latest_check_runs_count":1}
            ]}"#,
            )
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        assert_eq!(
            client.get_ci_status("org", "repo", "head").await.unwrap(),
            CiStatus::Success
        );
        status.assert_async().await;
        checks.assert_async().await;
    }

    #[tokio::test]
    async fn real_ci_queued_suite_with_zero_runs_is_not_ignored() {
        let mut server = mockito::Server::new_async().await;
        let status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .create_async()
            .await;
        let checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(
                r#"{"check_suites":[
                {"status":"queued","conclusion":null,"latest_check_runs_count":0,"app":{"slug":"github-actions"}},
                {"status":"completed","conclusion":"success","latest_check_runs_count":1}
            ]}"#,
            )
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        // GitHub Actions queued suite with 0 runs must stay Pending, not be ignored
        assert_eq!(
            client.get_ci_status("org", "repo", "head").await.unwrap(),
            CiStatus::Pending
        );
        status.assert_async().await;
        checks.assert_async().await;
    }

    #[tokio::test]
    async fn generic_queued_suite_with_zero_runs_is_not_ignored() {
        let mut server = mockito::Server::new_async().await;
        let status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .create_async()
            .await;
        let checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(
                r#"{"check_suites":[
                {"status":"queued","conclusion":null,"latest_check_runs_count":0},
                {"status":"completed","conclusion":"success","latest_check_runs_count":1}
            ]}"#,
            )
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        // Unscoped / generic queued suite with 0 runs must stay Pending
        assert_eq!(
            client.get_ci_status("org", "repo", "head").await.unwrap(),
            CiStatus::Pending
        );
        status.assert_async().await;
        checks.assert_async().await;
    }

    #[tokio::test]
    async fn completed_suite_with_zero_runs_and_failure_is_not_ignored() {
        let mut server = mockito::Server::new_async().await;
        let status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .create_async()
            .await;
        let checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(
                r#"{"check_suites":[
                {"status":"completed","conclusion":"failure","latest_check_runs_count":0}
            ]}"#,
            )
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        assert_eq!(
            client.get_ci_status("org", "repo", "head").await.unwrap(),
            CiStatus::Failure
        );
        status.assert_async().await;
        checks.assert_async().await;
    }
    #[tokio::test]
    async fn rejected_merge_is_definitive_but_timeouts_and_server_errors_are_unknown() {
        for (status, definitive) in [
            (400, true),
            (401, true),
            (403, true),
            (404, true),
            (405, true),
            (409, true),
            (422, true),
            (429, true),
            (408, false),
            (499, false),
            (500, false),
        ] {
            let mut server = mockito::Server::new_async().await;
            let request = server
                .mock("PUT", "/repos/org/repo/pulls/1/merge")
                .with_status(status)
                .with_body(r#"{"message":"rejected"}"#)
                .expect(1)
                .create_async()
                .await;
            let client = GithubRestClient {
                api_base: server.url(),
                client: reqwest::Client::new(),
                token: "test".into(),
            };
            let result = client
                .merge_pull_request("org", "repo", 1, "Merge", MergeMethod::Squash, "head")
                .await;
            if definitive {
                assert!(
                    !result
                        .unwrap_or_else(|e| panic!("status {status}: {e}"))
                        .merged
                );
            } else {
                assert!(result.is_err(), "status {status} has an unknown outcome");
            }
            request.assert_async().await;
        }
    }

    #[tokio::test]
    async fn merge_http_request_requires_the_reviewed_sha() {
        let mut server = mockito::Server::new_async().await;
        let merge = server
            .mock("PUT", "/repos/org/repo/pulls/1/merge")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"sha":"reviewed-head"}),
            ))
            .with_status(200)
            .with_body(r#"{"merged":true,"sha":"merge-commit","message":"merged"}"#)
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };
        let result = client
            .merge_pull_request(
                "org",
                "repo",
                1,
                "Merge",
                MergeMethod::Squash,
                "reviewed-head",
            )
            .await
            .unwrap();
        assert!(result.merged);
        assert_eq!(result.sha.as_deref(), Some("merge-commit"));
        merge.assert_async().await;
    }
    #[tokio::test]
    async fn startup_failure_in_check_suites_is_reported_in_failure_details() {
        let mut server = mockito::Server::new_async().await;
        let check_runs = server
            .mock("GET", "/repos/org/repo/commits/head/check-runs")
            .with_status(200)
            .with_body(r#"{"check_runs":[]}"#)
            .expect(2)
            .create_async()
            .await;
        let check_suites = server
            .mock("GET", "/repos/org/repo/commits/head/check-suites")
            .with_status(200)
            .with_body(r#"{"check_suites":[{"status":"completed","conclusion":"startup_failure","app":{"name":"GitHub Actions"}}]}"#)
            .expect(2)
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };

        let structured = client
            .get_failed_checks_detail_structured("org", "repo", "head")
            .await
            .unwrap();
        assert_eq!(structured.failed_checks.len(), 1);
        assert_eq!(structured.failed_checks[0].name, "GitHub Actions");
        assert_eq!(structured.failed_checks[0].conclusion, "startup_failure");

        let text = client
            .get_failed_checks_detail("org", "repo", "head")
            .await
            .unwrap();
        assert!(text.contains("GitHub Actions"));
        assert!(text.contains("startup_failure"));

        check_runs.assert_async().await;
        check_suites.assert_async().await;
    }

    #[tokio::test]
    async fn startup_failure_in_check_runs_is_reported_in_failure_details() {
        let mut server = mockito::Server::new_async().await;
        let check_runs = server
            .mock("GET", "/repos/org/repo/commits/head/check-runs")
            .with_status(200)
            .with_body(r#"{"check_runs":[{"name":"build-and-test","status":"completed","conclusion":"startup_failure","id":42}]}"#)
            .create_async()
            .await;
        let annotations = server
            .mock("GET", "/repos/org/repo/check-runs/42/annotations")
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;
        let actions_runs = server
            .mock(
                "GET",
                "/repos/org/repo/actions/runs?head_sha=head&status=failure&per_page=10",
            )
            .with_status(200)
            .with_body(r#"{"workflow_runs":[]}"#)
            .create_async()
            .await;
        let client = GithubRestClient {
            api_base: server.url(),
            client: reqwest::Client::new(),
            token: "test".into(),
        };

        let structured = client
            .get_failed_checks_detail_structured("org", "repo", "head")
            .await
            .unwrap();
        assert_eq!(structured.failed_checks.len(), 1);
        assert_eq!(structured.failed_checks[0].name, "build-and-test");
        assert_eq!(structured.failed_checks[0].conclusion, "startup_failure");

        check_runs.assert_async().await;
        annotations.assert_async().await;
        actions_runs.assert_async().await;
    }

    #[test]
    fn success_cannot_mask_failed_pending_or_missing_ci() {
        assert_eq!(
            aggregate_ci_sources(Some(CiStatus::Success), Some(CiStatus::Failure)),
            CiStatus::Failure
        );
        assert_eq!(
            aggregate_ci_sources(Some(CiStatus::Success), Some(CiStatus::Pending)),
            CiStatus::Pending
        );
        assert_eq!(aggregate_ci_sources(None, None), CiStatus::Pending);
        assert_eq!(
            aggregate_ci_sources(None, Some(CiStatus::Success)),
            CiStatus::Success
        );
    }
}
