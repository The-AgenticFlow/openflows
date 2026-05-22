// crates/pair-harness/src/worktree.rs
//! Git worktree management for pair isolation.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::sync::Mutex;
use tracing::{debug, info, warn};

/// Process-wide mutex to serialize git worktree creation across concurrent pairs.
///
/// Git uses lock files (`.git/index.lock`) that prevent concurrent operations on
/// the same repository. When two FORGE workers try to create worktrees simultaneously,
/// the second one fails with a lock contention error. This mutex ensures only one
/// pair creates a worktree at a time.
///
/// Uses `std::sync::Mutex` rather than `tokio::sync::Mutex` because the critical
/// section contains synchronous blocking operations (`Command::output()`,
/// `std::thread::sleep` for retries). A tokio mutex held across blocking calls
/// would starve the async runtime when called from non-`spawn_blocking` contexts.
static GIT_WORKTREE_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Maximum number of retries when git commands fail due to lock contention.
const GIT_LOCK_RETRY_COUNT: u32 = 5;

/// Base delay in milliseconds between retries for git lock contention.
const GIT_LOCK_RETRY_BASE_DELAY_MS: u64 = 200;

/// Manages Git worktrees for pair isolation.
pub struct WorktreeManager {
    /// Project root directory (contains .git)
    project_root: PathBuf,
    /// Directory where worktrees are created
    worktrees_dir: PathBuf,
}

