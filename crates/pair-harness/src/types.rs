// crates/pair-harness/src/types.rs
//! Core types for the pair-harness system.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Re-export CliBackend from the config crate — single source of truth.
pub use config::registry::CliBackend;

/// Filesystem events detected by the watcher.
/// These drive the event-driven harness state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    /// FORGE submitted a segment (WORKLOG.md modified)
    WorklogUpdated,
    /// FORGE finished planning (PLAN.md created)
    PlanWritten,
    /// SENTINEL reviewed plan (CONTRACT.md created)
    ContractWritten,
    /// SENTINEL finished segment-N evaluation
    SegmentEvalWritten(u32),
    /// SENTINEL approved all segments (final-review.md created)
    FinalReviewWritten,
    /// Terminal signal (PR_OPENED, BLOCKED, FUEL_EXHAUSTED)
    StatusJsonWritten,
    /// Context reset requested (HANDOFF.md created)
    HandoffWritten,
}

/// Ticket information for assignment to a pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    /// Ticket identifier (e.g., "T-42")
    pub id: String,
    /// GitHub issue number
    pub issue_number: u64,
    /// Ticket title
    pub title: String,
    /// Ticket description/body
    pub body: String,
    /// GitHub issue URL
    pub url: String,
    /// Files that will be touched (for initial locking)
    pub touched_files: Vec<String>,
    /// Acceptance criteria extracted from the issue
    pub acceptance_criteria: Vec<String>,
}

/// Configuration for a pair slot.
#[derive(Debug, Clone)]
pub struct PairConfig {
    pub pair_id: String,
    pub ticket_id: String,
    pub project_root: PathBuf,
    pub worktree: PathBuf,
    pub shared: PathBuf,
    pub redis_url: Option<String>,
    pub proxy_url: Option<String>,
    pub github_token: String,
    pub max_resets: u32,
    pub watchdog_timeout_secs: u64,
    /// CLI backend to use for this pair (claude or codex)
    pub cli_backend: CliBackend,
    pub verify_command: Option<String>,
    pub max_verify_attempts: u32,
}

impl PairConfig {
    /// Name of the shared orchestration directory inside each worktree.
    ///
    /// Placing the shared directory inside the worktree (rather than in
    /// `orchestration/pairs/`) is required for the Codex `workspace-write`
    /// sandbox: the sandbox only mounts the workspace root as writable, and
    /// the `--add-dir` flag has a known bug (Codex v0.130.0) where it
    /// reports the path as writable in the banner but does NOT create the
    /// corresponding bind mount in the sandbox namespace.  By locating the
    /// shared directory inside the worktree, it falls under the existing
    /// writable bind mount and no `--add-dir` workaround is needed.
    pub const SHARED_DIR_NAME: &'static str = ".pair-shared";

    fn shared_path(project_root: &std::path::Path, pair_id: &str, _ticket_id: &str) -> PathBuf {
        project_root
            .join("worktrees")
            .join(pair_id)
            .join(Self::SHARED_DIR_NAME)
    }

    /// Create a new pair configuration with filesystem-based state.
    pub fn new(
        pair_id: impl Into<String>,
        ticket_id: impl Into<String>,
        project_root: &std::path::Path,
        github_token: impl Into<String>,
    ) -> Self {
        let pair_id = pair_id.into();
        let ticket_id = ticket_id.into();
        Self {
            project_root: project_root.to_path_buf(),
            worktree: project_root.join("worktrees").join(&pair_id),
            shared: Self::shared_path(project_root, &pair_id, &ticket_id),
            pair_id,
            ticket_id,
            redis_url: None,
            proxy_url: None,
            github_token: github_token.into(),
            max_resets: 10,
            watchdog_timeout_secs: 3600, // 1 hour - must be > SENTINEL timeout
            cli_backend: CliBackend::default(),
            verify_command: None,
            max_verify_attempts: 3,
        }
    }

