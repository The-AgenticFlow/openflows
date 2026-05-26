// crates/pair-harness/src/pair.rs
//! ForgeSentinelPair - the main pair lifecycle manager.
//!
//! Implements the v3 event-driven architecture where:
//! - FORGE is a long-running process
//! - SENTINEL is spawned fresh per evaluation
//! - The harness uses inotify for zero-polling event detection

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::process::Child;
use tracing::{debug, error, info, warn};

use crate::isolation::FileLockManager;
use crate::process::{ProcessManager, SentinelMode};
use crate::provision::Provisioner;
use crate::reset::ResetManager;
use crate::types::{
    Complexity, ErrorHistory, ErrorHistoryEntry, FsEvent, PairConfig, PairOutcome, StatusJson,
    Ticket, TimeoutProfile, VerificationResult, VerificationState,
};
use crate::watchdog::Watchdog;
use crate::watcher::SharedDirWatcher;
use crate::worktree::{MergeMainResult, SetupWarning, WorktreeManager};

/// Default SENTINEL timeout in seconds. Can be overridden via SPRINTLESS_SENTINEL_TIMEOUT_SECS env var.
/// Must be greater than LLM_TIMEOUT_SECS to allow the LLM time to respond.
const DEFAULT_SENTINEL_TIMEOUT_SECS: u64 = 600; // 10 minutes

/// Default FORGE startup timeout. Can be overridden via SPRINTLESS_FORGE_STARTUP_TIMEOUT_SECS env var.
/// FORGE needs time to initialize and write PLAN.md.
const DEFAULT_FORGE_STARTUP_TIMEOUT_SECS: u64 = 600; // 10 minutes

const MAX_SENTINEL_RETRIES: u32 = 2;
const MIN_SENTINEL_RETRY_INTERVAL_SECS: u64 = 30;

