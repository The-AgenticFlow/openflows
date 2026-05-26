// crates/pair-harness/src/worktree.rs
//! Git worktree management for pair isolation.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// Manages Git worktrees for pair isolation.
pub struct WorktreeManager {
    /// Project root directory (contains .git)
    project_root: PathBuf,
    /// Directory where worktrees are created
    worktrees_dir: PathBuf,
    /// Detected default branch name (e.g., "main" or "master")
    default_branch: String,
}

impl WorktreeManager {
    /// Create a new worktree manager.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let default_branch = Self::detect_default_branch(&project_root);
        info!(default_branch = %default_branch, "Detected repository default branch");
        Self {
            worktrees_dir: project_root.join("worktrees"),
            project_root,
            default_branch,
        }
    }

    /// Returns the detected default branch name (e.g., "main" or "master").
    pub fn default_branch(&self) -> &str {
        &self.default_branch
    }

    /// Detect the repository's default branch from origin/HEAD.
    ///
    /// Uses `git symbolic-ref refs/remotes/origin/HEAD` to read the
    /// remote HEAD symref, which GitHub sets to point at the default
    /// branch (e.g., refs/remotes/origin/master). Falls back to
    /// trying "main" then "master" via remote ref existence checks.
    pub fn detect_default_branch(project_root: &Path) -> String {
        // Method 1: Read origin/HEAD symref (most reliable)
        let output = Command::new("git")
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
            .current_dir(project_root)
            .output();

        if let Ok(o) = output {
            if o.status.success() {
                let refname = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Extract branch name from "refs/remotes/origin/{branch}"
                if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
                    if !branch.is_empty() {
                        return branch.to_string();
                    }
                }
            }
        }

        // Method 2: Check which remote branch ref exists
        for candidate in ["main", "master"] {
            let ref_path = format!("refs/remotes/origin/{candidate}");
            let git_dir = project_root.join(".git");
            // Check packed-refs or loose ref
            if git_dir.join(&ref_path).exists() {
                return candidate.to_string();
            }
            // Also check packed-refs file
            if let Ok(packed) = std::fs::read_to_string(git_dir.join("packed-refs")) {
                if packed.contains(&ref_path) {
                    return candidate.to_string();
                }
            }
        }

        // Method 3: Try git rev-parse for each candidate
        for candidate in ["main", "master"] {
            let output = Command::new("git")
                .args(["rev-parse", "--verify", &format!("origin/{candidate}")])
                .current_dir(project_root)
                .output();
            if let Ok(o) = output {
                if o.status.success() {
                    return candidate.to_string();
                }
            }
        }

        // Final fallback
        warn!("Could not detect default branch, falling back to 'main'");
        "main".to_string()
    }

    /// Returns the origin-qualified default branch ref (e.g., "origin/main" or "origin/master").
    fn origin_default_branch(&self) -> String {
        format!("origin/{}", self.default_branch)
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
        let worktree_path = self.worktrees_dir.join(pair_id);
        let branch_name = Self::branch_name(pair_id, ticket_id);
        let mut warnings = Vec::new();

        info!(pair_id, ticket_id, branch = %branch_name, "Creating worktree");

        // Ensure the repository is not shallow — worktree/branch ops need full history
        self.unshallow_if_needed();

        // Remove any stale index entries for worktrees/ that would block
        // git worktree add (e.g., leftover submodule entries from previous runs)
        self.clean_stale_worktrees_index();

        // Ensure worktrees/ is in .gitignore so git doesn't track the worktree dirs
        self.ensure_worktrees_gitignored();

        if let Err(e) = self.run_git_in_main(&["fetch", "origin", &self.default_branch]) {
            warn!(error = %e, default_branch = %self.default_branch, "git fetch origin/{} failed, continuing", self.default_branch);
            warnings.push(SetupWarning {
                phase: format!("fetch_origin_{}", self.default_branch),
                error: e.to_string(),
                affected_files: vec![],
            });
        }
        if let Err(e) = self.run_git_in_main(&["merge", &self.origin_default_branch()]) {
            warn!(error = %e, default_branch = %self.default_branch, "git merge origin/{} failed, continuing", self.default_branch);
            let affected_files = self.list_unmerged_files_in_main();
            warnings.push(SetupWarning {
                phase: format!("merge_origin_{}", self.default_branch),
                error: e.to_string(),
                affected_files,
            });
        }

        if worktree_path.exists() {
            // Check if this is a proper git worktree (has .git file/link)
            let is_proper_worktree = worktree_path.join(".git").exists();

            if let Ok(current) = self.get_current_branch(&worktree_path) {
                if current == branch_name && is_proper_worktree {
                    info!(
                        path = %worktree_path.display(),
                        branch = %branch_name,
                        "Worktree already on correct branch - reusing"
                    );
                    return Ok(WorktreeSetupResult {
                        path: worktree_path,
                        warnings,
                    });
                }
                if is_proper_worktree {
                    info!(
                        path = %worktree_path.display(),
                        current = %current,
                        new_branch = %branch_name,
                        "Reusing existing worktree for new ticket"
                    );
                    return self
                        .reuse_worktree(&worktree_path, &branch_name, github_token)
                        .await;
                }
            }
            // Not a proper worktree — remove and recreate from scratch
            warn!(
                path = %worktree_path.display(),
                is_proper = is_proper_worktree,
                "Worktree directory exists but is not a proper git worktree, replacing"
            );
            self.remove_worktree_by_path(&worktree_path, "unknown")?;
        }

        self.prune_stale_worktrees();
        self.delete_branch_if_exists(&branch_name);

        std::fs::create_dir_all(&self.worktrees_dir)
            .context("Failed to create worktrees directory")?;

        let output = Command::new("git")
            .args(["worktree", "add"])
            .arg(&worktree_path)
            .args(["-b", &branch_name])
            .current_dir(&self.project_root)
            .output()
            .context("Failed to run git worktree add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already exists") {
                info!(branch = %branch_name, "Branch exists, creating worktree from existing branch");
                let output = Command::new("git")
                    .args(["worktree", "add"])
                    .arg(&worktree_path)
                    .arg(&branch_name)
                    .current_dir(&self.project_root)
                    .output()
                    .context("Failed to run git worktree add from existing branch")?;

                if !output.status.success() {
                    return Err(anyhow!(
                        "Failed to create worktree from existing branch: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
            } else {
                return Err(anyhow!("Failed to create worktree: {}", stderr));
            }
        }

        // Configure git identity from the PAT so commits show the PAT owner's identity
        if let Err(e) = self
            .configure_git_identity(&worktree_path, github_token)
            .await
        {
            warn!(error = %e, "Failed to configure git identity from PAT, using local git config");
        }

        // Configure remote URL with the token for push authentication
        if let Err(e) = self.configure_remote_with_token(&worktree_path, github_token) {
            warn!(error = %e, "Failed to configure remote URL with token");
        }

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
        Ok(WorktreeSetupResult {
            path: worktree_path,
            warnings,
        })
    }

    /// Unshallow the repository if it was cloned with --depth 1.
    /// Worktree and branch operations require full commit history.
    fn unshallow_if_needed(&self) {
        let shallow_file = self.project_root.join(".git").join("shallow");
        if !shallow_file.exists() {
            return;
        }

        info!(path = %self.project_root.display(), "Unshallowing repository for worktree support");

        let output = Command::new("git")
            .args(["fetch", "--unshallow"])
            .current_dir(&self.project_root)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                info!(path = %self.project_root.display(), "Repository unshallowed successfully");
            }
            Ok(o) => {
                warn!(
                    stderr = %String::from_utf8_lossy(&o.stderr),
                    "git fetch --unshallow failed, worktree operations may not work"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to run git fetch --unshallow");
            }
        }
    }

    /// Remove stale `worktrees/` entries from the git index.
    ///
    /// Previous runs may have accidentally committed worktree directories
    /// (as submodule entries with mode 160000), which blocks `git worktree add`.
    /// This removes them from the index without affecting the working tree.
    fn clean_stale_worktrees_index(&self) {
        // Clean both worktrees/ and orchestration/pairs/ — these are runtime
        // state that should not be tracked in the upstream repo
        for prefix in &["worktrees/", "orchestration/pairs/"] {
            let output = Command::new("git")
                .args(["ls-files", "--stage", "--", prefix])
                .current_dir(&self.project_root)
                .output();

            if let Ok(o) = output {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.trim().is_empty() {
                    continue; // Nothing to clean
                }

                // Remove entries from the index
                info!(
                    path = %self.project_root.display(),
                    prefix,
                    entries = stdout.lines().count(),
                    "Removing stale index entries"
                );

                let rm_output = Command::new("git")
                    .args(["rm", "--cached", "-r", "--", prefix])
                    .current_dir(&self.project_root)
                    .output();

                match rm_output {
                    Ok(o) if o.status.success() => {
                        info!(prefix, "Stale index entries removed");
                    }
                    Ok(o) => {
                        warn!(
                            stderr = %String::from_utf8_lossy(&o.stderr),
                            prefix,
                            "Failed to remove stale index entries"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, prefix, "Failed to run git rm --cached");
                    }
                }
            }
        }
    }

    /// Ensure `worktrees/` is listed in `.gitignore` so that worktree
    /// directories created inside the repo aren't tracked by git.
    fn ensure_worktrees_gitignored(&self) {
        let gitignore_path = self.project_root.join(".gitignore");

        let existing = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

        let entries = ["worktrees/", "orchestration/"];
        let mut updated = existing.clone();

        for entry in &entries {
            if updated.lines().any(|l| l.trim() == *entry) {
                continue; // Already present
            }
            if updated.is_empty() {
                updated = format!("{}\n", entry);
            } else if updated.ends_with('\n') {
                updated = format!("{}{}\n", updated, entry);
            } else {
                updated = format!("{}\n{}\n", updated, entry);
            }
        }

        if updated != existing {
            if let Err(e) = std::fs::write(&gitignore_path, &updated) {
                warn!(error = %e, "Failed to update .gitignore");
            } else {
                info!(path = %gitignore_path.display(), "Updated .gitignore with runtime directories");
            }
        }
    }

    /// Reuse an existing worktree by fetching origin/main and creating a new branch.
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

    /// Fetch origin/{default_branch} and reset the worktree to it.
    fn fetch_and_reset_to_main(&self, worktree_path: &Path) -> Result<()> {
        info!(path = %worktree_path.display(), default_branch = %self.default_branch, "Fetching origin/{} and resetting", self.default_branch);

        let fetch = Command::new("git")
            .args(["fetch", "origin", &self.default_branch])
            .current_dir(worktree_path)
            .output()
            .context(format!("Failed to fetch origin/{}", self.default_branch))?;

        if !fetch.status.success() {
            warn!(
                error = %String::from_utf8_lossy(&fetch.stderr),
                default_branch = %self.default_branch,
                "git fetch origin/{} failed, continuing", self.default_branch
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
            .args(["checkout", &self.default_branch])
            .current_dir(worktree_path)
            .output()
            .context(format!("Failed to checkout {}", self.default_branch))?;

        if !checkout.status.success() {
            let checkout = Command::new("git")
                .args(["checkout", &self.origin_default_branch()])
                .current_dir(worktree_path)
                .output()
                .context(format!(
                    "Failed to checkout {}",
                    self.origin_default_branch()
                ))?;

            if !checkout.status.success() {
                return Err(anyhow!(
                    "Failed to checkout {} or {}: {}",
                    self.default_branch,
                    self.origin_default_branch(),
                    String::from_utf8_lossy(&checkout.stderr)
                ));
            }
        }

        let pull = Command::new("git")
            .args(["pull", "origin", &self.default_branch])
            .current_dir(worktree_path)
            .output()
            .context(format!("Failed to pull origin/{}", self.default_branch))?;

        if !pull.status.success() {
            warn!(
                error = %String::from_utf8_lossy(&pull.stderr),
                default_branch = %self.default_branch,
                "git pull origin/{} failed, continuing", self.default_branch
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

        info!(pair_id, default_branch = %self.default_branch, "Creating idle worktree on {}", self.default_branch);

        if worktree_path.exists() {
            let current = self.get_current_branch(&worktree_path).ok();
            self.remove_worktree_by_path(&worktree_path, &current.unwrap_or_default())?;
        }

        std::fs::create_dir_all(&self.worktrees_dir)
            .context("Failed to create worktrees directory")?;

        let output = Command::new("git")
            .args(["worktree", "add"])
            .arg(&worktree_path)
            .arg(&self.default_branch)
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

    /// Fetch origin/{default_branch} and merge it into the worktree branch.
    ///
    /// This materializes conflicts locally so FORGE can see and resolve them.
    /// Used when VESSEL detects merge conflicts on GitHub but the worktree
    /// doesn't have them locally because the default branch was never merged in.
    pub fn merge_origin_main(&self, worktree_path: &Path) -> Result<MergeMainResult> {
        info!(path = %worktree_path.display(), default_branch = %self.default_branch, "Fetching origin/{} into worktree", self.default_branch);

        let fetch = Command::new("git")
            .args(["fetch", "origin", &self.default_branch])
            .current_dir(worktree_path)
            .output()
            .context(format!(
                "Failed to fetch origin/{} in worktree",
                self.default_branch
            ))?;

        if !fetch.status.success() {
            return Err(anyhow!(
                "git fetch origin/{} failed in worktree: {}",
                self.default_branch,
                String::from_utf8_lossy(&fetch.stderr)
            ));
        }

        info!(path = %worktree_path.display(), default_branch = %self.default_branch, "Merging origin/{} into worktree branch", self.default_branch);

        let merge = Command::new("git")
            .args(["merge", &self.origin_default_branch(), "--no-edit"])
            .current_dir(worktree_path)
            .output()
            .context(format!(
                "Failed to merge {} in worktree",
                self.origin_default_branch()
            ))?;

        if merge.status.success() {
            info!(path = %worktree_path.display(), "origin/{} merged cleanly — no conflicts", self.default_branch);
            return Ok(MergeMainResult::Clean);
        }

        let stderr = String::from_utf8_lossy(&merge.stderr);

        if stderr.contains("refusing to merge unrelated histories") {
            warn!(
                path = %worktree_path.display(),
                default_branch = %self.default_branch,
                "Branch and origin/{} have unrelated histories — retrying with --allow-unrelated-histories", self.default_branch
            );
            let retry = Command::new("git")
                .args([
                    "merge",
                    &self.origin_default_branch(),
                    "--no-edit",
                    "--allow-unrelated-histories",
                ])
                .current_dir(worktree_path)
                .output()
                .context(format!(
                    "Failed to merge {} with --allow-unrelated-histories",
                    self.origin_default_branch()
                ))?;

            if retry.status.success() {
                info!(path = %worktree_path.display(), "origin/{} merged cleanly with --allow-unrelated-histories", self.default_branch);
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
                "git merge {} --allow-unrelated-histories failed: {}",
                self.origin_default_branch(),
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

        Err(anyhow!(
            "git merge {} failed: {}",
            self.origin_default_branch(),
            stderr
        ))
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

    /// Rebase the worktree onto origin/{default_branch}.
    pub fn rebase_onto_main(&self, worktree_path: &Path) -> Result<RebaseResult> {
        info!(path = %worktree_path.display(), default_branch = %self.default_branch, "Rebasing onto origin/{}", self.default_branch);

        // Fetch latest
        let output = Command::new("git")
            .args(["fetch", "origin", &self.default_branch])
            .current_dir(worktree_path)
            .output()
            .context(format!("Failed to fetch origin/{}", self.default_branch))?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to fetch: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Rebase
        let output = Command::new("git")
            .args(["rebase", &self.origin_default_branch()])
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

    /// Count commits behind origin/{default_branch}.
    fn count_commits_behind(&self, worktree_path: &Path) -> Result<u32> {
        let origin_ref = self.origin_default_branch();
        let output = Command::new("git")
            .args(["rev-list", "--count", &format!("HEAD..{}", origin_ref)])
            .current_dir(worktree_path)
            .output()
            .context("Failed to count commits behind")?;

        if !output.status.success() {
            // If origin/{default_branch} doesn't exist, return 0
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
}