    /// Create a pair configuration with Redis backend.
    pub fn with_redis(
        pair_id: impl Into<String>,
        ticket_id: impl Into<String>,
        project_root: &std::path::Path,
        redis_url: impl Into<String>,
        github_token: impl Into<String>,
    ) -> Self {
        let pair_id = pair_id.into();
        let ticket_id = ticket_id.into();
        Self {
            project_root: project_root.to_path_buf(),
            worktree: project_root.join("worktrees").join(&pair_id),
            shared: Self::shared_path(project_root, &pair_id, &ticket_id),
            pair_id,
            ticket_id,
            redis_url: Some(redis_url.into()),
            proxy_url: None,
            github_token: github_token.into(),
            max_resets: 10,
            watchdog_timeout_secs: 3600, // 1 hour - must be > SENTINEL timeout
            cli_backend: CliBackend::default(),
            verify_command: None,
            max_verify_attempts: 3,
        }
    }

    pub fn with_proxy(
        pair_id: impl Into<String>,
        ticket_id: impl Into<String>,
        project_root: &std::path::Path,
        redis_url: Option<String>,
        proxy_url: impl Into<String>,
        github_token: impl Into<String>,
    ) -> Self {
        let pair_id = pair_id.into();
        let ticket_id = ticket_id.into();
        Self {
            project_root: project_root.to_path_buf(),
            worktree: project_root.join("worktrees").join(&pair_id),
            shared: Self::shared_path(project_root, &pair_id, &ticket_id),
            pair_id,
            ticket_id,
            redis_url,
            proxy_url: Some(proxy_url.into()),
            github_token: github_token.into(),
            max_resets: 10,
            watchdog_timeout_secs: 3600, // 1 hour - must be > SENTINEL timeout
            cli_backend: CliBackend::default(),
            verify_command: None,
            max_verify_attempts: 3,
        }
    }

    /// Set the CLI backend for this pair.
    pub fn with_cli_backend(mut self, backend: CliBackend) -> Self {
        self.cli_backend = backend;
        self
    }
}

/// Outcome of a pair's work on a ticket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PairOutcome {
    /// PR was opened successfully
    PrOpened {
        pr_url: String,
        pr_number: u64,
        branch: String,
    },
    /// Pair is blocked (needs human intervention)
    Blocked {
        reason: String,
        blockers: Vec<Blocker>,
    },
    /// Fuel exhausted (too many context resets or timeout)
    FuelExhausted { reason: String, reset_count: u32 },
}

/// A blocker preventing progress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Blocker {
    /// Type of blocker
    #[serde(rename = "type")]
    pub blocker_type: String,
    /// Human-readable description
    pub description: String,
    /// Suggested action for NEXUS
    pub nexus_action: String,
}

/// Files changed - can be either a count (integer) or a list of paths.
/// FORGE may write either format depending on the skill version.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum FilesChanged {
    #[default]
    Unknown,
    Count(u64),
    List(Vec<String>),
}

impl FilesChanged {
    pub fn is_empty(&self) -> bool {
        match self {
            FilesChanged::Unknown => true,
            FilesChanged::Count(c) => *c == 0,
            FilesChanged::List(v) => v.is_empty(),
        }
    }

    pub fn to_list(&self) -> Vec<String> {
        match self {
            FilesChanged::Unknown => vec![],
            FilesChanged::Count(_) => vec![],
            FilesChanged::List(v) => v.clone(),
        }
    }
}

/// Segments completed - can be either a count or a list of segment details.
/// FORGE may write either format depending on the skill version.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum SegmentsCompleted {
    #[default]
    None,
    Count(u32),
    List(Vec<SegmentEntry>),
}

impl SegmentsCompleted {
    pub fn count(&self) -> u32 {
        match self {
            SegmentsCompleted::None => 0,
            SegmentsCompleted::Count(c) => *c,
            SegmentsCompleted::List(v) => v.len() as u32,
        }
    }
}

/// A single segment entry in STATUS.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentEntry {
    pub segment: u32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, rename = "eval_file")]
    pub eval_file: Option<String>,
}