impl WorktreeManager {
    /// Create a new worktree manager.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        Self {
            worktrees_dir: project_root.join("worktrees"),
            project_root,
        }
    }

    /// Configure git user identity in a worktree using the GitHub PAT.
    /// This ensures commits are authored by the PAT identity, not the local git config.
    pub async fn configure_git_identity(
        &self,
        worktree_path: &Path,
        github_token: &str,
    ) -> Result<()> {
        // Use the token to identify the user via GitHub API
        let client = reqwest::Client::new();
        let resp = client
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {}", github_token))
            .header("User-Agent", "AgentFlow-Worktree/0.1")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("Failed to fetch GitHub user info")?;

        let (name, email) = if resp.status().is_success() {
            let user: serde_json::Value = resp
                .json()
                .await
                .context("Failed to parse GitHub user response")?;
            let name = user["name"]
                .as_str()
                .unwrap_or_else(|| user["login"].as_str().unwrap_or("AgentFlow"))
                .to_string();
            let email = user["email"]
                .as_str()
                .map(|e| e.to_string())
                .or_else(|| {
                    user["login"]
                        .as_str()
                        .map(|login| format!("{}@users.noreply.github.com", login))
                })
                .unwrap_or_else(|| "agentflow@github.com".to_string());
            (name, email)
        } else {
            warn!(
                "Failed to fetch GitHub user info (status {}), using generic identity",
                resp.status()
            );
            ("AgentFlow".to_string(), "agentflow@github.com".to_string())
        };

        self.run_git_in_worktree(worktree_path, &["config", "user.name", &name])?;
        self.run_git_in_worktree(worktree_path, &["config", "user.email", &email])?;

        info!(name, email, path = %worktree_path.display(), "Git identity configured from PAT");
        Ok(())
    }

    /// Configure the remote URL with embedded token for push authentication.
    /// This ensures each worktree uses its own token for git operations.
    pub fn configure_remote_with_token(
        &self,
        worktree_path: &Path,
        github_token: &str,
    ) -> Result<()> {
        let output = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to get remote URL")?;

        let current_url = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let new_url = if let Some(repo_part) = current_url.strip_prefix("https://github.com/") {
            let repo_part = repo_part.trim_end_matches(".git").trim_end_matches('/');
            format!(
                "https://x-access-token:{}@github.com/{}.git",
                github_token, repo_part
            )
        } else if let Some(repo_part) =
            current_url.strip_prefix("https://x-access-token:@github.com/")
        {
            let repo_part = repo_part.trim_end_matches(".git").trim_end_matches('/');
            format!(
                "https://x-access-token:{}@github.com/{}.git",
                github_token, repo_part
            )
        } else if current_url.contains("x-access-token:") {
            let re = regex::Regex::new(r"https://x-access-token:[^@]+@github\.com/(.+)").unwrap();
            if let Some(caps) = re.captures(&current_url) {
                let repo_part = caps.get(1).unwrap().as_str();
                let repo_part = repo_part.trim_end_matches(".git").trim_end_matches('/');
                format!(
                    "https://x-access-token:{}@github.com/{}.git",
                    github_token, repo_part
                )
            } else {
                current_url.clone()
            }
        } else {
            current_url.clone()
        };

        if new_url != current_url {
            self.run_git_in_worktree(worktree_path, &["remote", "set-url", "origin", &new_url])?;
            info!(path = %worktree_path.display(), "Remote URL configured with token");
        }

        Ok(())
    }

    fn run_git_in_worktree(&self, worktree_path: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .args(args)
            .current_dir(worktree_path)
            .output()
            .context("Failed to run git command in worktree")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Git command failed in worktree: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    /// Create a worktree for a pair on a new branch.
    ///
    /// Implements worktree reuse: when a pair gets a new ticket, the existing
    /// worktree is reused by fetching origin/main and creating a new branch.
    ///
    /// This method acquires a process-wide mutex to serialize git operations
    /// across concurrent pairs, preventing lock contention on the shared `.git`
    /// directory. When git lock contention is detected, the operation is retried
    /// with exponential backoff.
    ///
    /// # Arguments
    /// * `pair_id` - Pair identifier (e.g., "pair-1", "forge-1")
    /// * `ticket_id` - Ticket identifier (e.g., "T-42")
    /// * `github_token` - GitHub PAT used to configure git author identity
    ///
    /// # Returns
    /// `WorktreeSetupResult` containing the path and any setup warnings.
    pub async fn create_worktree(
        &self,
        pair_id: &str,
        ticket_id: &str,
        github_token: &str,
    ) -> Result<WorktreeSetupResult> {
        // Phase 1: Synchronous git operations under the process-wide mutex.
        // The std::sync::Mutex guard cannot be held across .await points,
        // so all async work (configure_git_identity) must happen after the
        // guard is dropped. We collect the result in a separate scope.
        let (worktree_path, warnings) = {
            let _guard = GIT_WORKTREE_MUTEX
                .lock()
                .map_err(|e| anyhow!("GIT_WORKTREE_MUTEX poisoned: {}", e))?;

            self.create_worktree_sync(pair_id, ticket_id)?
        };

        // Phase 2: Async configuration (identity, remote URL) — outside the mutex
        // since these are HTTP calls that don't touch the .git index.
        if let Err(e) = self
            .configure_git_identity(&worktree_path, github_token)
            .await
        {
            warn!(error = %e, "Failed to configure git identity from PAT, using local git config");
        }

        if let Err(e) = self.configure_remote_with_token(&worktree_path, github_token) {
            warn!(error = %e, "Failed to configure remote URL with token");
        }

        Ok(WorktreeSetupResult {
            path: worktree_path,
            warnings,
        })
    }

    /// Synchronous portion of worktree creation — must be called under GIT_WORKTREE_MUTEX.
    ///
    /// Performs all git commands (fetch, merge, worktree add) and returns the
    /// worktree path and any warnings. Does not perform async operations.
    fn create_worktree_sync(
        &self,
        pair_id: &str,
        ticket_id: &str,
    ) -> Result<(PathBuf, Vec<SetupWarning>)> {
        let worktree_path = self.worktrees_dir.join(pair_id);
        let branch_name = Self::branch_name(pair_id, ticket_id);
        let mut warnings = Vec::new();

        info!(pair_id, ticket_id, branch = %branch_name, "Creating worktree (under global mutex)");

        // Retry git operations that may fail due to transient lock contention
        // even under the mutex (e.g., another process outside our control).
        let fetch_result =
            Self::retry_git_operation(|| self.run_git_in_main(&["fetch", "origin", "main"]));
        if let Err(e) = fetch_result {
            warn!(error = %e, "git fetch origin/main failed, continuing");
            warnings.push(SetupWarning {
                phase: "fetch_origin_main".to_string(),
                error: e.to_string(),
                affected_files: vec![],
            });
        }

        let merge_result =
            Self::retry_git_operation(|| self.run_git_in_main(&["merge", "origin/main"]));
        if let Err(e) = merge_result {
            warn!(error = %e, "git merge origin/main failed, continuing");
            let affected_files = self.list_unmerged_files_in_main();
            warnings.push(SetupWarning {
                phase: "merge_origin_main".to_string(),
                error: e.to_string(),
                affected_files,
            });
        }

        if worktree_path.exists() {
            if let Ok(current) = self.get_current_branch(&worktree_path) {
                if current == branch_name {
                    info!(
                        path = %worktree_path.display(),
                        branch = %branch_name,
                        "Worktree already on correct branch - reusing"
                    );
                    return Ok((worktree_path, warnings));
                }
                // Reuse requires async configure_git_identity which can't happen
                // under the mutex. Return the path; the caller handles identity.
                info!(
                    path = %worktree_path.display(),
                    current = %current,
                    new_branch = %branch_name,
                    "Existing worktree needs reuse for new ticket"
                );
                // For reuse, we still need to do the git operations synchronously
                self.reuse_worktree_sync(&worktree_path, &branch_name)?;
                return Ok((worktree_path, warnings));
            }
            warn!(path = %worktree_path.display(), "Worktree exists but branch unknown, replacing");
            self.remove_worktree_by_path(&worktree_path, "unknown")?;
        }

        self.prune_stale_worktrees();
        self.delete_branch_if_exists(&branch_name);

        std::fs::create_dir_all(&self.worktrees_dir)
            .context("Failed to create worktrees directory")?;

        // Retry worktree add with backoff — lock contention from concurrent
        // git processes (even outside our mutex) can still cause transient failures.
        // The closure handles both lock-retriable errors and the "already exists"
        // fallback (create worktree from existing branch) inline, so after
        // retry_git_operation returns Ok(()) the worktree is guaranteed created.
        Self::retry_git_operation(|| -> Result<()> {
            let o = Command::new("git")
                .args(["worktree", "add"])
                .arg(&worktree_path)
                .args(["-b", &branch_name])
                .current_dir(&self.project_root)
                .output()
                .context("Failed to run git worktree add")?;

            if o.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&o.stderr);

            // Lock contention → retriable
            if Self::is_git_lock_error(&stderr) {
                return Err(anyhow!("git lock contention: {}", stderr));
            }

            // Branch already exists → try creating worktree from existing branch
            if stderr.contains("already exists") {
                info!(branch = %branch_name, "Branch exists, creating worktree from existing branch");
                let o2 = Command::new("git")
                    .args(["worktree", "add"])
                    .arg(&worktree_path)
                    .arg(&branch_name)
                    .current_dir(&self.project_root)
                    .output()
                    .context("Failed to run git worktree add from existing branch")?;

                if o2.status.success() {
                    return Ok(());
                }

                let stderr2 = String::from_utf8_lossy(&o2.stderr);
                if Self::is_git_lock_error(&stderr2) {
                    return Err(anyhow!(
                        "git lock contention on existing-branch add: {}",
                        stderr2
                    ));
                }
                return Err(anyhow!(
                    "Failed to create worktree from existing branch: {}",
                    stderr2
                ));
            }

            // Non-retriable, non-"already exists" error
            Err(anyhow!("git worktree add failed: {}", stderr))
        })?;

        // Check for dirty worktree state
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree_path)
            .output()
            .context("Failed to run git status")?;

        if !status.stdout.is_empty() {
            let dirty_files = String::from_utf8_lossy(&status.stdout)
                .lines()
                .filter_map(|l| l.get(3..).map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            warn!(path = %worktree_path.display(), files = dirty_files.len(), "Worktree is not clean");
            warnings.push(SetupWarning {
                phase: "worktree_dirty".to_string(),
                error: "Worktree has uncommitted changes".to_string(),
                affected_files: dirty_files,
            });
        }

        info!(path = %worktree_path.display(), branch = %branch_name, "Worktree created successfully");
        Ok((worktree_path, warnings))
    }

    /// Synchronous portion of worktree reuse — must be called under GIT_WORKTREE_MUTEX.
    fn reuse_worktree_sync(&self, worktree_path: &Path, new_branch: &str) -> Result<()> {
        self.fetch_and_reset_to_main(worktree_path)?;
        self.create_branch_from_main(worktree_path, new_branch)?;
        info!(
            path = %worktree_path.display(),
            branch = %new_branch,
            "Worktree reused successfully (sync phase)"
        );
        Ok(())
    }

    /// Check if a git stderr indicates lock contention.
    fn is_git_lock_error(stderr: &str) -> bool {
        stderr.contains("index.lock")
            || (stderr.contains("Unable to create") && stderr.contains(".lock"))
            || stderr.contains("fatal: Unable to create")
            || stderr.contains("Another git process seems to be running")
    }

    /// Retry a git operation with exponential backoff when lock contention is detected.
    ///
    /// This handles the case where an external git process (not controlled by our
    /// mutex) holds a lock on the repository. The retry uses exponential backoff
    /// starting at `GIT_LOCK_RETRY_BASE_DELAY_MS` with up to `GIT_LOCK_RETRY_COUNT`
    /// attempts.
    fn retry_git_operation<F, T>(mut op: F) -> Result<T>
    where
        F: FnMut() -> Result<T>,
    {
        let mut last_err = None;
        for attempt in 0..=GIT_LOCK_RETRY_COUNT {
            match op() {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let err_str = e.to_string();
                    if Self::is_git_lock_error(&err_str) && attempt < GIT_LOCK_RETRY_COUNT {
                        let delay_ms = GIT_LOCK_RETRY_BASE_DELAY_MS * 2u64.pow(attempt);
                        warn!(
                            attempt,
                            delay_ms,
                            error = %e,
                            "Git lock contention detected, retrying after delay"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        last_err = Some(e);
                        continue;
                    }
                    // Not a lock error, or we've exhausted retries
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("git retry exhausted without capturing error")))
    }

    /// Reuse an existing worktree by fetching origin/main and creating a new branch.
    ///
    /// This is the full (async) version used by callers that are NOT under the
    /// GIT_WORKTREE_MUTEX. The sync-only version `reuse_worktree_sync` is used
    /// inside the mutex. Kept for direct callers who need async identity config.
    #[allow(dead_code)]
    async fn reuse_worktree(
        &self,
        worktree_path: &Path,
        new_branch: &str,
        github_token: &str,
    ) -> Result<WorktreeSetupResult> {
        self.fetch_and_reset_to_main(worktree_path)?;
        self.create_branch_from_main(worktree_path, new_branch)?;

        // Configure git identity from the PAT
        if let Err(e) = self
            .configure_git_identity(worktree_path, github_token)
            .await
        {
            warn!(error = %e, "Failed to configure git identity from PAT, using local git config");
        }

        // Configure remote URL with the token for push authentication
        if let Err(e) = self.configure_remote_with_token(worktree_path, github_token) {
            warn!(error = %e, "Failed to configure remote URL with token");
        }

        info!(
            path = %worktree_path.display(),
            branch = %new_branch,
            "Worktree reused successfully"
        );
        Ok(WorktreeSetupResult {
            path: worktree_path.to_path_buf(),
            warnings: Vec::new(),
        })
    }

    /// Fetch origin/main and reset the worktree to it.
    fn fetch_and_reset_to_main(&self, worktree_path: &Path) -> Result<()> {
        info!(path = %worktree_path.display(), "Fetching origin/main and resetting");

        let fetch = Command::new("git")
            .args(["fetch", "origin", "main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to fetch origin/main")?;

        if !fetch.status.success() {
            warn!(
                error = %String::from_utf8_lossy(&fetch.stderr),
                "git fetch origin/main failed, continuing"
            );
        }

        let stash = Command::new("git")
            .args(["stash", "--include-untracked"])
            .current_dir(worktree_path)
            .output();

        if let Ok(output) = stash {
            if output.status.success() {
                info!(path = %worktree_path.display(), "Stashed uncommitted changes");
            }
        }

        let checkout = Command::new("git")
            .args(["checkout", "main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to checkout main")?;

        if !checkout.status.success() {
            let checkout = Command::new("git")
                .args(["checkout", "origin/main"])
                .current_dir(worktree_path)
                .output()
                .context("Failed to checkout origin/main")?;

            if !checkout.status.success() {
                return Err(anyhow!(
                    "Failed to checkout main or origin/main: {}",
                    String::from_utf8_lossy(&checkout.stderr)
                ));
            }
        }

        let pull = Command::new("git")
            .args(["pull", "origin", "main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to pull origin/main")?;

        if !pull.status.success() {
            warn!(
                error = %String::from_utf8_lossy(&pull.stderr),
                "git pull origin/main failed, continuing"
            );
        }

        Ok(())
    }

    /// Create a new branch from the current HEAD (assumed to be main).
    fn create_branch_from_main(&self, worktree_path: &Path, branch_name: &str) -> Result<()> {
        self.delete_branch_if_exists(branch_name);

        let output = Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(worktree_path)
            .output()
            .context("Failed to create new branch")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to create branch {}: {}",
                branch_name,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        info!(path = %worktree_path.display(), branch = %branch_name, "Created new branch from main");
        Ok(())
    }

    /// Remove a worktree and its associated branch by pair_id.
    pub fn remove_worktree(&self, pair_id: &str) -> Result<()> {
        let worktree_path = self.worktrees_dir.join(pair_id);

        if !worktree_path.exists() {
            bail!("No worktree found for pair {}", pair_id);
        }

        let current_branch = self.get_current_branch(&worktree_path).ok();
        let branch_name = current_branch.unwrap_or_else(|| "unknown".to_string());

        self.remove_worktree_by_path(&worktree_path, &branch_name)
    }

    /// Remove a specific worktree (now keyed by pair_id only).
    pub fn remove_worktree_for_ticket(&self, pair_id: &str, _ticket_id: &str) -> Result<()> {
        self.remove_worktree(pair_id)
    }

    /// Remove a worktree by its path and branch name.
    fn remove_worktree_by_path(&self, worktree_path: &Path, branch_name: &str) -> Result<()> {
        info!(path = %worktree_path.display(), "Removing worktree");

        let output = Command::new("git")
            .args(["worktree", "remove"])
            .arg(worktree_path)
            .current_dir(&self.project_root)
            .output();

        match output {
            Ok(output) if output.status.success() => {
                info!("Worktree removed successfully");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(error = %stderr, "Git worktree remove failed, forcing removal");

                let output = Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(worktree_path)
                    .current_dir(&self.project_root)
                    .output()
                    .context("Failed to force remove worktree")?;

                if !output.status.success() {
                    warn!(path = %worktree_path.display(), "Forcing manual worktree removal");
                    if worktree_path.exists() {
                        std::fs::remove_dir_all(worktree_path)
                            .context("Failed to manually remove worktree directory")?;
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to run git worktree remove");
                if worktree_path.exists() {
                    std::fs::remove_dir_all(worktree_path)
                        .context("Failed to manually remove worktree directory")?;
                }
            }
        }

        self.prune_stale_worktrees();
        self.delete_branch_if_exists(branch_name);

        info!("Worktree removed");
        Ok(())
    }

    /// Create an idle worktree on main branch (keyed by pair_id).
    pub fn create_idle_worktree(&self, pair_id: &str) -> Result<PathBuf> {
        let worktree_path = self.worktrees_dir.join(pair_id);

        info!(pair_id, "Creating idle worktree on main");

        if worktree_path.exists() {
            let current = self.get_current_branch(&worktree_path).ok();
            self.remove_worktree_by_path(&worktree_path, &current.unwrap_or_default())?;
        }

        std::fs::create_dir_all(&self.worktrees_dir)
            .context("Failed to create worktrees directory")?;

        let output = Command::new("git")
            .args(["worktree", "add"])
            .arg(&worktree_path)
            .arg("main")
            .current_dir(&self.project_root)
            .output()
            .context("Failed to run git worktree add")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to create idle worktree: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        info!(path = %worktree_path.display(), "Idle worktree created");
        Ok(worktree_path)
    }

    /// Check for divergence from main and optionally rebase.
    pub fn check_divergence(
        &self,
        worktree_path: &Path,
        threshold: u32,
    ) -> Result<DivergenceStatus> {
        let behind = self.count_commits_behind(worktree_path)?;

        debug!(path = %worktree_path.display(), behind, "Divergence check");

        if behind > threshold {
            info!(behind, threshold, "Branch is behind main, rebase needed");
            return Ok(DivergenceStatus::NeedsRebase {
                commits_behind: behind,
            });
        }

        Ok(DivergenceStatus::UpToDate)
    }

    /// Fetch origin/main and merge it into the worktree branch.
    ///
    /// This materializes conflicts locally so FORGE can see and resolve them.
    /// Used when VESSEL detects merge conflicts on GitHub but the worktree
    /// doesn't have them locally because main was never merged in.
    pub fn merge_origin_main(&self, worktree_path: &Path) -> Result<MergeMainResult> {
        info!(path = %worktree_path.display(), "Fetching origin/main into worktree");

        let fetch = Command::new("git")
            .args(["fetch", "origin", "main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to fetch origin/main in worktree")?;

        if !fetch.status.success() {
            return Err(anyhow!(
                "git fetch origin/main failed in worktree: {}",
                String::from_utf8_lossy(&fetch.stderr)
            ));
        }

        info!(path = %worktree_path.display(), "Merging origin/main into worktree branch");

        let merge = Command::new("git")
            .args(["merge", "origin/main", "--no-edit"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to merge origin/main in worktree")?;

        if merge.status.success() {
            info!(path = %worktree_path.display(), "origin/main merged cleanly — no conflicts");
            return Ok(MergeMainResult::Clean);
        }

        let stderr = String::from_utf8_lossy(&merge.stderr);

        if stderr.contains("refusing to merge unrelated histories") {
            warn!(
                path = %worktree_path.display(),
                "Branch and origin/main have unrelated histories — retrying with --allow-unrelated-histories"
            );
            let retry = Command::new("git")
                .args([
                    "merge",
                    "origin/main",
                    "--no-edit",
                    "--allow-unrelated-histories",
                ])
                .current_dir(worktree_path)
                .output()
                .context("Failed to merge origin/main with --allow-unrelated-histories")?;

            if retry.status.success() {
                info!(path = %worktree_path.display(), "origin/main merged cleanly with --allow-unrelated-histories");
                return Ok(MergeMainResult::Clean);
            }

            let retry_stderr = String::from_utf8_lossy(&retry.stderr);
            if retry_stderr.contains("conflict") || retry_stderr.contains("CONFLICT") {
                let conflicted_files = Self::list_conflicted_files_in(worktree_path)?;
                warn!(
                    path = %worktree_path.display(),
                    files = conflicted_files.len(),
                    "Merge with --allow-unrelated-histories produced conflict markers"
                );
                return Ok(MergeMainResult::Conflict { conflicted_files });
            }

            return Err(anyhow!(
                "git merge origin/main --allow-unrelated-histories failed: {}",
                retry_stderr
            ));
        }

        if stderr.contains("conflict") || stderr.contains("CONFLICT") {
            let conflicted_files = Self::list_conflicted_files_in(worktree_path)?;
            warn!(
                path = %worktree_path.display(),
                files = conflicted_files.len(),
                "Merge produced conflict markers in worktree"
            );
            return Ok(MergeMainResult::Conflict { conflicted_files });
        }

        Err(anyhow!("git merge origin/main failed: {}", stderr))
    }

    /// List files with conflict markers in a worktree.
    fn list_conflicted_files_in(worktree_path: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to list conflicted files")?;

        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Ok(files)
    }

    /// Rebase the worktree onto origin/main.
    pub fn rebase_onto_main(&self, worktree_path: &Path) -> Result<RebaseResult> {
        info!(path = %worktree_path.display(), "Rebasing onto origin/main");

        // Fetch latest
        let output = Command::new("git")
            .args(["fetch", "origin", "main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to fetch origin/main")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to fetch: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Rebase
        let output = Command::new("git")
            .args(["rebase", "origin/main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to rebase")?;

        if output.status.success() {
            info!(path = %worktree_path.display(), "Rebase successful");
            return Ok(RebaseResult::Success);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("conflict") {
            warn!(path = %worktree_path.display(), "Rebase has conflicts");
            return Ok(RebaseResult::Conflict);
        }

        Err(anyhow!("Rebase failed: {}", stderr))
    }

    /// Abort an in-progress rebase.
    pub fn abort_rebase(&self, worktree_path: &Path) -> Result<()> {
        let output = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to abort rebase")?;

        if !output.status.success() {
            warn!(error = %String::from_utf8_lossy(&output.stderr), "Failed to abort rebase");
        }

        Ok(())
    }

    /// Get the current branch name in a worktree.
    pub fn get_current_branch(&self, worktree_path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to get current branch")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to get branch: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Count commits behind origin/main.
    fn count_commits_behind(&self, worktree_path: &Path) -> Result<u32> {
        let output = Command::new("git")
            .args(["rev-list", "--count", "HEAD..origin/main"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to count commits behind")?;

        if !output.status.success() {
            // If origin/main doesn't exist, return 0
            return Ok(0);
        }

        let count: u32 = String::from_utf8(output.stdout)?
            .trim()
            .parse()
            .unwrap_or(0);

        Ok(count)
    }

    fn run_git_in_main(&self, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.project_root)
            .output()
            .context("Failed to run git command")?;

        if !output.status.success() {
            return Err(anyhow!(
                "Git command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(())
    }

    fn list_unmerged_files_in_main(&self) -> Vec<String> {
        Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(&self.project_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    fn prune_stale_worktrees(&self) {
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.project_root)
            .output();
    }

    /// Force-push the worktree's current branch to origin (with --force-with-lease).
    /// Used after merging origin/main during conflict rework to update the remote branch
    /// so GitHub re-evaluates the PR's mergeability.
    pub fn force_push_branch(&self, worktree_path: &Path) -> Result<()> {
        let branch = self.get_current_branch(worktree_path)?;

        let fetch = Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to fetch before force-push")?;

        if !fetch.status.success() {
            warn!(
                path = %worktree_path.display(),
                error = %String::from_utf8_lossy(&fetch.stderr),
                "git fetch origin failed before force-push — continuing anyway"
            );
        }

        info!(path = %worktree_path.display(), branch = %branch, "Force-pushing branch to origin with --force-with-lease");

        let output = Command::new("git")
            .args(["push", "origin", &branch, "--force-with-lease"])
            .current_dir(worktree_path)
            .output()
            .context("Failed to force-push branch")?;

        if output.status.success() {
            info!(path = %worktree_path.display(), branch = %branch, "Force-push succeeded");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("stale info") || stderr.contains("rejected") {
                warn!(
                    path = %worktree_path.display(),
                    branch = %branch,
                    "force-with-lease rejected (stale info) — falling back to --force"
                );
                let force = Command::new("git")
                    .args(["push", "origin", &branch, "--force"])
                    .current_dir(worktree_path)
                    .output()
                    .context("Failed to force-push branch")?;

                if force.status.success() {
                    info!(path = %worktree_path.display(), branch = %branch, "Force-push (no lease) succeeded");
                    return Ok(());
                }
                let force_stderr = String::from_utf8_lossy(&force.stderr);
                return Err(anyhow!("Force-push failed: {}", force_stderr));
            }
            Err(anyhow!("Force-push failed: {}", stderr))
        }
    }

    fn delete_branch_if_exists(&self, branch_name: &str) {
        let output = Command::new("git")
            .args(["branch", "-D"])
            .arg(branch_name)
            .current_dir(&self.project_root)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                info!(branch = branch_name, "Deleted stale branch");
            }
            _ => {
                debug!(
                    branch = branch_name,
                    "Branch does not exist or could not be deleted"
                );
            }
        }
    }

    /// Generate branch name for a pair/ticket.
    pub fn branch_name(pair_id: &str, ticket_id: &str) -> String {
        if pair_id.starts_with("forge-") {
            format!("{}/{}", pair_id, ticket_id)
        } else {
            format!("forge-{}/{}", pair_id, ticket_id)
        }
    }
}

/// Status of branch divergence from main.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DivergenceStatus {
    /// Branch is up to date with main
    UpToDate,
    /// Branch needs rebase
    NeedsRebase { commits_behind: u32 },
}

/// Result of a rebase operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseResult {
    /// Rebase completed successfully
    Success,
    /// Rebase has conflicts that need resolution
    Conflict,
}

/// Result of worktree creation, including any setup warnings.
#[derive(Debug, Clone)]
pub struct WorktreeSetupResult {
    /// Path to the created worktree.
    pub path: PathBuf,
    /// Warnings encountered during setup (git fetch/merge errors, dirty state, etc.).
    pub warnings: Vec<SetupWarning>,
}

/// A warning encountered during worktree setup.
#[derive(Debug, Clone)]
pub struct SetupWarning {
    /// Phase that produced the warning (e.g., "fetch_origin_main", "merge_origin_main", "worktree_dirty").
    pub phase: String,
    /// The error message or git stderr.
    pub error: String,
    /// Affected files (unmerged/dirty) if detectable.
    pub affected_files: Vec<String>,
}

/// Result of a merge origin/main operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeMainResult {
    /// Merge completed cleanly — no conflicts
    Clean,
    /// Merge produced conflict markers that need resolution
    Conflict { conflicted_files: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_name() {
        assert_eq!(
            WorktreeManager::branch_name("pair-1", "T-42"),
            "forge-pair-1/T-42"
        );
    }

    #[test]
    fn test_is_git_lock_error_detects_index_lock() {
        assert!(WorktreeManager::is_git_lock_error(
            "fatal: Unable to create '/path/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn test_is_git_lock_error_detects_another_git_process() {
        assert!(WorktreeManager::is_git_lock_error(
            "fatal: Another git process seems to be running in this repository"
        ));
    }

    #[test]
    fn test_is_git_lock_error_rejects_other_errors() {
        assert!(!WorktreeManager::is_git_lock_error(
            "fatal: not a git repository"
        ));
        assert!(!WorktreeManager::is_git_lock_error(
            "error: pathspec 'foo' did not match any file(s) known to git"
        ));
    }

    #[test]
    fn test_retry_git_operation_succeeds_immediately() {
        let call_count = std::cell::Cell::new(0);
        let result = WorktreeManager::retry_git_operation(|| {
            call_count.set(call_count.get() + 1);
            Ok(42)
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.get(), 1);
    }

    #[test]
    fn test_retry_git_operation_retries_on_lock_error() {
        let call_count = std::cell::Cell::new(0);
        let result = WorktreeManager::retry_git_operation(|| {
            call_count.set(call_count.get() + 1);
            if call_count.get() < 3 {
                Err(anyhow!(
                    "fatal: Unable to create '.git/index.lock': File exists."
                ))
            } else {
                Ok(99)
            }
        });
        assert_eq!(result.unwrap(), 99);
        assert_eq!(call_count.get(), 3);
    }

    #[test]
    fn test_retry_git_operation_exhausts_retries() {
        let result: Result<i32> = WorktreeManager::retry_git_operation(|| {
            Err(anyhow!(
                "fatal: Unable to create '.git/index.lock': File exists."
            ))
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("index.lock"));
    }

    #[test]
    fn test_retry_git_operation_does_not_retry_non_lock_errors() {
        let call_count = std::cell::Cell::new(0);
        let result: Result<i32> = WorktreeManager::retry_git_operation(|| {
            call_count.set(call_count.get() + 1);
            Err(anyhow!("fatal: not a git repository"))
        });
        assert!(result.is_err());
        // Should have been called only once (no retry for non-lock errors)
        assert_eq!(call_count.get(), 1);
    }
}
