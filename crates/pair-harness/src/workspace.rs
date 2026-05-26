// crates/pair-harness/src/workspace.rs
//! Workspace management for isolated repository operations.
//!
//! Handles cloning of target repositories into dedicated workspace
//! directories, ensuring the orchestrator doesn't work on itself.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

/// Manages the target repository workspace.
///
/// The orchestrator clones the target GitHub repository into a dedicated
/// workspace directory, ensuring complete isolation from the orchestrator's
/// own source code.
pub struct WorkspaceManager {
    /// Base directory for all workspaces (e.g., ~/.agentflow/workspaces/)
    workspaces_base: PathBuf,
    /// The specific workspace directory for this repository
    workspace_dir: PathBuf,
    /// Repository identifier (e.g., "owner/repo")
    repo_id: String,
}

impl WorkspaceManager {
    /// Create a new workspace manager for a given repository.
    ///
    /// # Arguments
    /// * `workspaces_base` - Base directory for all workspaces
    /// * `repo_id` - Repository identifier in "owner/repo" format
    pub fn new(workspaces_base: impl Into<PathBuf>, repo_id: &str) -> Self {
        let workspaces_base = workspaces_base.into();
        // Convert "owner/repo" to "owner-repo" for directory name
        let dir_name = repo_id.replace('/', "-");
        let workspace_dir = workspaces_base.join(&dir_name);

        Self {
            workspaces_base,
            workspace_dir,
            repo_id: repo_id.to_string(),
        }
    }

    /// Get the workspace directory path.
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    /// Ensure the workspace exists, cloning if necessary.
    ///
    /// If the workspace already exists and is a git repo, it will be updated with `git pull`.
    /// If it exists but is not a git repo, it will be cloned fresh.
    /// If it doesn't exist, it will be cloned from GitHub.
    ///
    /// # Arguments
    /// * `github_token` - GitHub personal access token for authentication
    ///
    /// # Returns
    /// Path to the workspace directory.
    pub async fn ensure_workspace(&self, github_token: &str) -> Result<PathBuf> {
        // Create base workspaces directory
        tokio::fs::create_dir_all(&self.workspaces_base)
            .await
            .context("Failed to create workspaces base directory")?;

        if self.workspace_dir.exists() && self.workspace_dir.join(".git").exists() {
            info!(workspace = %self.workspace_dir.display(), "Workspace exists, updating...");
            self.update_workspace()?;
        } else if self.workspace_dir.exists() {
            info!(workspace = %self.workspace_dir.display(), "Workspace directory exists but is not a git repo — removing and re-cloning");
            tokio::fs::remove_dir_all(&self.workspace_dir)
                .await
                .context("Failed to remove invalid workspace directory")?;
            self.clone_workspace(github_token).await?;
        } else {
            info!(repo = %self.repo_id, workspace = %self.workspace_dir.display(), "Cloning repository...");
            self.clone_workspace(github_token).await?;
        }

        Ok(self.workspace_dir.clone())
    }

    /// Clone the repository into the workspace.
    async fn clone_workspace(&self, github_token: &str) -> Result<()> {
        // Build authenticated URL
        // Format: https://x-access-token:TOKEN@github.com/owner/repo.git
        let clone_url = format!(
            "https://x-access-token:{}@github.com/{}.git",
            github_token, self.repo_id
        );

        // Clone the full main branch history (not --depth 1).
        // Shallow clones cannot create worktrees or new branches,
        // which breaks the FORGE-SENTINEL pair isolation model.
        // --single-branch --no-tags avoids fetching unrelated history
        // while still providing enough commits for branching.
        let output = Command::new("git")
            .args(["clone", "--single-branch", "--no-tags"])
            .arg(&clone_url)
            .arg(&self.workspace_dir)
            .output()
            .context("Failed to execute git clone")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to clone repository: {}", stderr));
        }

        info!(workspace = %self.workspace_dir.display(), "Repository cloned successfully");
        Ok(())
    }

    /// Update an existing workspace with git pull.
    fn update_workspace(&self) -> Result<()> {
        // Unshallow if needed — worktree/branch operations require full history
        self.unshallow_if_needed();

        // Fetch and pull latest changes
        let output = Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&self.workspace_dir)
            .output()
            .context("Failed to execute git fetch")?;

        if !output.status.success() {
            warn!(stderr = %String::from_utf8_lossy(&output.stderr), "Git fetch failed, continuing anyway");
        }

        // Detect the default branch instead of hardcoding "main"
        let default_branch =
            crate::worktree::WorktreeManager::detect_default_branch(&self.workspace_dir);
        let output = Command::new("git")
            .args(["pull", "--rebase", "origin", &default_branch])
            .current_dir(&self.workspace_dir)
            .output()
            .context("Failed to execute git pull")?;

        if !output.status.success() {
            warn!(stderr = %String::from_utf8_lossy(&output.stderr), default_branch = %default_branch, "Git pull from origin/{} failed, continuing anyway", default_branch);
        }

        info!(workspace = %self.workspace_dir.display(), "Workspace updated");
        Ok(())
    }

    /// Unshallow the repository if it was cloned with --depth 1.
    /// Worktree and branch operations require full history.
    fn unshallow_if_needed(&self) {
        let shallow_file = self.workspace_dir.join(".git").join("shallow");
        if !shallow_file.exists() {
            return;
        }

        info!(workspace = %self.workspace_dir.display(), "Unshallowing repository for worktree support");

        let output = Command::new("git")
            .args(["fetch", "--unshallow"])
            .current_dir(&self.workspace_dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                info!(workspace = %self.workspace_dir.display(), "Repository unshallowed successfully");
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

    /// Remove the workspace directory.
    pub fn remove_workspace(&self) -> Result<()> {
        if self.workspace_dir.exists() {
            std::fs::remove_dir_all(&self.workspace_dir)
                .context("Failed to remove workspace directory")?;
            info!(workspace = %self.workspace_dir.display(), "Workspace removed");
        }
        Ok(())
    }

    /// Get the default branch name for the repository.
    pub fn get_default_branch(&self) -> Result<String> {
        Ok(crate::worktree::WorktreeManager::detect_default_branch(
            &self.workspace_dir,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_workspace_dir_naming() {
        let temp_dir = tempdir().unwrap();
        let manager = WorkspaceManager::new(temp_dir.path(), "owner/repo");

        assert!(manager.workspace_dir().ends_with("owner-repo"));
    }
}