/// Status written to STATUS.json by FORGE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusJson {
    /// Current status — LLMs may write either "status" or "outcome" as the key.
    /// The `status` field is preferred; `outcome` is a fallback when `status` is absent.
    #[serde(default)]
    pub status: String,
    /// Outcome status — some LLMs write "outcome" instead of "status".
    /// Used as fallback when `status` is empty or absent.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Pair identifier (optional - may not be present in all STATUS.json formats)
    #[serde(default)]
    pub pair: Option<String>,
    /// Ticket identifier - can be "ticket" or "ticket_id" in STATUS.json
    /// FORGE may omit this field; we fall back to the pair's known ticket_id.
    #[serde(alias = "ticket", alias = "task_id", default)]
    pub ticket_id: Option<String>,
    /// PR URL (if PR_OPENED)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// PR number (if PR_OPENED)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// Branch name (optional - may not be present in all STATUS.json formats)
    #[serde(default)]
    pub branch: Option<String>,
    /// Files changed (can be count or list)
    #[serde(default)]
    pub files_changed: FilesChanged,
    /// Test results (optional)
    #[serde(default)]
    pub test_results: Option<TestResults>,
    /// Segments completed - can be a count or a detailed list.
    /// FORGE may write either format depending on the skill version.
    #[serde(default)]
    pub segments_completed: SegmentsCompleted,
    /// Number of context resets (optional)
    #[serde(default)]
    pub context_resets: u32,
    /// Whether SENTINEL approved (optional)
    #[serde(default)]
    pub sentinel_approved: bool,
    /// Active blockers (if BLOCKED)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub blockers: Vec<Blocker>,
    /// Elapsed time in milliseconds (optional)
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Timestamp (optional)
    #[serde(default)]
    pub timestamp: String,
}

impl StatusJson {
    /// Resolve the effective status string.
    ///
    /// LLMs inconsistently write either `status` or `outcome` as the top-level key.
    /// This method returns `status` if present and non-empty, otherwise falls back
    /// to `outcome`, ensuring both field name variants are handled.
    pub fn effective_status(&self) -> &str {
        if !self.status.is_empty() {
            &self.status
        } else {
            self.outcome.as_deref().unwrap_or("")
        }
    }
}

/// Test results summary.
/// FORGE may write structured counts or arbitrary key-value data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResults {
    #[serde(default)]
    pub passed: u32,
    #[serde(default)]
    pub failed: u32,
    #[serde(default)]
    pub skipped: u32,
    /// Catch-all for arbitrary test result data written by FORGE.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Complexity level for timeout estimation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Complexity {
    #[default]
    Low,
    Medium,
    High,
}

/// Per-mode timeout profile written by SENTINEL during plan review.
/// The harness reads these values and applies environmental overhead
/// (network delay, streaming latency, build/test startup) on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutProfile {
    /// Timeout for plan review (lightweight read/write evaluation)
    pub plan_review_secs: u64,
    /// Timeout for a single segment evaluation (may involve test runs, linting)
    pub segment_eval_secs: u64,
    /// Timeout for final review (full test suite, all criteria)
    pub final_review_secs: u64,
    /// Estimated complexity of the issue
    pub complexity: Complexity,
}

impl Default for TimeoutProfile {
    fn default() -> Self {
        Self {
            plan_review_secs: 120,
            segment_eval_secs: 300,
            final_review_secs: 480,
            complexity: Complexity::Medium,
        }
    }
}

/// Contract status written by SENTINEL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    /// Status: AGREED or ISSUES
    pub status: String,
    /// Contract terms (definition of done)
    pub terms: Vec<ContractTerm>,
    /// Objections (if status is ISSUES)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub objections: Vec<String>,
    /// Timeout profile estimated by SENTINEL based on issue complexity
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub timeout_profile: Option<TimeoutProfile>,
}

/// A single contract term.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractTerm {
    pub criterion: String,
    pub verification: String,
}