/// Get the SENTINEL timeout from environment or use default.
fn get_sentinel_timeout_secs() -> u64 {
    std::env::var("SPRINTLESS_SENTINEL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SENTINEL_TIMEOUT_SECS)
}

/// Get the FORGE startup timeout from environment or use default.
fn get_forge_startup_timeout_secs() -> u64 {
    std::env::var("SPRINTLESS_FORGE_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FORGE_STARTUP_TIMEOUT_SECS)
}

/// Normalize agent-written STATUS.json status strings to canonical values.
///
/// LLM agents frequently write status variants that don't match the expected
/// canonical set. This function maps common synonyms and misspellings to the
/// closest canonical status so the pair lifecycle can proceed instead of
/// falling into the "unrecognized — blocked" path.
fn normalize_status(raw: &str) -> String {
    let upper = raw.trim().to_uppercase();

    // Exact matches that are already canonical — pass through unchanged.
    match upper.as_str() {
        "PR_OPENED"
        | "COMPLETE"
        | "COMPLETED"
        | "SEGMENTS_COMPLETE"
        | "SEGMENT_COMPLETE_AWAITING_REVIEW"
        | "ALL_SEGMENTS_DONE"
        | "IMPLEMENTATION_COMPLETE"
        | "BLOCKED"
        | "FUEL_EXHAUSTED"
        | "PENDING_REVIEW"
        | "APPROVED_READY"
        | "AWAITING_SENTINEL_REVIEW" => return upper,
        _ => {}
    }

    // Fuzzy mapping: group by canonical target.
    // "DONE" / "FINISHED" / "SUCCESS" / "READY" → COMPLETE (work done, check PR metadata)
    // "PR_CREATED" / "PR_SUBMITTED" / "PR_MERGED" → PR_OPENED
    // "FAILED" / "ERROR" / "STUCK" → BLOCKED
    // "PAUSED" / "WAITING" / "NEEDS_REVIEW" → PENDING_REVIEW
    match upper.as_str() {
        "DONE"
        | "FINISHED"
        | "SUCCESS"
        | "SUCCESSFUL"
        | "READY"
        | "WORK_COMPLETE"
        | "WORK_DONE"
        | "IMPLEMENTATION_DONE"
        | "IMPLEMENTED"
        | "TASK_COMPLETE"
        | "TASK_DONE"
        | "RESOLVED" => "COMPLETE".to_string(),

        "PR_CREATED" | "PR_SUBMITTED" | "PR_OPEN" | "PULL_REQUEST_OPENED" | "PR_READY" => {
            "PR_OPENED".to_string()
        }

        "FAILED" | "FAILURE" | "ERROR" | "ERRORED" | "STUCK" | "CANNOT_PROCEED"
        | "UNABLE_TO_COMPLETE" | "ABORTED" | "ABANDONED" => "BLOCKED".to_string(),

        "PAUSED" | "WAITING" | "NEEDS_REVIEW" | "REVIEW_REQUESTED" | "AWAITING_REVIEW"
        | "IN_REVIEW" | "PARTIAL" | "PARTIALLY_DONE" => "PENDING_REVIEW".to_string(),

        // Segment variants: SEGMENT_1_DONE, SEGMENT_2_COMPLETE, etc.
        _ if upper.starts_with("SEGMENT_")
            && (upper.ends_with("_DONE")
                || upper.ends_with("_COMPLETE")
                || upper.ends_with("_FINISHED")) =>
        {
            // Preserve the original so the SEGMENT_*_DONE pattern still matches downstream.
            upper
        }

        // Keyword-based fuzzy matching: infer intent from keywords in the status string.
        // This is the fallback before treating the status as unrecognized.
        // Rules are ordered from most-specific to least-specific to avoid incorrect mapping.
        _ => {
            // 1. PR-related keywords → PR_OPENED
            if (upper.contains("PR") || upper.contains("PULL_REQUEST"))
                && (upper.contains("OPEN") || upper.contains("CREAT") || upper.contains("SUBMIT"))
            {
                info!(
                    raw = raw,
                    matched = "PR_OPENED",
                    "Keyword-based status normalization: status contains PR + open/create/submit keywords"
                );
                return "PR_OPENED".to_string();
            }

            // 2. Exhaust/fuel keywords → FUEL_EXHAUSTED
            if upper.contains("EXHAUST") || upper.contains("FUEL") || upper.contains("BUDGET") {
                info!(
                    raw = raw,
                    matched = "FUEL_EXHAUSTED",
                    "Keyword-based status normalization: status contains exhaust/fuel/budget keywords"
                );
                return "FUEL_EXHAUSTED".to_string();
            }

            // 3. Sentinel keywords → AWAITING_SENTINEL_REVIEW (more specific than generic REVIEW)
            if upper.contains("SENTINEL") {
                info!(
                    raw = raw,
                    matched = "AWAITING_SENTINEL_REVIEW",
                    "Keyword-based status normalization: status contains sentinel keyword"
                );
                return "AWAITING_SENTINEL_REVIEW".to_string();
            }

            // 4. Approved/ready keywords → APPROVED_READY
            if upper.contains("APPROVE") || (upper.contains("READY") && !upper.contains("PR")) {
                info!(
                    raw = raw,
                    matched = "APPROVED_READY",
                    "Keyword-based status normalization: status contains approve/ready keywords"
                );
                return "APPROVED_READY".to_string();
            }

            // 5. Review keywords → PENDING_REVIEW (most common unrecognized status)
            //    Exclude statuses that also contain completion keywords (DONE/COMPLETE/FINISH/SUCCESS)
            //    since "REVIEW_COMPLETE" means the review is done, not that it's pending.
            let has_completion_keyword = upper.contains("DONE")
                || upper.contains("COMPLETE")
                || upper.contains("FINISH")
                || upper.contains("SUCCESS");
            if !has_completion_keyword
                && (upper.contains("REVIEW")
                    || upper.contains("WAIT")
                    || upper.contains("PAUSE")
                    || upper.contains("HOLD"))
            {
                info!(
                    raw = raw,
                    matched = "PENDING_REVIEW",
                    "Keyword-based status normalization: status contains review/wait/pause/hold keywords"
                );
                return "PENDING_REVIEW".to_string();
            }

            // 6. Done/complete/finish keywords → COMPLETE
            if upper.contains("DONE")
                || upper.contains("COMPLETE")
                || upper.contains("FINISH")
                || upper.contains("SUCCESS")
            {
                info!(
                    raw = raw,
                    matched = "COMPLETE",
                    "Keyword-based status normalization: status contains done/complete/finish/success keywords"
                );
                return "COMPLETE".to_string();
            }

            // 7. Blocked/fail/error/stuck keywords → BLOCKED
            if upper.contains("BLOCK")
                || upper.contains("FAIL")
                || upper.contains("ERROR")
                || upper.contains("STUCK")
                || upper.contains("ABORT")
                || upper.contains("ABANDON")
                || upper.contains("CANNOT")
            {
                info!(
                    raw = raw,
                    matched = "BLOCKED",
                    "Keyword-based status normalization: status contains block/fail/error/stuck keywords"
                );
                return "BLOCKED".to_string();
            }

            // 8. Segment keywords → preserve as non-terminal segment status
            if upper.contains("SEGMENT") {
                info!(
                    raw = raw,
                    matched = &upper,
                    "Keyword-based status normalization: status contains segment keyword, preserving as non-terminal"
                );
                return upper;
            }

            // No keyword match — truly unrecognized
            raw.to_string()
        }
    }
}

const ENV_OVERHEAD_NETWORK_SECS: u64 = 15;
const ENV_OVERHEAD_STREAMING_SECS: u64 = 10;
const ENV_OVERHEAD_BUILD_SECS: u64 = 30;
const ENV_OVERHEAD_BUFFER_SECS: u64 = 20;

fn format_setup_errors(warnings: &[SetupWarning]) -> String {
    warnings
        .iter()
        .map(|w| {
            let files = if w.affected_files.is_empty() {
                String::new()
            } else {
                format!(" (affected: {})", w.affected_files.join(", "))
            };
            format!("[{}] {}{}", w.phase, w.error, files)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn classify_error(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("unmerged") || lower.contains("conflict") {
        "merge_conflict".to_string()
    } else if lower.contains("compilation")
        || lower.contains("error ts")
        || lower.contains("cargo ")
    {
        "compilation_error".to_string()
    } else if lower.contains("test") && (lower.contains("fail") || lower.contains("error")) {
        "test_failure".to_string()
    } else if lower.contains("fetch") || lower.contains("push") || lower.contains("network") {
        "network_error".to_string()
    } else if lower.contains("permission") || lower.contains("denied") {
        "permission_error".to_string()
    } else {
        "unknown".to_string()
    }
}

fn truncate_message(message: &str, max_len: usize) -> String {
    if message.len() <= max_len {
        message.to_string()
    } else {
        format!("{}...[truncated]", &message[..max_len])
    }
}

fn compute_effective_timeout(base_secs: u64, complexity: &Complexity) -> u64 {
    let overhead =
        ENV_OVERHEAD_NETWORK_SECS + ENV_OVERHEAD_STREAMING_SECS + ENV_OVERHEAD_BUFFER_SECS;
    let build_overhead = match complexity {
        Complexity::Low => ENV_OVERHEAD_BUILD_SECS / 2,
        Complexity::Medium => ENV_OVERHEAD_BUILD_SECS,
        Complexity::High => ENV_OVERHEAD_BUILD_SECS * 2,
    };
    base_secs + overhead + build_overhead
}

struct SentinelTracker {
    mode: SentinelMode,
    spawn_time: Instant,
    child: Child,
    timeout_secs: u64,
}

/// Tracks whether a SENTINEL process is actively running or its spawn was
/// deferred due to the retry interval.  The deferred variant prevents the
/// event loop from spinning when `check_sentinel_retry_interval()` returns
/// false — the caller treats "deferred" the same as "active" for the purpose
/// of deciding whether to wait rather than re-spawn.
enum SentinelState {
    Active(SentinelTracker),
    Deferred { retry_after: Instant },
}

struct SentinelFailureInfo {
    mode: SentinelMode,
    reason: String,
}

struct SentinelRetryState {
    plan_review_retries: u32,
    segment_eval_retries: std::collections::HashMap<u32, u32>,
    final_review_retries: u32,
}

impl SentinelRetryState {
    fn new() -> Self {
        Self {
            plan_review_retries: 0,
            segment_eval_retries: std::collections::HashMap::new(),
            final_review_retries: 0,
        }
    }

    fn get(&self, mode: &SentinelMode) -> u32 {
        match mode {
            SentinelMode::PlanReview => self.plan_review_retries,
            SentinelMode::SegmentEval(n) => *self.segment_eval_retries.get(n).unwrap_or(&0),
            SentinelMode::FinalReview => self.final_review_retries,
        }
    }

    fn increment(&mut self, mode: &SentinelMode) {
        match mode {
            SentinelMode::PlanReview => self.plan_review_retries += 1,
            SentinelMode::SegmentEval(n) => {
                *self.segment_eval_retries.entry(*n).or_insert(0) += 1;
            }
            SentinelMode::FinalReview => self.final_review_retries += 1,
        }
    }

    fn reset(&mut self, mode: &SentinelMode) {
        match mode {
            SentinelMode::PlanReview => self.plan_review_retries = 0,
            SentinelMode::SegmentEval(n) => {
                self.segment_eval_retries.remove(n);
            }
            SentinelMode::FinalReview => self.final_review_retries = 0,
        }
    }

    fn reset_all(&mut self) {
        self.plan_review_retries = 0;
        self.segment_eval_retries.clear();
        self.final_review_retries = 0;
    }
}

/// The main FORGE-SENTINEL pair lifecycle manager.
pub struct ForgeSentinelPair {
    config: PairConfig,
    worktree: WorktreeManager,
    locks: FileLockManager,
    process: ProcessManager,
    reset: ResetManager,
    watchdog: Watchdog,
    start_time: Instant,
    sentinel_tracker: Option<SentinelState>,
    forge_spawn_time: Instant,
    sentinel_retries: SentinelRetryState,
    last_sentinel_spawn_time: Option<Instant>,
    last_sentinel_failure: Option<SentinelFailureInfo>,
    ticket_id: String,
    plan_approved: bool,
    final_approved: bool,
    contract_timeout: Option<TimeoutProfile>,
    error_feedback_attempts: u32,
    verification_state: VerificationState,
    /// Counts consecutive rapid FORGE exits (<30s). Used to break infinite
    /// respawn loops when progress files are stale from a previous lifecycle.
    rapid_exit_count: u32,
}

/// Maximum consecutive rapid FORGE exits before giving up.
const MAX_RAPID_EXITS: u32 = 5;

impl ForgeSentinelPair {
    /// Create a new ForgeSentinelPair.
    pub fn new(config: PairConfig) -> Self {
        // Use the project_root from config (contains .git)
        let project_root = config.project_root.clone();
        let cli_backend = config.cli_backend;

        Self {
            worktree: WorktreeManager::new(&project_root),
            locks: FileLockManager::new(&project_root),
            process: match (&config.redis_url, &config.proxy_url) {
                (Some(redis_url), Some(proxy_url)) => ProcessManager::with_proxy(
                    &config.github_token,
                    Some(redis_url.clone()),
                    proxy_url,
                    &config.worktree,
                    &config.shared,
                )
                .with_default_backend(cli_backend),
                (Some(redis_url), None) => ProcessManager::with_redis(
                    &config.github_token,
                    redis_url,
                    &config.worktree,
                    &config.shared,
                )
                .with_default_backend(cli_backend),
                (None, Some(proxy_url)) => ProcessManager::with_proxy(
                    &config.github_token,
                    None,
                    proxy_url,
                    &config.worktree,
                    &config.shared,
                )
                .with_default_backend(cli_backend),
                (None, None) => {
                    ProcessManager::new(&config.github_token, &config.worktree, &config.shared)
                        .with_default_backend(cli_backend)
                }
            },
            reset: ResetManager::new(config.shared.clone(), config.max_resets),
            watchdog: Watchdog::new(config.shared.clone(), config.watchdog_timeout_secs),
            verification_state: VerificationState::new(config.max_verify_attempts),
            config,
            start_time: Instant::now(),
            sentinel_tracker: None,
            forge_spawn_time: Instant::now(),
            sentinel_retries: SentinelRetryState::new(),
            last_sentinel_spawn_time: None,
            last_sentinel_failure: None,
            ticket_id: String::new(),
            plan_approved: false,
            final_approved: false,
            contract_timeout: None,
            error_feedback_attempts: 0,
            rapid_exit_count: 0,
        }
    }

    /// Run the pair lifecycle for a ticket.
    ///
    /// This is the main event loop that:
    /// 1. Provisions the worktree and configuration
    /// 2. Spawns FORGE
    /// 3. Watches for filesystem events
    /// 4. Spawns SENTINEL for evaluations
    /// 5. Handles context resets
    /// 6. Returns the final outcome
    pub async fn run(&mut self, ticket: &Ticket) -> Result<PairOutcome> {
        info!(
            pair = %self.config.pair_id,
            ticket = %ticket.id,
            "Starting pair lifecycle"
        );

        self.start_time = Instant::now();
        self.ticket_id = ticket.id.clone();

        // Check if this is a resume with existing approved plan
        let contract_path = self.config.shared.join("CONTRACT.md");
        if contract_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&contract_path).await {
                if content.contains("status: AGREED") || content.contains("status: \"AGREED\"") {
                    self.plan_approved = true;
                    self.contract_timeout = Self::parse_timeout_profile(&content);
                    info!(timeout = ?self.contract_timeout, "Resuming with approved plan - skipping plan review phase");
                }
            }
        }

        // Check if this is a conflict rework — skip plan review, go straight to implementation
        let conflict_resolution_path = self.config.shared.join("CONFLICT_RESOLUTION.md");
        if conflict_resolution_path.exists() {
            self.plan_approved = true;
            self.final_approved = true;
            info!(
                pair = %self.config.pair_id,
                "CONFLICT_RESOLUTION.md detected — skipping plan/final review, forge will resolve conflicts"
            );
        }

        // Check if this is a CI fix rework — skip plan review, go straight to implementation
        let ci_fix_path = self.config.shared.join("CI_FIX.md");
        if ci_fix_path.exists() {
            self.plan_approved = true;
            self.final_approved = true;
            info!(
                pair = %self.config.pair_id,
                "CI_FIX.md detected — skipping plan/final review, forge will fix CI failures"
            );
        }

        // Check if ERROR_FEEDBACK.md exists — skip plan/final review, forge will attempt self-repair
        let error_feedback_path = self.config.shared.join("ERROR_FEEDBACK.md");
        if error_feedback_path.exists() {
            self.plan_approved = true;
            self.final_approved = true;
            info!(
                pair = %self.config.pair_id,
                "ERROR_FEEDBACK.md detected — skipping plan/final review, forge will attempt self-repair"
            );
        }

        // Check if this is a resume with existing final approval
        let final_review_path = self.config.shared.join("final-review.md");
        if final_review_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&final_review_path).await {
                if content.contains("APPROVED") {
                    self.final_approved = true;
                    info!("Resuming with final approval - FORGE should create PR");
                }
            }
        }

        // Check if all segments are already approved on resume
        if self.plan_approved && self.all_segments_approved().await? {
            if final_review_path.exists() {
                self.final_approved = true;
                info!("All segments approved and final review exists on resume");
            } else {
                info!("All segments approved on resume - will proceed to final review");
            }
        }

        // 1. Provision worktree (reuses existing if on correct branch)
        self.provision_worktree(ticket).await?;

        // 1b. If conflict rework: merge origin/main into worktree so conflicts are visible to FORGE
        if conflict_resolution_path.exists() {
            match self.worktree.merge_origin_main(&self.config.worktree) {
                Ok(MergeMainResult::Clean) => {
                    info!(
                        pair = %self.config.pair_id,
                        "origin/main merged cleanly during conflict rework — force-pushing updated branch to remote"
                    );
                    if let Err(e) = self.worktree.force_push_branch(&self.config.worktree) {
                        warn!(
                            pair = %self.config.pair_id,
                            error = %e,
                            "Failed to force-push after clean merge — GitHub PR may still show conflicts"
                        );
                    }
                }
                Ok(MergeMainResult::Conflict { conflicted_files }) => {
                    info!(
                        pair = %self.config.pair_id,
                        files = conflicted_files.len(),
                        "Conflict markers materialized in worktree — FORGE will resolve"
                    );
                }
                Err(e) => {
                    warn!(
                        pair = %self.config.pair_id,
                        error = %e,
                        "Failed to merge origin/main into worktree for conflict rework — writing ERROR_FEEDBACK.md"
                    );
                    self.write_error_feedback(
                        "git_operation",
                        &e.to_string(),
                        Some("Run `git merge --abort` to clean up, then `git fetch origin main && git merge origin/main --no-edit`"),
                    ).await?;
                }
            }
        }

        // 1c. If CI fix rework: merge origin/main so FORGE has latest workflow files + detects conflicts
        if ci_fix_path.exists() {
            match self.worktree.merge_origin_main(&self.config.worktree) {
                Ok(MergeMainResult::Clean) => {
                    info!(
                        pair = %self.config.pair_id,
                        "origin/main merged cleanly before CI fix — FORGE has latest workflows"
                    );
                    if let Err(e) = self.worktree.force_push_branch(&self.config.worktree) {
                        warn!(
                            pair = %self.config.pair_id,
                            error = %e,
                            "Failed to force-push after clean merge — GitHub PR may be stale"
                        );
                    }
                }
                Ok(MergeMainResult::Conflict { conflicted_files }) => {
                    info!(
                        pair = %self.config.pair_id,
                        files = conflicted_files.len(),
                        "Merge conflicts surfaced during CI fix prep — FORGE will resolve both conflicts and CI failures"
                    );
                }
                Err(e) => {
                    warn!(
                        pair = %self.config.pair_id,
                        error = %e,
                        "Failed to merge origin/main before CI fix — writing ERROR_FEEDBACK.md"
                    );
                    self.write_error_feedback(
                        "git_operation",
                        &e.to_string(),
                        Some("Run `git merge --abort` to clean up, then `git fetch origin main && git merge origin/main --no-edit`"),
                    ).await?;
                }
            }
        }

        // 2. Provision configuration files
        self.provision_config(ticket).await?;

        // 3. Seed initial file locks
        self.seed_locks(ticket).await?;

        // 4. Create shared directory structure
        self.create_shared_structure().await?;

        // 4b. Reset watchdog so stale WORKLOG.md mtime from a previous
        //     lifecycle doesn't cause an immediate stall detection.
        self.watchdog.reset();

        // 5. Write TICKET.md and TASK.md
        self.write_task_context(ticket).await?;

        // 6. Spawn FORGE process
        let mut forge = self.spawn_forge().await?;

        // 7. Start filesystem watcher
        let mut watcher = SharedDirWatcher::new(&self.config.shared)?;

        // 8. Event loop
        let outcome = self.event_loop(&mut forge, &mut watcher).await?;

        // 9. Cleanup
        self.cleanup(&forge).await?;

        info!(
            pair = %self.config.pair_id,
            outcome = ?outcome,
            elapsed = ?self.start_time.elapsed(),
            "Pair lifecycle complete"
        );

        Ok(outcome)
    }

    /// The main event loop.
    async fn event_loop(
        &mut self,
        forge: &mut Child,
        watcher: &mut SharedDirWatcher,
    ) -> Result<PairOutcome> {
        loop {
            // Check if a deferred SENTINEL spawn interval has elapsed.
            if let Some(SentinelState::Deferred { retry_after }) = &self.sentinel_tracker {
                if Instant::now() >= *retry_after {
                    debug!("SENTINEL spawn defer interval elapsed — clearing deferred state");
                    self.sentinel_tracker = None;
                }
            }

            // Check if SENTINEL has already exited.
            if let Some(SentinelState::Active(tracker)) = &mut self.sentinel_tracker {
                match tracker.child.try_wait() {
                    Ok(Some(status)) => {
                        let mode = tracker.mode.clone();
                        if status.success() {
                            self.materialize_sentinel_artifact(&mode).await?;
                            self.sentinel_retries.reset(&mode);
                        } else {
                            warn!(
                                mode = ?mode,
                                exit_code = ?status.code(),
                                "SENTINEL exited with error before producing a watched artifact"
                            );
                            self.last_sentinel_failure = Some(SentinelFailureInfo {
                                mode: mode.clone(),
                                reason: format!("exit code {:?}", status.code()),
                            });
                            // Do NOT reset retry counter on failure — let it accumulate
                            // so MAX_SENTINEL_RETRIES eventually triggers the synthetic
                            // rejection fallback that breaks the loop.
                        }
                        self.sentinel_tracker = None;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(mode = ?tracker.mode, error = %e, "Failed to poll SENTINEL status");
                        self.sentinel_tracker = None;
                    }
                }
            }

            // Check for SENTINEL timeout
            if let Some(SentinelState::Active(tracker)) = &mut self.sentinel_tracker {
                if tracker.spawn_time.elapsed().as_secs() > tracker.timeout_secs {
                    warn!(
                        mode = ?tracker.mode,
                        "SENTINEL timed out after {}s",
                        tracker.timeout_secs
                    );
                    let mode = tracker.mode.clone();
                    let timeout_secs = tracker.timeout_secs;
                    let _ = self.process.kill(&mut tracker.child).await;
                    let stderr_excerpt = self.read_sentinel_stderr_excerpt(&mode).await;
                    self.last_sentinel_failure = Some(SentinelFailureInfo {
                        mode: mode.clone(),
                        reason: format!("timeout after {}s", timeout_secs),
                    });
                    let _ = self
                        .reset
                        .append_sentinel_failure(
                            &format!("{:?}", mode),
                            &format!("timeout after {}s", timeout_secs),
                            stderr_excerpt.as_deref(),
                        )
                        .await;
                    // Do NOT reset retry counter on timeout — same as error exit,
                    // let it accumulate so MAX_SENTINEL_RETRIES breaks the loop.
                    self.sentinel_tracker = None;
                }
            }

            // Check for FORGE startup timeout (no PLAN.md written)
            let forge_startup_timeout = get_forge_startup_timeout_secs();
            let plan_path = self.config.shared.join("PLAN.md");
            if !plan_path.exists()
                && self.forge_spawn_time.elapsed().as_secs() > forge_startup_timeout
            {
                error!(
                    "FORGE startup timeout - no PLAN.md after {}s",
                    forge_startup_timeout
                );

                // Check if FORGE is still running
                if self.process.is_running(forge).await {
                    warn!("Killing stuck FORGE process and respawning");
                    self.process.kill(forge).await?;
                    self.sentinel_retries.reset_all();
                    *forge = self.spawn_forge_resume().await?;
                    self.reset.increment_reset();
                }
            }

            // Check for filesystem events (with timeout)
            let event = watcher.recv_timeout(Duration::from_millis(100));

            if let Some(evt) = event {
                match evt {
                    FsEvent::PlanWritten => {
                        // Only spawn SENTINEL for plan review if plan hasn't been approved yet
                        if !self.plan_approved && self.sentinel_tracker.is_none() {
                            info!("PLAN.md written - spawning SENTINEL for plan review");
                            self.spawn_sentinel_for_plan().await?;
                        } else if self.plan_approved {
                            debug!("PLAN.md written but plan already approved - ignoring");
                        } else {
                            warn!("SENTINEL already active - skipping duplicate spawn");
                        }
                    }

                    FsEvent::ContractWritten => {
                        self.sentinel_tracker = None;
                        let status = self.read_contract_status().await?;
                        if status == "AGREED" {
                            self.plan_approved = true;
                            self.read_contract_timeout_profile().await?;
                            info!(
                                timeout = ?self.contract_timeout,
                                "Contract agreed - respawning FORGE to begin implementation"
                            );
                            self.process.kill(forge).await?;
                            self.sentinel_retries.reset_all();
                            *forge = self.spawn_forge_resume().await?;
                        } else {
                            info!("Contract has issues - FORGE must revise plan");
                        }
                    }

                    FsEvent::WorklogUpdated => {
                        if self.final_approved {
                            debug!("Worklog updated but final already approved — skipping SENTINEL spawn");
                        } else if self.all_segments_approved().await? {
                            info!("All segments complete - spawning SENTINEL for final review");
                            self.spawn_sentinel_for_final().await?;
                        } else if let Some(segment_n) = self.next_segment_to_eval().await? {
                            info!("Spawning SENTINEL for segment {} eval", segment_n);
                            self.spawn_sentinel_for_segment(segment_n).await?;
                        }
                        self.watchdog.reset();
                    }

                    FsEvent::SegmentEvalWritten(n) => {
                        self.sentinel_tracker = None;
                        info!("Segment {} evaluation complete", n);

                        // Check if this was the last segment - if so, spawn final review
                        if self.all_segments_approved().await? {
                            info!("All segments approved - spawning SENTINEL for final review");
                            self.spawn_sentinel_for_final().await?;
                        }
                    }

                    FsEvent::FinalReviewWritten => {
                        self.sentinel_tracker = None;
                        let verdict = self.read_final_review_verdict().await?;
                        if verdict == "APPROVED" {
                            self.final_approved = true;
                            info!("Final review APPROVED - respawning FORGE to create PR");
                            self.process.kill(forge).await?;
                            *forge = self.spawn_forge_for_pr().await?;
                        } else {
                            info!("Final review REJECTED - FORGE must fix issues");
                        }
                    }

                    FsEvent::StatusJsonWritten => {
                        self.sentinel_tracker = None;
                        let awaiting_review = self.check_status_awaiting_sentinel_review().await;
                        if let Some(status) = self.read_status().await? {
                            return Ok(status);
                        }
                        if awaiting_review && self.sentinel_tracker.is_none() {
                            if self.all_segments_approved().await? {
                                info!(
                                    "AWAITING_SENTINEL_REVIEW — spawning SENTINEL for final review"
                                );
                                self.spawn_sentinel_for_final().await?;
                            } else if let Some(segment_n) = self.next_segment_to_eval().await? {
                                info!(
                                    "AWAITING_SENTINEL_REVIEW — spawning SENTINEL for segment {} eval",
                                    segment_n
                                );
                                self.spawn_sentinel_for_segment(segment_n).await?;
                            } else {
                                info!(
                                    "AWAITING_SENTINEL_REVIEW — spawning SENTINEL for final review"
                                );
                                self.spawn_sentinel_for_final().await?;
                            }
                        }
                    }

                    FsEvent::HandoffWritten => {
                        self.sentinel_tracker = None;
                        info!("Context reset - respawning FORGE");
                        self.process.kill(forge).await?;
                        self.sentinel_retries.reset_all();
                        *forge = self.spawn_forge_resume().await?;
                        self.reset.increment_reset();
                    }
                }
            }

            // Check watchdog (every ~60 seconds)
            if self.start_time.elapsed().as_secs().wrapping_rem(60) == 0 {
                let status = self.watchdog.check_stalled()?;
                if status.is_stalled() {
                    let total_elapsed = self.start_time.elapsed().as_secs();
                    let worklog_elapsed = status.elapsed().unwrap_or_default().as_secs();
                    warn!(
                        elapsed_secs = worklog_elapsed,
                        "Pair stalled - no WORKLOG update for too long, killing pair"
                    );
                    let _ = self.process.kill(forge).await;
                    self.cleanup(forge).await?;
                    return Ok(PairOutcome::Blocked {
                        reason: format!(
                            "Pair stalled — no progress for {}s (total elapsed: {}s)",
                            worklog_elapsed, total_elapsed
                        ),
                        blockers: vec![],
                    });
                }
            }

            // Check if FORGE has exited
            if !self.process.is_running(forge).await {
                // Drain any pending watcher events first - FORGE may have written
                // files just before exiting and the events may not have been
                // processed yet in the event-handling section above.
                while let Some(evt) = watcher.try_recv() {
                    match evt {
                        FsEvent::PlanWritten => {
                            if !self.plan_approved && self.sentinel_tracker.is_none() {
                                info!("PLAN.md written (drained after FORGE exit) - spawning SENTINEL for plan review");
                                self.spawn_sentinel_for_plan().await?;
                            }
                        }
                        FsEvent::ContractWritten => {
                            self.sentinel_tracker = None;
                            let status = self.read_contract_status().await?;
                            if status == "AGREED" {
                                self.plan_approved = true;
                                self.read_contract_timeout_profile().await?;
                                info!(timeout = ?self.contract_timeout, "Contract agreed (drained after FORGE exit) - respawning FORGE to begin implementation");
                                self.process.kill(forge).await?;
                                self.sentinel_retries.reset_all();
                                *forge = self.spawn_forge_resume().await?;
                            }
                        }
                        FsEvent::WorklogUpdated => {
                            if self.all_segments_approved().await? {
                                self.spawn_sentinel_for_final().await?;
                            } else if let Some(segment_n) = self.next_segment_to_eval().await? {
                                self.spawn_sentinel_for_segment(segment_n).await?;
                            }
                            self.watchdog.reset();
                        }
                        FsEvent::SegmentEvalWritten(n) => {
                            self.sentinel_tracker = None;
                            info!("Segment {} evaluation complete (drained)", n);
                            if self.all_segments_approved().await? {
                                self.spawn_sentinel_for_final().await?;
                            }
                        }
                        FsEvent::FinalReviewWritten => {
                            self.sentinel_tracker = None;
                            let verdict = self.read_final_review_verdict().await?;
                            if verdict == "APPROVED" {
                                self.final_approved = true;
                                info!("Final review APPROVED (drained) - respawning FORGE to create PR");
                                *forge = self.spawn_forge_for_pr().await?;
                            }
                        }
                        FsEvent::StatusJsonWritten => {
                            let awaiting_review =
                                self.check_status_awaiting_sentinel_review().await;
                            if let Some(status) = self.read_status().await? {
                                return Ok(status);
                            }
                            if awaiting_review && self.sentinel_tracker.is_none() {
                                if self.all_segments_approved().await? {
                                    info!("AWAITING_SENTINEL_REVIEW (drained) — spawning SENTINEL for final review");
                                    self.spawn_sentinel_for_final().await?;
                                } else if let Some(segment_n) = self.next_segment_to_eval().await? {
                                    info!(
                                        "AWAITING_SENTINEL_REVIEW (drained) — spawning SENTINEL for segment {} eval",
                                        segment_n
                                    );
                                    self.spawn_sentinel_for_segment(segment_n).await?;
                                } else {
                                    info!("AWAITING_SENTINEL_REVIEW (drained) — spawning SENTINEL for final review");
                                    self.spawn_sentinel_for_final().await?;
                                }
                            }
                        }
                        FsEvent::HandoffWritten => {
                            self.sentinel_tracker = None;
                        }
                    }
                }

                // After draining events, re-evaluate state based on filesystem
                if self.reset.has_handoff() {
                    info!("FORGE exited with handoff - respawning");
                    self.sentinel_retries.reset_all();
                    *forge = self.spawn_forge_resume().await?;
                    self.reset.increment_reset();
                } else if self.config.shared.join("STATUS.json").exists() {
                    if let Some(status) = self.read_status().await? {
                        return Ok(status);
                    }
                } else if self.has_progress_files().await {
                    // FORGE made progress - determine what SENTINEL action is needed
                    //
                    // IMPORTANT: progress files may be stale from a previous lifecycle.
                    // If FORGE ran for less than 30 seconds, it almost certainly didn't
                    // complete a segment — it likely crashed on startup.  Treat this as
                    // a startup error rather than "segment work completed" to avoid an
                    // infinite respawn loop.
                    let forge_uptime = self.forge_spawn_time.elapsed().as_secs();
                    if forge_uptime < 30 && !self.reset.has_handoff() {
                        self.rapid_exit_count += 1;
                        if self.rapid_exit_count >= MAX_RAPID_EXITS {
                            error!(
                                consecutive_exits = self.rapid_exit_count,
                                "FORGE repeatedly exits within 30s — giving up (check FORGE stderr logs for startup errors)"
                            );
                            return Ok(PairOutcome::Blocked {
                                reason: format!(
                                    "FORGE exited {} times within 30 seconds — likely a startup error. \
                                     Check forge-stderr.log in the shared directory.",
                                    self.rapid_exit_count
                                ),
                                blockers: vec![],
                            });
                        }
                        warn!(
                            elapsed_secs = forge_uptime,
                            consecutive = self.rapid_exit_count,
                            "FORGE exited quickly with stale progress files — likely startup error, not segment completion"
                        );
                        // Treat the same as "no progress" quick exit: retry with backoff
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        *forge = self.spawn_forge().await?;
                    } else if self.sentinel_tracker.is_some() {
                        info!("FORGE exited but SENTINEL is active - waiting for completion");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } else {
                        // FORGE ran for a healthy duration — reset rapid exit counter
                        self.rapid_exit_count = 0;

                        // Check the lifecycle phase and spawn SENTINEL if needed
                        let plan_exists = self.config.shared.join("PLAN.md").exists();
                        let contract_exists = self.config.shared.join("CONTRACT.md").exists();
                        let worklog_exists = self.config.shared.join("WORKLOG.md").exists();
                        let final_review_exists =
                            self.config.shared.join("final-review.md").exists();

                        if plan_exists && !contract_exists && !self.plan_approved {
                            // Plan written but not reviewed - spawn SENTINEL
                            info!("FORGE exited after writing PLAN.md - spawning SENTINEL for plan review");
                            self.spawn_sentinel_for_plan().await?;
                        } else if contract_exists && self.plan_approved && !worklog_exists {
                            info!("FORGE exited, contract agreed - respawning FORGE to begin implementation");
                            self.sentinel_retries.reset_all();
                            *forge = self.spawn_forge_resume().await?;
                        } else if worklog_exists {
                            // Implementation in progress - check segment status
                            if self.all_segments_approved().await? {
                                if !final_review_exists && !self.final_approved {
                                    info!("FORGE exited, all segments approved - spawning SENTINEL for final review");
                                    self.spawn_sentinel_for_final().await?;
                                } else {
                                    // Final review already approved (CI fix / conflict rework)
                                    // or already exists — respawn FORGE to continue rework
                                    info!("FORGE exited with final approval already granted — respawning to continue rework");
                                    self.sentinel_retries.reset_all();
                                    *forge = self.spawn_forge_resume().await?;
                                }
                            } else if let Some(segment_n) = self.next_segment_to_eval().await? {
                                info!(
                                    "FORGE exited - spawning SENTINEL for segment {} eval",
                                    segment_n
                                );
                                self.spawn_sentinel_for_segment(segment_n).await?;
                            } else if self.segments_remaining_in_plan().await?.is_some() {
                                // Written segments have evals, but PLAN has more segments to implement.
                                // This is expected with --print mode: FORGE exits after each segment,
                                // and we just respawn to continue the next one.
                                info!("FORGE exited after segment work - respawning to continue implementation");
                                self.sentinel_retries.reset_all();
                                *forge = self.spawn_forge_resume().await?;
                            } else {
                                info!("FORGE exited with partial worklog - respawning to continue implementation");
                                self.sentinel_retries.reset_all();
                                *forge = self.spawn_forge_resume().await?;
                                self.reset.increment_reset();
                            }
                        } else {
                            info!("FORGE exited after making progress - respawning to continue");
                            self.sentinel_retries.reset_all();
                            *forge = self.spawn_forge_resume().await?;
                        }
                    }
                } else {
                    // No progress files - check if FORGE just started and may not have had time
                    let forge_uptime = self.forge_spawn_time.elapsed().as_secs();
                    if forge_uptime < 30 {
                        self.rapid_exit_count += 1;
                        if self.rapid_exit_count >= MAX_RAPID_EXITS {
                            error!(
                                consecutive_exits = self.rapid_exit_count,
                                "FORGE repeatedly exits within 30s — giving up (check FORGE stderr logs for startup errors)"
                            );
                            return Ok(PairOutcome::Blocked {
                                reason: format!(
                                    "FORGE exited {} times within 30 seconds — likely a startup error. \
                                     Check forge-stderr.log in the shared directory.",
                                    self.rapid_exit_count
                                ),
                                blockers: vec![],
                            });
                        }
                        // Very quick exit - likely a startup error, retry
                        warn!(
                            elapsed_secs = forge_uptime,
                            consecutive = self.rapid_exit_count,
                            "FORGE exited quickly without progress - retrying spawn"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        *forge = self.spawn_forge().await?;
                    } else {
                        // Ran for a while but produced nothing - synthesize handoff and respawn
                        warn!("FORGE exited unexpectedly after {}s without progress - synthesizing handoff", forge_uptime);
                        self.reset.synthesize_handoff().await?;
                        self.sentinel_retries.reset_all();
                        *forge = self.spawn_forge_resume().await?;
                        self.reset.increment_reset();
                    }
                }
            }

            // Check reset limit
            if self.reset.reset_count() >= self.config.max_resets {
                warn!("Max resets exceeded - fuel exhausted");
                return Ok(PairOutcome::FuelExhausted {
                    reason: "Maximum context resets exceeded".to_string(),
                    reset_count: self.reset.reset_count(),
                });
            }

            // Small sleep to prevent busy loop
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Provision the worktree for this pair.
    async fn provision_worktree(&mut self, ticket: &Ticket) -> Result<()> {
        let result = self
            .worktree
            .create_worktree(&self.config.pair_id, &ticket.id, &self.config.github_token)
            .await
            .context("Failed to create worktree")?;
        self.config.worktree = result.path;

        if !result.warnings.is_empty() {
            let error_output = format_setup_errors(&result.warnings);
            self.write_error_feedback(
                "setup",
                &error_output,
                Some("Run `git status` and `git diff --name-only --diff-filter=U` to assess worktree state"),
            ).await?;
        } else {
            self.clear_error_feedback().await?;
        }
        Ok(())
    }

    /// Provision configuration files.
    async fn provision_config(&self, _ticket: &Ticket) -> Result<()> {
        // Use project_root where orchestration/plugin exists
        let provisioner = Provisioner::new(&self.config.project_root);

        provisioner
            .provision_pair(
                &self.config.pair_id,
                &self.config.worktree,
                &self.config.shared,
                &self.config.github_token,
                self.config.redis_url.as_deref(),
                self.config.cli_backend,
            )
            .await
    }

    /// Write ERROR_FEEDBACK.md with the current error context.
    ///
    /// OVERWRITES any existing ERROR_FEEDBACK.md — this file represents
    /// ONE current error, not a log of past errors.
    /// Past errors are tracked in error_history.json.
    async fn write_error_feedback(
        &mut self,
        source: &str,
        error_output: &str,
        hint: Option<&str>,
    ) -> Result<()> {
        let attempt = self.error_feedback_attempts + 1;
        let branch = WorktreeManager::branch_name(&self.config.pair_id, &self.ticket_id);

        let history_section = self.format_error_history_section().await?;

        let hint_section = hint
            .map(|h| format!("## Resolution Hints\n{}\n\n", h))
            .unwrap_or_default();

        let content = format!(
            "# Error Feedback — Self-Repair Required\n\n\
             An error occurred during the pair lifecycle that you must resolve.\n\n\
             ## Error Source\n{}\n\n\
             ## Error Output\n```\n{}\n```\n\n\
             ## Context\n\
             - Branch: {}\n\
             - Worktree: {}\n\
             - Attempt: {} of {}\n\n\
             {}{}\
             ## Instructions\n\
             1. Assess the error output above\n\
             2. Take the suggested resolution steps (if any)\n\
             3. If you cannot resolve, write STATUS.json with status BLOCKED\n\
             4. If you resolve the error, continue with your task and write STATUS.json normally\n",
            source,
            error_output,
            branch,
            self.config.worktree.display(),
            attempt,
            self.config.max_verify_attempts,
            hint_section,
            history_section,
        );

        let path = self.config.shared.join("ERROR_FEEDBACK.md");
        // Ensure the shared directory exists — write_error_feedback can be called
        // early (e.g. from provision_worktree) before create_shared_structure runs.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create shared directory for ERROR_FEEDBACK.md")?;
        }
        tokio::fs::write(&path, &content)
            .await
            .context("Failed to write ERROR_FEEDBACK.md")?;

        info!(
            path = %path.display(),
            source,
            attempt,
            "Wrote ERROR_FEEDBACK.md for agent self-repair"
        );

        self.error_feedback_attempts += 1;
        Ok(())
    }

    /// Clear ERROR_FEEDBACK.md — called when error is resolved.
    async fn clear_error_feedback(&self) -> Result<()> {
        let path = self.config.shared.join("ERROR_FEEDBACK.md");
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .context("Failed to remove ERROR_FEEDBACK.md")?;
            debug!("Removed ERROR_FEEDBACK.md — error resolved");
        }
        Ok(())
    }

    /// Run post-completion verification if configured.
    async fn verify_completion(&self) -> Result<VerificationResult> {
        let command = match &self.config.verify_command {
            Some(cmd) => cmd.clone(),
            None => return Ok(VerificationResult::Skipped),
        };

        info!(command = %command, "Running post-completion verification");

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&self.config.worktree)
            .output()
            .await
            .context("Failed to run verification command")?;

        if output.status.success() {
            info!("Verification passed — accepting FORGE completion");
            Ok(VerificationResult::Passed)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let combined = format!("{}\n{}", stdout, stderr);
            warn!(command = %command, "Verification failed — feeding error back to FORGE");
            Ok(VerificationResult::Failed {
                output: combined,
                command,
            })
        }
    }

    /// Append an entry to error_history.json.
    async fn append_error_history(&self, source: &str, message: &str) -> Result<()> {
        let path = self.config.shared.join("error_history.json");

        let mut history = if path.exists() {
            tokio::fs::read_to_string(&path)
                .await
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            ErrorHistory::default()
        };

        history.entries.push(ErrorHistoryEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            error_type: classify_error(message),
            message: truncate_message(message, 2000),
            resolution_attempted: None,
            resolved: false,
        });

        tokio::fs::write(&path, serde_json::to_string_pretty(&history)?).await?;
        Ok(())
    }

    /// Mark the latest unresolved error history entry as resolved.
    async fn mark_error_resolved(&self) -> Result<()> {
        let path = self.config.shared.join("error_history.json");
        if !path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let mut history: ErrorHistory = serde_json::from_str(&content)?;

        if let Some(last_unresolved) = history.entries.iter_mut().rev().find(|e| !e.resolved) {
            last_unresolved.resolved = true;
            last_unresolved.resolution_attempted = Some("resolved_by_forge".to_string());
        }

        tokio::fs::write(&path, serde_json::to_string_pretty(&history)?).await?;
        Ok(())
    }

    /// Format a summary of error history for inclusion in prompts.
    async fn format_error_history_section(&self) -> Result<String> {
        let path = self.config.shared.join("error_history.json");
        if !path.exists() {
            return Ok(String::new());
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .context("Failed to read error_history.json")?;
        let history: ErrorHistory = match serde_json::from_str(&content) {
            Ok(h) => h,
            Err(_) => return Ok(String::new()),
        };

        let unresolved: Vec<_> = history
            .entries
            .iter()
            .rev()
            .take(5)
            .filter(|e| !e.resolved)
            .collect();

        if unresolved.is_empty() {
            return Ok(String::new());
        }

        let mut section = String::from("## Previous Attempts\n");
        for entry in unresolved {
            section.push_str(&format!(
                "- Attempt ({}): {} — {}\n",
                entry.source, entry.error_type, entry.message
            ));
        }
        section.push_str("(See error_history.json for full details)\n\n");
        Ok(section)
    }

    /// Seed initial file locks for the ticket.
    async fn seed_locks(&self, ticket: &Ticket) -> Result<()> {
        self.locks
            .seed_locks(&ticket.touched_files, &self.config.pair_id)?;
        Ok(())
    }

    /// Create shared directory structure.
    async fn create_shared_structure(&self) -> Result<()> {
        let provisioner = Provisioner::new(&self.config.project_root);
        provisioner.create_shared_structure(&self.config.shared)
    }

    /// Write TICKET.md and TASK.md to shared directory.
    async fn write_task_context(&self, ticket: &Ticket) -> Result<()> {
        let provisioner = Provisioner::new(&self.config.project_root);
        provisioner.write_ticket(&self.config.shared, ticket)?;

        let error_feedback_path = self.config.shared.join("ERROR_FEEDBACK.md");
        let conflict_path = self.config.shared.join("CONFLICT_RESOLUTION.md");
        let ci_fix_path = self.config.shared.join("CI_FIX.md");
        let task = if error_feedback_path.exists() {
            format!(
                "Resolve lifecycle errors for ticket {} before proceeding with implementation.\n\n\
                 Branch: {}\n\n\
                 ERROR_FEEDBACK.md in this directory contains error output from the pair lifecycle.\n\
                 You MUST resolve the errors described before implementing the ticket.\n\
                 After resolving errors, continue with implementation and write STATUS.json.\n\n\
                 VALID STATUS.json status values: PR_OPENED, COMPLETE, BLOCKED, FUEL_EXHAUSTED, PENDING_REVIEW. \
                 Do NOT use any other value.",
                ticket.id,
                WorktreeManager::branch_name(&self.config.pair_id, &ticket.id)
            )
        } else if conflict_path.exists() {
            format!(
                "Resolve merge conflicts for ticket {}.\n\n\
                 Branch: {}\n\n\
                 CONFLICT_RESOLUTION.md in this directory contains detailed instructions.\n\
                 Resolve all conflict markers, commit, then force-push with 'git push --force-with-lease origin HEAD' (the branch has diverged due to the merge of origin/main).\n\
                 If a PR already exists for this branch, do NOT create a new one — just push and update STATUS.json.\n\
                 Write STATUS.json with status PR_OPENED, the existing PR URL if known, or create a new PR only if none exists.\n\n\
                 VALID STATUS.json status values: PR_OPENED, COMPLETE, BLOCKED, FUEL_EXHAUSTED, PENDING_REVIEW. \
                 Do NOT use any other value.",
                ticket.id,
                WorktreeManager::branch_name(&self.config.pair_id, &ticket.id)
            )
        } else if ci_fix_path.exists() {
            let ci_fix_content = tokio::fs::read_to_string(&ci_fix_path)
                .await
                .unwrap_or_default();
            let failure_summary = ci_fix_content
                .lines()
                .find(|l| l.starts_with("## Failed Checks"))
                .map(|_| {
                    let after = ci_fix_content
                        .split("## Failed Checks\n\n")
                        .nth(1)
                        .unwrap_or("");
                    after
                        .split("\n## ")
                        .next()
                        .unwrap_or(after)
                        .trim()
                        .to_string()
                })
                .unwrap_or_else(|| "CI checks failed".to_string());
            let job_log_section = ci_fix_content
                .lines()
                .find(|l| l.starts_with("## Job Log Output"))
                .map(|_| {
                    let after = ci_fix_content
                        .split("## Job Log Output")
                        .nth(1)
                        .unwrap_or("");
                    after.trim().to_string()
                })
                .unwrap_or_default();
            let conflict_notice = if self.config.worktree.join(".git").exists()
                && std::process::Command::new("git")
                    .args(["diff", "--name-only", "--diff-filter=U"])
                    .current_dir(&self.config.worktree)
                    .output()
                    .ok()
                    .map(|o| !o.stdout.is_empty())
                    .unwrap_or(false)
            {
                "\n\n**MERGE CONFLICTS DETECTED:** origin/main was merged into your branch and there are conflict markers.\n\
                 Resolve ALL conflict markers (lines with <<<<<<<) BEFORE running CI checks.\n".to_string()
            } else {
                String::new()
            };
            format!(
                "Fix CI failures for ticket {}.\n\n\
                 Branch: {}\n\n\
                 The CI pipeline failed:\n{}\n\n\
                 {}{}\
                 CRITICAL RULES:\n\
                 1. Read .github/workflows/ to find the failing workflow(s). Match the check names above to the job names in the YAML.\n\
                 2. Run the failing job's exact `run:` steps locally. Install any missing tools first.\n\
                 3. Resolve ALL merge conflict markers (if any) BEFORE running CI checks.\n\
                 4. Fix ALL errors, then push ONCE. Do NOT push after fixing only one error — CI will just fail on the next check.\n\
                 5. You MUST update {}/WORKLOG.md as you work — the watchdog will kill your process if WORKLOG.md is not updated within 20 minutes.\n\n\
                 Steps:\n\
                 1. Read .github/workflows/ — find the workflow(s) matching the failed check names above\n\
                 2. Install any missing tools the workflow expects (pip, npm, ruff, etc.)\n\
                 3. Install project deps as the workflow does (pip install -r requirements.txt, npm ci, etc.)\n\
                 4. Resolve any <<<<<<< conflict markers in any files\n\
                 5. Update WORKLOG.md with what you installed\n\
                 6. Run the failing job's steps locally using the exact commands from the workflow YAML\n\
                 7. Fix ALL errors found\n\
                 8. Update WORKLOG.md with what you fixed\n\
                 9. Run ALL failing checks again to confirm everything passes\n\
                 10. git add -A && git commit -m \"fix CI failures\" && git push\n\
                 11. Write STATUS.json with status PR_OPENED\n\n\
                 If a PR already exists for this branch, do NOT create a new one — just push and update STATUS.json\n\n\
                 VALID STATUS.json status values: PR_OPENED, COMPLETE, BLOCKED, FUEL_EXHAUSTED, PENDING_REVIEW. \
                 Do NOT use any other value — an invalid status will be treated as BLOCKED and your fix will be wasted.",
                ticket.id,
                WorktreeManager::branch_name(&self.config.pair_id, &ticket.id),
                failure_summary,
                if job_log_section.is_empty() { String::new() } else { format!("## Job Log Output (for reference)\n\n{}\n\n", job_log_section) },
                conflict_notice,
                self.config.shared.display(),
            )
        } else {
            format!(
                "Implement ticket {}.\n\nBranch: {}\n\nWhen done, open a PR and write STATUS.json.\n\n\
                 VALID STATUS.json status values: PR_OPENED, COMPLETE, BLOCKED, FUEL_EXHAUSTED, PENDING_REVIEW, \
                 AWAITING_SENTINEL_REVIEW, APPROVED_READY, SEGMENT_N_DONE. \
                 Do NOT use any other value — an invalid status will be treated as BLOCKED and your work wasted.",
                ticket.id,
                WorktreeManager::branch_name(&self.config.pair_id, &ticket.id)
            )
        };
        provisioner.write_task(&self.config.shared, &task)
    }

    /// Spawn FORGE process.
    async fn spawn_forge(&mut self) -> Result<Child> {
        self.forge_spawn_time = Instant::now();
        self.process
            .spawn_forge(
                &self.config.pair_id,
                &self.ticket_id,
                &self.config.worktree,
                &self.config.shared,
            )
            .await
    }

    /// Spawn FORGE process in resume mode.
    async fn spawn_forge_resume(&mut self) -> Result<Child> {
        self.append_sentinel_failure_to_handoff().await?;
        self.forge_spawn_time = Instant::now();
        self.sentinel_retries.reset_all();
        self.process
            .spawn_forge_resume(
                &self.config.pair_id,
                &self.ticket_id,
                &self.config.worktree,
                &self.config.shared,
            )
            .await
    }

    /// Spawn FORGE process for PR creation after final approval.
    async fn spawn_forge_for_pr(&mut self) -> Result<Child> {
        self.forge_spawn_time = Instant::now();
        self.process
            .spawn_forge_for_pr(
                &self.config.pair_id,
                &self.ticket_id,
                &self.config.worktree,
                &self.config.shared,
            )
            .await
    }

    /// Resolve the effective timeout for a SENTINEL evaluation based on mode and contract.
    fn resolve_sentinel_timeout(&self, mode: &SentinelMode) -> u64 {
        let (base_secs, complexity) = match &self.contract_timeout {
            Some(profile) => {
                let base = match mode {
                    SentinelMode::PlanReview => profile.plan_review_secs,
                    SentinelMode::SegmentEval(_) => profile.segment_eval_secs,
                    SentinelMode::FinalReview => profile.final_review_secs,
                };
                (base, profile.complexity.clone())
            }
            None => (get_sentinel_timeout_secs(), Complexity::Medium),
        };
        compute_effective_timeout(base_secs, &complexity)
    }

    fn check_sentinel_retry_interval(&self) -> bool {
        if let Some(last) = self.last_sentinel_spawn_time {
            let elapsed = last.elapsed().as_secs();
            if elapsed < MIN_SENTINEL_RETRY_INTERVAL_SECS {
                debug!(
                    elapsed_secs = elapsed,
                    min_secs = MIN_SENTINEL_RETRY_INTERVAL_SECS,
                    "Sentinel retry too soon — deferring"
                );
                return false;
            }
        }
        true
    }

    /// Spawn SENTINEL for plan review.
    async fn spawn_sentinel_for_plan(&mut self) -> Result<()> {
        if !self.check_sentinel_retry_interval() {
            // Spawning is deferred — record this so the event loop knows to
            // wait rather than spinning on the "FORGE exited" branch.
            self.sentinel_tracker = Some(SentinelState::Deferred {
                retry_after: Instant::now() + Duration::from_secs(MIN_SENTINEL_RETRY_INTERVAL_SECS),
            });
            debug!("SENTINEL plan review spawn deferred — retry interval not yet elapsed");
            return Ok(());
        }
        let mode = SentinelMode::PlanReview;
        if self.sentinel_retries.get(&mode) >= MAX_SENTINEL_RETRIES {
            warn!(
                retries = self.sentinel_retries.get(&mode),
                "SENTINEL plan review exceeded max retries — writing synthetic changes_requested"
            );
            self.write_synthetic_plan_rejection().await?;
            self.reset.increment_reset();
            self.sentinel_retries.reset(&mode);
            return Ok(());
        }
        self.sentinel_retries.increment(&mode);
        self.last_sentinel_spawn_time = Some(Instant::now());
        let timeout_secs = self.resolve_sentinel_timeout(&SentinelMode::PlanReview);
        let child = self
            .process
            .spawn_sentinel_with_timeout(
                &self.config.pair_id,
                &self.ticket_id,
                SentinelMode::PlanReview,
                &self.config.worktree,
                &self.config.shared,
                timeout_secs,
            )
            .await?;

        self.sentinel_tracker = Some(SentinelState::Active(SentinelTracker {
            mode: SentinelMode::PlanReview,
            spawn_time: Instant::now(),
            child,
            timeout_secs,
        }));

        Ok(())
    }

    /// Spawn SENTINEL for segment evaluation.
    async fn spawn_sentinel_for_segment(&mut self, segment: u32) -> Result<()> {
        if !self.check_sentinel_retry_interval() {
            // Spawning is deferred — record this so the event loop knows to
            // wait rather than spinning.
            self.sentinel_tracker = Some(SentinelState::Deferred {
                retry_after: Instant::now() + Duration::from_secs(MIN_SENTINEL_RETRY_INTERVAL_SECS),
            });
            debug!(
                segment,
                "SENTINEL segment eval spawn deferred — retry interval not yet elapsed"
            );
            return Ok(());
        }
        let mode = SentinelMode::SegmentEval(segment);
        if self.sentinel_retries.get(&mode) >= MAX_SENTINEL_RETRIES {
            warn!(
                retries = self.sentinel_retries.get(&mode),
                segment,
                "SENTINEL segment eval exceeded max retries — writing synthetic changes_requested"
            );
            self.write_synthetic_segment_rejection(segment).await?;
            self.reset.increment_reset();
            self.sentinel_retries.reset(&mode);
            return Ok(());
        }
        self.sentinel_retries.increment(&mode);
        self.last_sentinel_spawn_time = Some(Instant::now());
        let timeout_secs = self.resolve_sentinel_timeout(&SentinelMode::SegmentEval(segment));
        let child = self
            .process
            .spawn_sentinel_with_timeout(
                &self.config.pair_id,
                &self.ticket_id,
                SentinelMode::SegmentEval(segment),
                &self.config.worktree,
                &self.config.shared,
                timeout_secs,
            )
            .await?;

        self.sentinel_tracker = Some(SentinelState::Active(SentinelTracker {
            mode: SentinelMode::SegmentEval(segment),
            spawn_time: Instant::now(),
            child,
            timeout_secs,
        }));

        Ok(())
    }

    /// Spawn SENTINEL for final review.
    async fn spawn_sentinel_for_final(&mut self) -> Result<()> {
        // Don't spawn if final review already done
        if self.config.shared.join("final-review.md").exists() {
            debug!("Final review already exists - skipping spawn");
            return Ok(());
        }

        if !self.check_sentinel_retry_interval() {
            // Spawning is deferred — record this so the event loop knows to
            // wait rather than spinning.
            self.sentinel_tracker = Some(SentinelState::Deferred {
                retry_after: Instant::now() + Duration::from_secs(MIN_SENTINEL_RETRY_INTERVAL_SECS),
            });
            debug!("SENTINEL final review spawn deferred — retry interval not yet elapsed");
            return Ok(());
        }
        let mode = SentinelMode::FinalReview;
        if self.sentinel_retries.get(&mode) >= MAX_SENTINEL_RETRIES {
            warn!(
                retries = self.sentinel_retries.get(&mode),
                "SENTINEL final review exceeded max retries — writing synthetic rejection"
            );
            self.write_synthetic_final_rejection().await?;
            self.reset.increment_reset();
            self.sentinel_retries.reset(&mode);
            return Ok(());
        }
        self.sentinel_retries.increment(&mode);
        self.last_sentinel_spawn_time = Some(Instant::now());
        info!("Spawning SENTINEL for final review");
        let timeout_secs = self.resolve_sentinel_timeout(&SentinelMode::FinalReview);
        let child = self
            .process
            .spawn_sentinel_with_timeout(
                &self.config.pair_id,
                &self.ticket_id,
                SentinelMode::FinalReview,
                &self.config.worktree,
                &self.config.shared,
                timeout_secs,
            )
            .await?;

        self.sentinel_tracker = Some(SentinelState::Active(SentinelTracker {
            mode: SentinelMode::FinalReview,
            spawn_time: Instant::now(),
            child,
            timeout_secs,
        }));

        Ok(())
    }

    /// Check if all segments from PLAN.md are approved.
    async fn all_segments_approved(&self) -> Result<bool> {
        let plan_path = self.config.shared.join("PLAN.md");
        if !plan_path.exists() {
            return Ok(false);
        }

        let content = tokio::fs::read_to_string(&plan_path).await?;
        let implementation_segments = Self::implementation_segments_from_plan(&content);
        let total_segments = implementation_segments.len();

        if total_segments == 0 {
            // If no segments defined, check if WORKLOG.md exists (implementation done)
            return Ok(self.config.shared.join("WORKLOG.md").exists());
        }

        // Count approved segment evaluations
        let mut approved_count = 0;
        for n in implementation_segments {
            let eval_path = self.config.shared.join(format!("segment-{}-eval.md", n));
            if eval_path.exists() {
                let eval_content = tokio::fs::read_to_string(&eval_path).await?;
                if eval_content.contains("APPROVED") {
                    approved_count += 1;
                }
            }
        }

        Ok(approved_count >= total_segments as u32)
    }

    /// Read CONTRACT.md status.
    async fn read_contract_status(&self) -> Result<String> {
        let path = self.config.shared.join("CONTRACT.md");
        if !path.exists() {
            return Ok("UNKNOWN".to_string());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        if content.contains("status: AGREED") || content.contains("status: \"AGREED\"") {
            Ok("AGREED".to_string())
        } else if content.contains("status: ISSUES") || content.contains("status: \"ISSUES\"") {
            Ok("ISSUES".to_string())
        } else {
            Ok("UNKNOWN".to_string())
        }
    }

    /// Parse the timeout_profile section from CONTRACT.md and store it.
    async fn read_contract_timeout_profile(&mut self) -> Result<()> {
        let path = self.config.shared.join("CONTRACT.md");
        if !path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        self.contract_timeout = Self::parse_timeout_profile(&content);
        Ok(())
    }

    /// Parse timeout_profile from CONTRACT.md markdown content.
    fn parse_timeout_profile(content: &str) -> Option<TimeoutProfile> {
        if !content.contains("timeout_profile:") {
            return None;
        }

        let mut plan_review_secs: Option<u64> = None;
        let mut segment_eval_secs: Option<u64> = None;
        let mut final_review_secs: Option<u64> = None;
        let mut complexity: Option<Complexity> = None;

        let mut in_profile = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("timeout_profile:") {
                in_profile = true;
                continue;
            }
            if in_profile {
                if !trimmed.starts_with('-')
                    && !trimmed.is_empty()
                    && !trimmed
                        .chars()
                        .next()
                        .map(|c| c.is_whitespace())
                        .unwrap_or(false)
                    && !trimmed.starts_with("plan_review")
                    && !trimmed.starts_with("segment_eval")
                    && !trimmed.starts_with("final_review")
                    && !trimmed.starts_with("complexity")
                {
                    in_profile = false;
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("plan_review_secs:") {
                    plan_review_secs = rest.trim().trim_end_matches(',').parse().ok();
                } else if let Some(rest) = trimmed.strip_prefix("segment_eval_secs:") {
                    segment_eval_secs = rest.trim().trim_end_matches(',').parse().ok();
                } else if let Some(rest) = trimmed.strip_prefix("final_review_secs:") {
                    final_review_secs = rest.trim().trim_end_matches(',').parse().ok();
                } else if let Some(rest) = trimmed.strip_prefix("complexity:") {
                    let val = rest.trim().trim_end_matches(',').to_lowercase();
                    complexity = match val.as_str() {
                        "low" => Some(Complexity::Low),
                        "medium" => Some(Complexity::Medium),
                        "high" => Some(Complexity::High),
                        _ => None,
                    };
                }
            }
        }

        match (
            plan_review_secs,
            segment_eval_secs,
            final_review_secs,
            complexity,
        ) {
            (Some(pr), Some(se), Some(fr), Some(cx)) => Some(TimeoutProfile {
                plan_review_secs: pr,
                segment_eval_secs: se,
                final_review_secs: fr,
                complexity: cx,
            }),
            _ => None,
        }
    }

    /// Extract the latest segment number from WORKLOG.md.
    async fn extract_latest_segment(&self) -> Result<u32> {
        let path = self.config.shared.join("WORKLOG.md");
        if !path.exists() {
            return Ok(0);
        }

        let content = tokio::fs::read_to_string(&path).await?;

        let mut latest = 0;
        for line in content.lines() {
            if line.starts_with("## Segment") || line.starts_with("### Segment") {
                if let Some(n) = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|s| s.trim_end_matches(':').parse::<u32>().ok())
                {
                    latest = n;
                }
            }
        }

        Ok(latest)
    }

    /// Find the next segment number that needs SENTINEL evaluation.
    /// Returns None if no segments need evaluation or if WORKLOG.md doesn't exist.
    async fn next_segment_to_eval(&self) -> Result<Option<u32>> {
        let worklog_path = self.config.shared.join("WORKLOG.md");
        if !worklog_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&worklog_path).await?;

        let mut segments_in_worklog: Vec<u32> = Vec::new();
        for line in content.lines() {
            if line.starts_with("## Segment") || line.starts_with("### Segment") {
                if let Some(n) = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|s| s.trim_end_matches(':').parse::<u32>().ok())
                {
                    segments_in_worklog.push(n);
                }
            }
        }

        for n in &segments_in_worklog {
            let eval_path = self.config.shared.join(format!("segment-{}-eval.md", n));
            if !eval_path.exists() {
                return Ok(Some(*n));
            }
        }

        Ok(None)
    }

    /// Check if PLAN.md has more segments than WORKLOG.md has written.
    /// Returns Some(count) of remaining segments, or None if PLAN.md doesn't exist
    /// or has no segment headers.
    ///
    /// This is used to distinguish between:
    /// - FORGE exited after completing a segment (--print mode, normal exit)
    ///   where more segments remain to be implemented
    /// - FORGE exited with a genuinely incomplete/partial worklog
    async fn segments_remaining_in_plan(&self) -> Result<Option<u32>> {
        let plan_path = self.config.shared.join("PLAN.md");
        if !plan_path.exists() {
            return Ok(None);
        }

        let plan_content = tokio::fs::read_to_string(&plan_path).await?;
        let total_in_plan = Self::implementation_segments_from_plan(&plan_content);

        if total_in_plan.is_empty() {
            return Ok(None);
        }

        let worklog_path = self.config.shared.join("WORKLOG.md");
        if !worklog_path.exists() {
            return Ok(Some(total_in_plan.len() as u32));
        }

        let worklog_content = tokio::fs::read_to_string(&worklog_path).await?;
        let written_segments: std::collections::HashSet<u32> = worklog_content
            .lines()
            .filter(|line| line.starts_with("## Segment") || line.starts_with("### Segment"))
            .filter_map(|line| {
                line.split_whitespace()
                    .nth(2)
                    .and_then(|s| s.trim_end_matches(':').parse::<u32>().ok())
            })
            .collect();

        let remaining = total_in_plan
            .iter()
            .filter(|n| !written_segments.contains(n))
            .count() as u32;

        if remaining > 0 {
            Ok(Some(remaining))
        } else {
            Ok(None)
        }
    }

    /// Read final-review.md verdict.
    async fn read_final_review_verdict(&self) -> Result<String> {
        let path = self.config.shared.join("final-review.md");
        if !path.exists() {
            return Ok("UNKNOWN".to_string());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        if content.contains("APPROVED") {
            Ok("APPROVED".to_string())
        } else if content.contains("REJECTED") {
            Ok("REJECTED".to_string())
        } else {
            Ok("UNKNOWN".to_string())
        }
    }

    async fn check_status_awaiting_sentinel_review(&self) -> bool {
        let path = self.config.shared.join("STATUS.json");
        if !path.exists() {
            return false;
        }
        tokio::fs::read_to_string(&path)
            .await
            .is_ok_and(|c| c.contains("AWAITING_SENTINEL_REVIEW"))
    }

    /// Read STATUS.json and convert to PairOutcome.
    /// Returns `Ok(None)` if the file exists but is empty (race: inotify fires before flush).
    /// Handles deserialization errors gracefully by logging a warning and returning None,
    /// rather than crashing the entire pair lifecycle.
    async fn read_status(&mut self) -> Result<Option<PairOutcome>> {
        let path = self.config.shared.join("STATUS.json");
        if !path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&path).await?;

        if content.trim().is_empty() {
            return Ok(None);
        }

        let status: StatusJson = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "Failed to parse STATUS.json — renaming to .broken to break respawn loop"
                );
                let broken_path = self.config.shared.join("STATUS.json.broken");
                let _ = tokio::fs::rename(&path, &broken_path).await;
                return Ok(None);
            }
        };

        let effective = status.effective_status().to_string();
        let normalized = normalize_status(&effective);
        if normalized != effective {
            info!(
                raw = %effective,
                normalized = %normalized,
                "Normalized unrecognized STATUS.json status to canonical value"
            );
        }

        let outcome = match normalized.as_str() {
            "PR_OPENED"
            | "COMPLETE"
            | "COMPLETED"
            | "SEGMENTS_COMPLETE"
            | "SEGMENT_COMPLETE_AWAITING_REVIEW"
            | "ALL_SEGMENTS_DONE" => {
                if status.pr_url.is_some() && !status.pr_url.as_ref().unwrap().is_empty() {
                    PairOutcome::PrOpened {
                        pr_url: status.pr_url.clone().unwrap_or_default(),
                        pr_number: status.pr_number.unwrap_or(0),
                        branch: status.branch.clone().unwrap_or_default(),
                    }
                } else {
                    PairOutcome::Blocked {
                        reason: "Work complete but PR not created - needs push/PR creation"
                            .to_string(),
                        blockers: vec![],
                    }
                }
            }
            "IMPLEMENTATION_COMPLETE" => PairOutcome::Blocked {
                reason: "Implementation complete but PR not created - needs push/PR creation"
                    .to_string(),
                blockers: vec![],
            },
            "BLOCKED" => PairOutcome::Blocked {
                reason: "See blockers".to_string(),
                blockers: status.blockers,
            },
            "FUEL_EXHAUSTED" => PairOutcome::FuelExhausted {
                reason: "Fuel exhausted".to_string(),
                reset_count: status.context_resets,
            },
            "PENDING_REVIEW" | "APPROVED_READY" | "AWAITING_SENTINEL_REVIEW" => {
                self.archive_non_terminal_status(&normalized).await?;
                debug!(
                    status = %normalized,
                    "FORGE requests additional review/work — treating as non-terminal, continuing event loop"
                );
                return Ok(None);
            }
            _ => {
                let s = normalized.as_str();
                if s.starts_with("SEGMENT_")
                    && (s.ends_with("_DONE")
                        || s.ends_with("_COMPLETE")
                        || s.ends_with("_FINISHED"))
                {
                    self.archive_non_terminal_status(s).await?;
                    debug!(status = s, "Intermediate segment status in STATUS.json — treating as non-terminal, continuing event loop");
                    return Ok(None);
                }
                if status.pr_url.as_ref().is_some_and(|url| !url.is_empty()) {
                    warn!(
                        status = %status.effective_status(),
                        pr_url = status.pr_url.as_deref().unwrap_or_default(),
                        "Unrecognized STATUS.json status with PR metadata — treating as PR_OPENED"
                    );
                    PairOutcome::PrOpened {
                        pr_url: status.pr_url.clone().unwrap_or_default(),
                        pr_number: status.pr_number.unwrap_or(0),
                        branch: status.branch.clone().unwrap_or_default(),
                    }
                } else {
                    // Write STATUS_UNRECOGNIZED.md for Nexus fallback re-mapping
                    let unrecognized_path = self.config.shared.join("STATUS_UNRECOGNIZED.md");
                    let remap_content = format!(
                        "# Unrecognized STATUS.json\n\n\
                        Raw status: `{}`\n\
                        Normalized: `{}`\n\n\
                        ## Valid STATUS.json Status Values\n\n\
                        | Status | Category | When to use |\n\
                        |---|---|---|\n\
                        | `PR_OPENED` | Terminal | Work complete, PR created |\n\
                        | `COMPLETE` | Terminal | All work done |\n\
                        | `BLOCKED` | Terminal | Cannot proceed |\n\
                        | `FUEL_EXHAUSTED` | Terminal | Budget/tokens exhausted |\n\
                        | `PENDING_REVIEW` | Non-terminal | Waiting for review |\n\
                        | `AWAITING_SENTINEL_REVIEW` | Non-terminal | Segment done, waiting for SENTINEL |\n\
                        | `APPROVED_READY` | Non-terminal | Changes addressed |\n\
                        | `SEGMENT_N_DONE` | Non-terminal | Segment N complete |\n\n\
                        The agent wrote an unrecognized status. Nexus should interpret the raw status\n\
                        intent and re-map it to the closest valid status above, then re-assign the worker.",
                        status.effective_status(), normalized
                    );
                    let _ = tokio::fs::write(&unrecognized_path, &remap_content).await;

                    warn!(
                        status = %status.effective_status(),
                        "Unrecognized STATUS.json status — treating as blocked (STATUS_UNRECOGNIZED.md written for Nexus fallback)"
                    );
                    PairOutcome::Blocked {
                        reason: format!(
                            "Unrecognized STATUS.json status: {} (normalized: {})",
                            status.effective_status(),
                            normalized
                        ),
                        blockers: vec![],
                    }
                }
            }
        };

        // For terminal outcomes (PR_OPENED, Blocked), run verification if configured
        if matches!(
            outcome,
            PairOutcome::PrOpened { .. } | PairOutcome::Blocked { .. }
        ) {
            match self.verify_completion().await? {
                VerificationResult::Passed => {
                    self.clear_error_feedback().await?;
                    self.mark_error_resolved().await?;
                    return Ok(Some(outcome));
                }
                VerificationResult::Failed { output, command } => {
                    self.verification_state.attempt += 1;

                    if self.verification_state.attempt >= self.verification_state.max_attempts {
                        warn!(
                            attempts = self.verification_state.attempt,
                            "Max verification attempts reached — escalating to nexus"
                        );
                        return Ok(Some(PairOutcome::Blocked {
                            reason: format!(
                                "Verification failed {} times. Last error from `{}):\n{}",
                                self.verification_state.attempt, command, output
                            ),
                            blockers: vec![crate::types::Blocker {
                                blocker_type: "verification_failure".to_string(),
                                description: format!(
                                    "Post-completion verification failed: {}",
                                    command
                                ),
                                nexus_action:
                                    "Review verification errors and decide whether to re-assign or close"
                                        .to_string(),
                            }],
                        }));
                    }

                    self.write_error_feedback(
                        "build_verification",
                        &format!("Verification command `{} failed:\n{}", command, output),
                        Some(&format!(
                            "Run `{} locally, fix ALL errors, then commit and push",
                            command
                        )),
                    )
                    .await?;

                    self.append_error_history("build_verification", &output)
                        .await?;

                    self.archive_non_terminal_status(&normalized).await?;
                    return Ok(None);
                }
                VerificationResult::Skipped => return Ok(Some(outcome)),
            }
        }

        Ok(Some(outcome))
    }

    /// Check if FORGE has made progress (PLAN.md or WORKLOG.md exists).
    async fn has_progress_files(&self) -> bool {
        let plan_path = self.config.shared.join("PLAN.md");
        let worklog_path = self.config.shared.join("WORKLOG.md");

        plan_path.exists() || worklog_path.exists()
    }

    /// Check if we're waiting for SENTINEL output (plan reviewed but no contract).
    #[allow(dead_code)]
    async fn waiting_for_sentinel_output(&self) -> bool {
        let plan_path = self.config.shared.join("PLAN.md");
        let contract_path = self.config.shared.join("CONTRACT.md");
        let worklog_path = self.config.shared.join("WORKLOG.md");

        // Waiting for plan review
        if plan_path.exists() && !contract_path.exists() {
            return true;
        }

        // Waiting for segment eval (WORKLOG exists but no corresponding eval)
        if worklog_path.exists() {
            if let Ok(segment) = self.extract_latest_segment().await {
                if segment > 0 {
                    let eval_path = self
                        .config
                        .shared
                        .join(format!("segment-{}-eval.md", segment));
                    if !eval_path.exists() {
                        return true;
                    }
                }
            }
        }

        false
    }

    async fn write_synthetic_plan_rejection(&self) -> Result<()> {
        let path = self.config.shared.join("CONTRACT.md");
        if path.exists() {
            return Ok(());
        }
        let content = format!(
            "# Contract\n\n\
             ## Status: ISSUES\n\n\
             ## Feedback\n\n\
             SENTINEL plan review failed after {} attempts.\n\
             FORGE must re-verify the plan against the project requirements.\n\
             Review PLAN.md for completeness and update it before continuing.\n",
            MAX_SENTINEL_RETRIES
        );
        tokio::fs::write(&path, &content)
            .await
            .context("Failed to write synthetic CONTRACT.md")?;
        info!(path = %path.display(), "Wrote synthetic CONTRACT.md with ISSUES verdict");
        Ok(())
    }

    async fn write_synthetic_segment_rejection(&self, segment: u32) -> Result<()> {
        let path = self
            .config
            .shared
            .join(format!("segment-{}-eval.md", segment));
        if path.exists() {
            return Ok(());
        }
        let content = format!(
            "# Segment {} Evaluation\n\n\
             ## Verdict: CHANGES_REQUESTED\n\n\
             ## Specific feedback\n\n\
             SENTINEL evaluation failed after {} attempts.\n\
             FORGE must re-verify this segment's implementation locally before resubmitting.\n\
             Run all tests and lint checks for this segment's code before updating WORKLOG.md.\n\
             Check .github/workflows/ for CI commands and run them locally to confirm everything passes.\n",
            segment, MAX_SENTINEL_RETRIES
        );
        tokio::fs::write(&path, &content)
            .await
            .context("Failed to write synthetic segment eval")?;
        info!(path = %path.display(), "Wrote synthetic segment-{}-eval.md with CHANGES_REQUESTED", segment);
        Ok(())
    }

    async fn write_synthetic_final_rejection(&self) -> Result<()> {
        let path = self.config.shared.join("final-review.md");
        if path.exists() {
            return Ok(());
        }
        let content = format!(
            "# Final Review\n\n\
             ## Verdict: REJECTED\n\n\
             ## Specific feedback\n\n\
             SENTINEL final review failed after {} attempts.\n\
             FORGE must re-verify all segments locally before requesting final review again.\n\
             Run the full test suite and all CI checks locally (see .github/workflows/) before proceeding.\n",
            MAX_SENTINEL_RETRIES
        );
        tokio::fs::write(&path, &content)
            .await
            .context("Failed to write synthetic final-review.md")?;
        info!(path = %path.display(), "Wrote synthetic final-review.md with REJECTED verdict");
        Ok(())
    }

    async fn append_sentinel_failure_to_handoff(&mut self) -> Result<()> {
        let handoff_path = self.config.shared.join("HANDOFF.md");
        if !handoff_path.exists() {
            return Ok(());
        }
        let Some(ref failure) = self.last_sentinel_failure else {
            return Ok(());
        };
        let mode_str = match &failure.mode {
            SentinelMode::PlanReview => "PlanReview".to_string(),
            SentinelMode::SegmentEval(n) => format!("SegmentEval({})", n),
            SentinelMode::FinalReview => "FinalReview".to_string(),
        };
        let section = format!(
            "\n\n## Last Sentinel Failure\n\n\
             Mode: {}\n\
             Reason: {}\n",
            mode_str, failure.reason,
        );
        let existing = tokio::fs::read_to_string(&handoff_path)
            .await
            .unwrap_or_default();
        if existing.contains("## Last Sentinel Failure") {
            return Ok(());
        }
        tokio::fs::write(&handoff_path, format!("{}{}", existing, section))
            .await
            .context("Failed to append sentinel failure to HANDOFF.md")?;
        info!("Appended sentinel failure diagnostics to HANDOFF.md");
        self.last_sentinel_failure = None;
        Ok(())
    }

    async fn materialize_sentinel_artifact(&self, mode: &SentinelMode) -> Result<()> {
        match mode {
            SentinelMode::PlanReview => {
                let output_path = self.config.shared.join("CONTRACT.md");
                if output_path.exists() {
                    return Ok(());
                }

                if let Some(content) = self.read_sentinel_result_payload(mode).await? {
                    tokio::fs::write(&output_path, content)
                        .await
                        .context("Failed to write CONTRACT.md from SENTINEL stdout")?;
                    info!(path = %output_path.display(), "Materialized CONTRACT.md from SENTINEL stdout");
                }
            }
            SentinelMode::SegmentEval(segment) => {
                let output_path = self
                    .config
                    .shared
                    .join(format!("segment-{}-eval.md", segment));
                if output_path.exists() {
                    return Ok(());
                }

                if let Some(content) = self.read_sentinel_result_payload(mode).await? {
                    tokio::fs::write(&output_path, content)
                        .await
                        .context("Failed to write segment eval from SENTINEL stdout")?;
                    info!(path = %output_path.display(), "Materialized segment eval from SENTINEL stdout");
                }
            }
            SentinelMode::FinalReview => {
                let output_path = self.config.shared.join("final-review.md");
                if output_path.exists() {
                    return Ok(());
                }

                if let Some(content) = self.read_sentinel_result_payload(mode).await? {
                    tokio::fs::write(&output_path, content)
                        .await
                        .context("Failed to write final-review.md from SENTINEL stdout")?;
                    info!(path = %output_path.display(), "Materialized final-review.md from SENTINEL stdout");
                }
            }
        }

        Ok(())
    }

    async fn read_sentinel_stderr_excerpt(&self, mode: &SentinelMode) -> Option<String> {
        let mode_str = match mode {
            SentinelMode::PlanReview => "PlanReview".to_string(),
            SentinelMode::SegmentEval(n) => format!("SegmentEval({})", n),
            SentinelMode::FinalReview => "FinalReview".to_string(),
        };
        let log_path = self
            .config
            .shared
            .join("logs")
            .join(format!("sentinel-{}-stderr.log", mode_str));

        if !log_path.exists() {
            return None;
        }

        let content = match tokio::fs::read_to_string(&log_path).await {
            Ok(c) => c,
            Err(_) => return None,
        };

        if content.trim().is_empty() {
            return None;
        }

        const MAX_EXCERPT_LEN: usize = 500;
        if content.len() <= MAX_EXCERPT_LEN {
            Some(content.trim().to_string())
        } else {
            Some(format!(
                "...{}",
                &content[content.len() - MAX_EXCERPT_LEN..].trim()
            ))
        }
    }

    async fn read_sentinel_result_payload(&self, mode: &SentinelMode) -> Result<Option<String>> {
        let log_path = self
            .config
            .shared
            .join("logs")
            .join(format!("sentinel-{:?}-stdout.log", mode));

        if !log_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&log_path)
            .await
            .context("Failed to read SENTINEL stdout log")?;

        if content.trim().is_empty() {
            return Ok(None);
        }

        let last_line = content.lines().rev().find(|line| !line.trim().is_empty());

        let Some(last_line) = last_line else {
            return Ok(None);
        };

        let value: Value = match serde_json::from_str(last_line) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    mode = ?mode,
                    error = %e,
                    "Failed to parse SENTINEL stdout JSON - SENTINEL may not have produced structured output"
                );
                return Ok(None);
            }
        };
        let result_text = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if result_text.is_empty() {
            return Ok(None);
        }

        Ok(Self::extract_result_block(result_text).or_else(|| Some(result_text.to_string())))
    }

    fn extract_result_block(result_text: &str) -> Option<String> {
        let start = result_text.find("<result>")?;
        let end = result_text.rfind("</result>")?;
        let inner = &result_text[start + "<result>".len()..end];
        Some(inner.trim().to_string())
    }

    fn implementation_segments_from_plan(content: &str) -> Vec<u32> {
        let mut segments = Vec::new();
        let mut current_segment: Option<u32> = None;
        let mut current_has_files = false;
        let mut current_has_commands = false;

        let finish_segment =
            |segment: Option<u32>, has_files: bool, has_commands: bool, out: &mut Vec<u32>| {
                if let Some(n) = segment {
                    // Command-only verification steps should not block final review.
                    if !has_commands || has_files {
                        out.push(n);
                    }
                }
            };

        for line in content.lines() {
            if line.starts_with("## Segment") || line.starts_with("### Segment") {
                finish_segment(
                    current_segment.take(),
                    current_has_files,
                    current_has_commands,
                    &mut segments,
                );

                current_segment = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|s| s.trim_end_matches(':').parse::<u32>().ok());
                current_has_files = false;
                current_has_commands = false;
                continue;
            }

            let trimmed = line.trim();
            if trimmed.starts_with("**Files**") || trimmed.starts_with("**File**") {
                current_has_files = true;
            }
            if trimmed.starts_with("**Commands**") || trimmed.starts_with("**Command**") {
                current_has_commands = true;
            }
        }

        finish_segment(
            current_segment,
            current_has_files,
            current_has_commands,
            &mut segments,
        );

        segments
    }

    async fn archive_non_terminal_status(&self, status: &str) -> Result<()> {
        let path = self.config.shared.join("STATUS.json");
        if !path.exists() {
            return Ok(());
        }

        let status_slug: String = status
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let archived_path = self
            .config
            .shared
            .join(format!("STATUS.{}.{}.json", status_slug, nonce));

        tokio::fs::rename(&path, &archived_path)
            .await
            .with_context(|| format!("Failed to archive non-terminal STATUS.json ({status})"))?;

        debug!(
            status,
            archived_to = %archived_path.display(),
            "Archived non-terminal STATUS.json so the event loop can continue"
        );
        Ok(())
    }

    /// Cleanup after pair completion.
    async fn cleanup(&self, _forge: &Child) -> Result<()> {
        self.locks.release_all_for_pair(&self.config.pair_id)?;

        let conflict_path = self.config.shared.join("CONFLICT_RESOLUTION.md");
        if conflict_path.exists() {
            let _ = tokio::fs::remove_file(&conflict_path).await;
            debug!("Removed CONFLICT_RESOLUTION.md after pair completion");
        }

        let ci_fix_path = self.config.shared.join("CI_FIX.md");
        if ci_fix_path.exists() {
            let _ = tokio::fs::remove_file(&ci_fix_path).await;
            debug!("Removed CI_FIX.md after pair completion");
        }

        let error_feedback_path = self.config.shared.join("ERROR_FEEDBACK.md");
        if error_feedback_path.exists() {
            let _ = tokio::fs::remove_file(&error_feedback_path).await;
            debug!("Removed ERROR_FEEDBACK.md in cleanup (safety net)");
        }

        // Note: error_history.json is intentionally NOT removed during cleanup.
        // It persists across pair lifecycles for the same ticket and is only
        // removed when the ticket is completed or marked Exhausted.

        info!(pair = %self.config.pair_id, "Cleanup complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_pair_config_creation() {
        let config = PairConfig::new(
            "pair-1",
            "T-1",
            std::path::Path::new("/project"),
            "ghp_test",
        );

        assert_eq!(config.pair_id, "pair-1");
        assert!(config.worktree.starts_with("/project/worktrees/"));
        assert!(config.shared.ends_with("worktrees/pair-1/.pair-shared"));
        assert!(config.redis_url.is_none());
    }

    #[test]
    fn test_pair_config_with_redis() {
        let config = PairConfig::with_redis(
            "pair-1",
            "T-1",
            std::path::Path::new("/project"),
            "redis://localhost",
            "ghp_test",
        );

        assert_eq!(config.pair_id, "pair-1");
        assert!(config.redis_url.is_some());
        assert_eq!(config.redis_url.as_deref(), Some("redis://localhost"));
    }

    #[test]
    fn test_extract_result_block() {
        let text = "<result>\nstatus: AGREED\nsummary: ok\n</result>";
        let extracted = ForgeSentinelPair::extract_result_block(text).unwrap();
        assert_eq!(extracted, "status: AGREED\nsummary: ok");
    }

    #[test]
    fn test_implementation_segments_ignore_verification_only_segments() {
        let plan = "\
## Segments

### Segment 1: Update store
**Files**: `frontend/src/store.ts`

### Segment 2: Add tests
**Files**: `frontend/tests/store.test.ts`

### Segment 3: Verify all checks pass
**Commands**:
1. `npm run typecheck`
2. `npm run lint`
3. `npm test`
";

        assert_eq!(
            ForgeSentinelPair::implementation_segments_from_plan(plan),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn test_read_sentinel_result_payload_from_stdout_log() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        let pair = ForgeSentinelPair::new(config.clone());

        let logs_dir = config.shared.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(
            logs_dir.join("sentinel-PlanReview-stdout.log"),
            "{\"result\":\"<result>\\nstatus: AGREED\\nsummary: ok\\n</result>\"}\n",
        )
        .unwrap();

        let payload = pair
            .read_sentinel_result_payload(&SentinelMode::PlanReview)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(payload, "status: AGREED\nsummary: ok");
    }

    #[tokio::test]
    async fn test_read_status_archives_approved_ready_as_non_terminal() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        std::fs::create_dir_all(&config.shared).unwrap();
        std::fs::write(
            config.shared.join("STATUS.json"),
            r#"{"status":"APPROVED_READY","task_id":"T-1"}"#,
        )
        .unwrap();

        let mut pair = ForgeSentinelPair::new(config.clone());
        let outcome = pair.read_status().await.unwrap();

        assert!(outcome.is_none());
        assert!(!config.shared.join("STATUS.json").exists());

        let archived = std::fs::read_dir(&config.shared)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("STATUS.approved_ready.")
            });
        assert!(archived);
    }

    #[tokio::test]
    async fn test_read_status_unknown_status_blocks_instead_of_fuel_exhausted() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        std::fs::create_dir_all(&config.shared).unwrap();
        std::fs::write(
            config.shared.join("STATUS.json"),
            r#"{"status":"MYSTERY_STATUS","task_id":"T-1"}"#,
        )
        .unwrap();

        let mut pair = ForgeSentinelPair::new(config);
        let outcome = pair.read_status().await.unwrap().unwrap();

        assert!(matches!(
            outcome,
            PairOutcome::Blocked { ref reason, .. } if reason.contains("MYSTERY_STATUS")
        ));
    }

    #[test]
    fn test_normalize_status_done_variants() {
        assert_eq!(normalize_status("DONE"), "COMPLETE");
        assert_eq!(normalize_status("done"), "COMPLETE");
        assert_eq!(normalize_status("Finished"), "COMPLETE");
        assert_eq!(normalize_status("SUCCESS"), "COMPLETE");
        assert_eq!(normalize_status("READY"), "COMPLETE");
        assert_eq!(normalize_status("WORK_COMPLETE"), "COMPLETE");
        assert_eq!(normalize_status("IMPLEMENTED"), "COMPLETE");
        assert_eq!(normalize_status("TASK_COMPLETE"), "COMPLETE");
    }

    #[test]
    fn test_normalize_status_pr_variants() {
        assert_eq!(normalize_status("PR_CREATED"), "PR_OPENED");
        assert_eq!(normalize_status("PR_SUBMITTED"), "PR_OPENED");
        assert_eq!(normalize_status("PR_OPEN"), "PR_OPENED");
    }

    #[test]
    fn test_normalize_status_blocked_variants() {
        assert_eq!(normalize_status("FAILED"), "BLOCKED");
        assert_eq!(normalize_status("ERROR"), "BLOCKED");
        assert_eq!(normalize_status("STUCK"), "BLOCKED");
        assert_eq!(normalize_status("ABORTED"), "BLOCKED");
    }

    #[test]
    fn test_normalize_status_pending_variants() {
        assert_eq!(normalize_status("PAUSED"), "PENDING_REVIEW");
        assert_eq!(normalize_status("NEEDS_REVIEW"), "PENDING_REVIEW");
        assert_eq!(normalize_status("AWAITING_REVIEW"), "PENDING_REVIEW");
        assert_eq!(normalize_status("PARTIAL"), "PENDING_REVIEW");
    }

    #[test]
    fn test_normalize_status_segment_variants() {
        assert_eq!(normalize_status("SEGMENT_1_DONE"), "SEGMENT_1_DONE");
        assert_eq!(normalize_status("segment_2_complete"), "SEGMENT_2_COMPLETE");
        assert_eq!(normalize_status("Segment_3_Finished"), "SEGMENT_3_FINISHED");
    }

    #[test]
    fn test_normalize_status_canonical_passthrough() {
        assert_eq!(normalize_status("PR_OPENED"), "PR_OPENED");
        assert_eq!(normalize_status("COMPLETE"), "COMPLETE");
        assert_eq!(normalize_status("BLOCKED"), "BLOCKED");
        assert_eq!(normalize_status("FUEL_EXHAUSTED"), "FUEL_EXHAUSTED");
    }

    #[test]
    fn test_normalize_status_truly_unknown() {
        assert_eq!(normalize_status("MYSTERY_STATUS"), "MYSTERY_STATUS");
        assert_eq!(normalize_status("GIBBERISH"), "GIBBERISH");
    }

    #[test]
    fn test_normalize_status_keyword_fuzzy_matching() {
        assert_eq!(normalize_status("AWAITING_REVIEW"), "PENDING_REVIEW");
        assert_eq!(normalize_status("REVIEW_PENDING"), "PENDING_REVIEW");
        assert_eq!(normalize_status("WAITING_FOR_APPROVAL"), "PENDING_REVIEW");
        assert_eq!(normalize_status("ON_HOLD"), "PENDING_REVIEW");
        assert_eq!(
            normalize_status("IMPLEMENTATION_COMPLETE"),
            "IMPLEMENTATION_COMPLETE"
        ); // canonical passthrough
        assert_eq!(normalize_status("WORK_FINISHED"), "COMPLETE");
        assert_eq!(normalize_status("BUILD_FAILED"), "BLOCKED");
        assert_eq!(normalize_status("PR_OPEN_PENDING"), "PR_OPENED");
        assert_eq!(normalize_status("BUDGET_EXCEEDED"), "FUEL_EXHAUSTED");
        assert_eq!(
            normalize_status("SENTINEL_REVIEW_NEEDED"),
            "AWAITING_SENTINEL_REVIEW"
        );
        assert_eq!(
            normalize_status("SEGMENT_IN_PROGRESS"),
            "SEGMENT_IN_PROGRESS"
        );
        assert_eq!(normalize_status("CODE_REVIEW_COMPLETE"), "COMPLETE"); // "COMPLETE" overrides "REVIEW"
        assert_eq!(normalize_status("TASK_SUCCESSFUL"), "COMPLETE");
    }

    #[tokio::test]
    async fn test_read_status_done_with_pr_maps_to_pr_opened() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        std::fs::create_dir_all(&config.shared).unwrap();
        std::fs::write(
            config.shared.join("STATUS.json"),
            r#"{"status":"DONE","task_id":"T-1","pr_url":"https://github.com/o/r/pull/5","pr_number":5}"#,
        )
        .unwrap();

        let mut pair = ForgeSentinelPair::new(config);
        let outcome = pair.read_status().await.unwrap().unwrap();

        assert!(matches!(
            outcome,
            PairOutcome::PrOpened { pr_number: 5, .. }
        ));
    }

    #[tokio::test]
    async fn test_read_status_done_without_pr_maps_to_blocked() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        std::fs::create_dir_all(&config.shared).unwrap();
        std::fs::write(
            config.shared.join("STATUS.json"),
            r#"{"status":"DONE","task_id":"T-1"}"#,
        )
        .unwrap();

        let mut pair = ForgeSentinelPair::new(config);
        let outcome = pair.read_status().await.unwrap().unwrap();

        assert!(matches!(
            outcome,
            PairOutcome::Blocked { ref reason, .. } if reason.contains("PR not created")
        ));
    }

    #[test]
    fn test_parse_timeout_profile_medium() {
        let content = "\
status: AGREED
summary: Add authentication module
definition_of_done:
- Auth middleware implemented
- Tests passing
objections:
- None
timeout_profile:
  plan_review_secs: 120
  segment_eval_secs: 300
  final_review_secs: 480
  complexity: medium";
        let profile = ForgeSentinelPair::parse_timeout_profile(content).unwrap();
        assert_eq!(profile.plan_review_secs, 120);
        assert_eq!(profile.segment_eval_secs, 300);
        assert_eq!(profile.final_review_secs, 480);
        assert_eq!(profile.complexity, Complexity::Medium);
    }

    #[test]
    fn test_parse_timeout_profile_high() {
        let content = "\
status: AGREED
summary: Refactor API layer
timeout_profile:
  plan_review_secs: 180
  segment_eval_secs: 480
  final_review_secs: 720
  complexity: high";
        let profile = ForgeSentinelPair::parse_timeout_profile(content).unwrap();
        assert_eq!(profile.plan_review_secs, 180);
        assert_eq!(profile.segment_eval_secs, 480);
        assert_eq!(profile.final_review_secs, 720);
        assert_eq!(profile.complexity, Complexity::High);
    }

    #[test]
    fn test_parse_timeout_profile_missing() {
        let content = "status: AGREED\nsummary: Simple fix";
        assert!(ForgeSentinelPair::parse_timeout_profile(content).is_none());
    }

    #[test]
    fn test_parse_timeout_profile_partial() {
        let content = "\
status: AGREED
timeout_profile:
  plan_review_secs: 90
  complexity: low";
        assert!(ForgeSentinelPair::parse_timeout_profile(content).is_none());
    }

    #[test]
    fn test_compute_effective_timeout_low() {
        let timeout = compute_effective_timeout(90, &Complexity::Low);
        let expected = 90 + 15 + 10 + 20 + 15; // base + network + streaming + buffer + build_low
        assert_eq!(timeout, expected);
    }

    #[test]
    fn test_compute_effective_timeout_high() {
        let timeout = compute_effective_timeout(480, &Complexity::High);
        let expected = 480 + 15 + 10 + 20 + 60; // base + network + streaming + buffer + build_high
        assert_eq!(timeout, expected);
    }

    #[test]
    fn test_resolve_sentinel_timeout_with_profile() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        let mut pair = ForgeSentinelPair::new(config);
        pair.contract_timeout = Some(TimeoutProfile {
            plan_review_secs: 180,
            segment_eval_secs: 480,
            final_review_secs: 720,
            complexity: Complexity::High,
        });

        let pr_timeout = pair.resolve_sentinel_timeout(&SentinelMode::PlanReview);
        let se_timeout = pair.resolve_sentinel_timeout(&SentinelMode::SegmentEval(1));
        let fr_timeout = pair.resolve_sentinel_timeout(&SentinelMode::FinalReview);

        assert!(pr_timeout > 180);
        assert!(se_timeout > 480);
        assert!(fr_timeout > 720);
    }

    #[test]
    fn test_resolve_sentinel_timeout_fallback() {
        let dir = tempdir().unwrap();
        let config = PairConfig::new("pair-1", "T-1", dir.path(), "ghp_test");
        let pair = ForgeSentinelPair::new(config);

        let timeout = pair.resolve_sentinel_timeout(&SentinelMode::PlanReview);
        assert!(timeout > DEFAULT_SENTINEL_TIMEOUT_SECS);
    }
}