/// Segment evaluation written by SENTINEL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentEval {
    /// Segment number
    pub segment: u32,
    /// Verdict: APPROVED or CHANGES_REQUESTED
    pub verdict: String,
    /// Specific feedback items (if CHANGES_REQUESTED)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub feedback: Vec<FeedbackItem>,
}

/// A specific feedback item for changes requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackItem {
    pub file: String,
    pub line: u32,
    pub problem: String,
    pub fix: String,
}

/// Final review written by SENTINEL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalReview {
    /// Verdict: APPROVED or REJECTED
    pub verdict: String,
    /// PR description (if APPROVED)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_description: Option<String>,
    /// Remaining issues (if REJECTED)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub issues: Vec<String>,
}

/// File lock metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLock {
    /// Pair that owns the lock
    pub pair: String,
    /// File path (relative to project root)
    pub file: String,
    /// When the lock was acquired
    pub acquired_at: String,
}

impl FileLock {
    /// Create a new file lock for a pair.
    pub fn new(pair: impl Into<String>, file: impl Into<String>) -> Self {
        Self {
            pair: pair.into(),
            file: file.into(),
            acquired_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Result of post-completion verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationResult {
    /// Verification passed — accept FORGE completion.
    Passed,
    /// Verification failed — feed error back to FORGE for self-repair.
    Failed { output: String, command: String },
    /// Verification skipped (no verify_command configured).
    Skipped,
}

/// Tracks verification attempt state across the pair lifecycle.
#[derive(Debug, Clone)]
pub struct VerificationState {
    pub attempt: u32,
    pub max_attempts: u32,
}

impl VerificationState {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            attempt: 0,
            max_attempts,
        }
    }
}

/// Persistent error history across self-repair attempts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorHistory {
    pub entries: Vec<ErrorHistoryEntry>,
}

/// A single entry in the error history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorHistoryEntry {
    pub timestamp: String,
    pub source: String,
    pub error_type: String,
    pub message: String,
    pub resolution_attempted: Option<String>,
    pub resolved: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_files_changed_count() {
        let json = r#"{
            "ticket_id": "T-1",
            "status": "IMPLEMENTATION_COMPLETE",
            "branch": "forge-1/T-1",
            "files_changed": 14
        }"#;

        let status: StatusJson = serde_json::from_str(json).expect("Failed to parse");
        assert_eq!(status.ticket_id, Some("T-1".to_string()));
        assert_eq!(status.status, "IMPLEMENTATION_COMPLETE");
        match status.files_changed {
            FilesChanged::Count(n) => assert_eq!(n, 14),
            _ => panic!("Expected Count variant, got {:?}", status.files_changed),
        }
    }

    #[test]
    fn test_files_changed_list() {
        let json = r#"{
            "ticket_id": "T-2",
            "status": "PR_OPENED",
            "branch": "forge-1/T-2",
            "files_changed": ["src/main.rs", "src/lib.rs"]
        }"#;

        let status: StatusJson = serde_json::from_str(json).expect("Failed to parse");
        assert_eq!(status.ticket_id, Some("T-2".to_string()));
        match status.files_changed {
            FilesChanged::List(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected List variant, got {:?}", status.files_changed),
        }
    }

    #[test]
    fn test_files_changed_missing() {
        let json = r#"{
            "ticket_id": "T-3",
            "status": "BLOCKED",
            "branch": "forge-1/T-3"
        }"#;

        let status: StatusJson = serde_json::from_str(json).expect("Failed to parse");
        assert_eq!(status.ticket_id, Some("T-3".to_string()));
        assert!(status.files_changed.is_empty());
    }

    #[test]
    fn test_ticket_id_missing() {
        let json = r#"{
            "status": "COMPLETE",
            "branch": "forge-1/T-005",
            "pr_url": "https://github.com/org/repo/pull/1"
        }"#;

        let status: StatusJson = serde_json::from_str(json).expect("Failed to parse");
        assert_eq!(status.ticket_id, None);
        assert_eq!(status.status, "COMPLETE");
    }

    #[test]
    fn test_status_json_with_arbitrary_test_results() {
        let json = r#"{
            "status": "COMPLETE",
            "ticket": "T-005",
            "branch": "forge-1/T-005",
            "pr_number": 28,
            "pr_url": "https://github.com/The-AgenticFlow/template-counterapp/pull/28",
            "segments_completed": 3,
            "segments_total": 3,
            "definition_of_done": {
                "get_counter": true,
                "increment_counter": true,
                "decrement_counter": true,
                "cors_enabled": true,
                "port_3001": true
            },
            "test_results": {
                "get_counter_initial": {"count": 0},
                "increment_sequence": [{"count": 1}, {"count": 2}, {"count": 3}],
                "decrement_sequence": [{"count": 2}, {"count": 1}, {"count": 0}, {"count": 0}],
                "cors_preflight": "PASSED"
            },
            "completion_date": "2026-04-17"
        }"#;

        let status: StatusJson = serde_json::from_str(json)
            .expect("Failed to parse STATUS.json with arbitrary test_results");
        assert_eq!(status.status, "COMPLETE");
        assert_eq!(status.ticket_id, Some("T-005".to_string()));
        assert_eq!(status.pr_number, Some(28));
        assert_eq!(status.segments_completed.count(), 3);
        let tr = status.test_results.expect("test_results should be present");
        assert_eq!(tr.passed, 0);
        assert_eq!(tr.failed, 0);
        assert!(tr.extra.contains_key("cors_preflight"));
    }

    #[test]
    fn test_segments_completed_list() {
        let json = r#"{
            "status": "READY_FOR_REVIEW",
            "forge_agent": "forge-1",
            "current_segment": "segment-2-and-3",
            "segments_completed": [
                {"segment": 1, "status": "APPROVED", "eval_file": "segment-1-eval.md"},
                {"segment": 2, "status": "COMPLETE", "files": ["src/main.rs"]},
                {"segment": 3, "status": "COMPLETE", "files": ["src/handlers.rs"]}
            ]
        }"#;

        let status: StatusJson = serde_json::from_str(json)
            .expect("Failed to parse STATUS.json with segments_completed list");
        assert_eq!(status.segments_completed.count(), 3);
        match &status.segments_completed {
            SegmentsCompleted::List(v) => {
                assert_eq!(v[0].segment, 1);
                assert_eq!(v[0].status, "APPROVED");
                assert_eq!(v[1].files, vec!["src/main.rs".to_string()]);
            }
            other => panic!("Expected List variant, got {:?}", other),
        }
    }

    #[test]
    fn test_status_json_outcome_alias() {
        // LLMs sometimes write "outcome" instead of "status" — must handle both.

        // Case 1: "outcome" only (no "status" field) — the real-world broken case
        let json_outcome_only = r#"{
            "outcome": "blocked",
            "blocker": {"kind": "Other", "description": "Unable to push"}
        }"#;

        let status: StatusJson = serde_json::from_str(json_outcome_only)
            .expect("Failed to parse STATUS.json with 'outcome' instead of 'status'");
        assert_eq!(status.effective_status(), "blocked");

        // Case 2: "status" only — standard format
        let json_status_only = r#"{
            "status": "COMPLETE",
            "pair": "forge-1"
        }"#;

        let status: StatusJson = serde_json::from_str(json_status_only)
            .expect("Failed to parse STATUS.json with standard 'status' field");
        assert_eq!(status.effective_status(), "COMPLETE");

        // Case 3: Both "outcome" and "status" present — "status" takes precedence
        let json_both = r#"{
            "outcome": "REJECTED",
            "status": "COMPLETE"
        }"#;

        let status: StatusJson = serde_json::from_str(json_both)
            .expect("Failed to parse STATUS.json with both outcome and status");
        assert_eq!(status.effective_status(), "COMPLETE");
    }
}
