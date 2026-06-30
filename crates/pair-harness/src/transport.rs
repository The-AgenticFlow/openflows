// crates/pair-harness/src/transport.rs
//! Workspace transport abstraction for local and Coder-based operations.
//!
//! The `WorkspaceTransport` trait decouples pair-harness operations from the
//! local filesystem, enabling both local git worktree workflows and remote
//! Coder workspace workflows through the same interface.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;
use tracing::debug;

/// Output from a command executed in a workspace.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// A directory entry returned by `list_directory`.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Trait for workspace transport operations. Both local filesystem and
/// remote Coder workspace operations implement this trait.
#[async_trait]
pub trait WorkspaceTransport: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    async fn execute(&self, command: &str) -> Result<CommandOutput>;
    async fn list_directory(&self, path: &str) -> Result<Vec<DirEntry>>;

    /// Create a symlink (local) or copy contents (Coder) from source to target.
    /// source is always a local filesystem path; target is a workspace path.
    async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()>;
    /// Create a directory and all parent directories.
    async fn create_dir_all(&self, path: &str) -> Result<()>;
    /// Check if a path exists (file or directory).
    async fn path_exists(&self, path: &str) -> bool;
    /// Recursively remove a directory.
    async fn remove_dir_all(&self, path: &str) -> Result<()>;
    /// Copy a local file to the workspace.
    async fn copy_file(&self, source_local: &Path, target: &str) -> Result<()>;
}

/// Local filesystem transport — wraps existing file and process operations
/// relative to a worktree root directory.
pub struct LocalTransport {
    worktree_root: PathBuf,
}

impl LocalTransport {
    pub fn new(worktree_root: impl Into<PathBuf>) -> Self {
        Self {
            worktree_root: worktree_root.into(),
        }
    }

    pub fn root(&self) -> &PathBuf {
        &self.worktree_root
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let candidate = PathBuf::from(path);
        if candidate.is_absolute() {
            candidate
        } else {
            self.worktree_root.join(candidate)
        }
    }
}

#[async_trait]
impl WorkspaceTransport for LocalTransport {
    async fn read_file(&self, path: &str) -> Result<String> {
        let full_path = self.resolve_path(path);
        debug!(path = %full_path.display(), "LocalTransport: reading file");
        let content = fs::read_to_string(&full_path).await?;
        Ok(content)
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        debug!(path = %full_path.display(), "LocalTransport: writing file");
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&full_path, content).await?;
        Ok(())
    }

    async fn execute(&self, command: &str) -> Result<CommandOutput> {
        debug!(command = %command, root = %self.worktree_root.display(), "LocalTransport: executing command");
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.worktree_root)
            .output()
            .await?;

        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    async fn list_directory(&self, path: &str) -> Result<Vec<DirEntry>> {
        let full_path = self.resolve_path(path);
        debug!(path = %full_path.display(), "LocalTransport: listing directory");
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&full_path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await?.is_dir();
            entries.push(DirEntry { name, is_dir });
        }
        Ok(entries)
    }

    async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()> {
        let full_target = self.resolve_path(target);
        if source.is_dir() {
            std::os::unix::fs::symlink(source, &full_target)?;
        } else {
            std::os::unix::fs::symlink(source, &full_target)?;
        }
        Ok(())
    }

    async fn create_dir_all(&self, path: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        fs::create_dir_all(&full_path).await?;
        Ok(())
    }

    async fn path_exists(&self, path: &str) -> bool {
        let full_path = self.resolve_path(path);
        full_path.exists()
    }

    async fn remove_dir_all(&self, path: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        fs::remove_dir_all(&full_path).await?;
        Ok(())
    }

    async fn copy_file(&self, source_local: &Path, target: &str) -> Result<()> {
        let full_target = self.resolve_path(target);
        if let Some(parent) = full_target.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::copy(source_local, &full_target).await?;
        Ok(())
    }
}

/// Coder workspace transport — executes commands and reads/writes files
/// via the Coder REST API's exec endpoint.
///
/// Only available when the `coder` feature is enabled.
#[cfg(feature = "coder")]
pub struct CoderTransport {
    client: coder_client::CoderClient,
    workspace_id: String,
}

#[cfg(feature = "coder")]
impl CoderTransport {
    pub fn new(client: coder_client::CoderClient, workspace_id: &str) -> Self {
        Self {
            client,
            workspace_id: workspace_id.to_string(),
        }
    }

    fn copy_dir_recursive<'a>(
        &'a self,
        source_dir: &'a Path,
        target_dir: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.create_dir_all(target_dir).await?;
            for entry in std::fs::read_dir(source_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                let target_path = format!("{}/{}", target_dir.trim_end_matches('/'), name);
                let source_path = entry.path();
                if source_path.is_dir() {
                    self.copy_dir_recursive(&source_path, &target_path).await?;
                } else if source_path.is_file() {
                    self.copy_file(&source_path, &target_path).await?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(feature = "coder")]
#[async_trait::async_trait]
impl WorkspaceTransport for CoderTransport {
    async fn read_file(&self, path: &str) -> Result<String> {
        let result = self
            .client
            .workspace_read_file(&self.workspace_id, path)
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport read_file failed: {}", e))?;
        Ok(result)
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.client
            .workspace_write_file(&self.workspace_id, path, content)
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport write_file failed: {}", e))
    }

    async fn execute(&self, command: &str) -> Result<CommandOutput> {
        let output = self
            .client
            .workspace_exec_with_timeout(&self.workspace_id, command, 3600)
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport execute failed: {}", e))?;
        Ok(CommandOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        })
    }

    async fn list_directory(&self, path: &str) -> Result<Vec<DirEntry>> {
        let escaped_path = shell_escape(path);
        let output = self
            .client
            .workspace_exec(&self.workspace_id, &format!("ls -la {}", escaped_path))
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport list_directory failed: {}", e))?;
        if output.exit_code != 0 {
            anyhow::bail!("list_directory failed: {}", output.stderr);
        }
        Ok(parse_ls_output(&output.stdout))
    }

    async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()> {
        if source.is_dir() {
            self.copy_dir_recursive(source, target).await
        } else if source.is_file() {
            self.copy_file(source, target).await
        } else {
            anyhow::bail!(
                "symlink_or_copy: source does not exist: {}",
                source.display()
            );
        }
    }

    async fn create_dir_all(&self, path: &str) -> Result<()> {
        let escaped_path = shell_escape(path);
        let output = self
            .client
            .workspace_exec(&self.workspace_id, &format!("mkdir -p {}", escaped_path))
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport create_dir_all failed: {}", e))?;
        if output.exit_code != 0 {
            anyhow::bail!("create_dir_all failed: {}", output.stderr);
        }
        Ok(())
    }

    async fn path_exists(&self, path: &str) -> bool {
        let escaped_path = shell_escape(path);
        self.client
            .workspace_exec(&self.workspace_id, &format!("test -e {}", escaped_path))
            .await
            .map(|o| o.exit_code == 0)
            .unwrap_or(false)
    }

    async fn remove_dir_all(&self, path: &str) -> Result<()> {
        let escaped_path = shell_escape(path);
        let output = self
            .client
            .workspace_exec(&self.workspace_id, &format!("rm -rf {}", escaped_path))
            .await
            .map_err(|e| anyhow::anyhow!("CoderTransport remove_dir_all failed: {}", e))?;
        if output.exit_code != 0 {
            anyhow::bail!("remove_dir_all failed: {}", output.stderr);
        }
        Ok(())
    }

    async fn copy_file(&self, source_local: &Path, target: &str) -> Result<()> {
        let content = std::fs::read_to_string(source_local)
            .map_err(|e| anyhow::anyhow!("copy_file: failed to read local file: {}", e))?;
        self.client
            .workspace_write_file(&self.workspace_id, target, &content)
            .await
            .map_err(|e| anyhow::anyhow!("copy_file: workspace_write_file failed: {}", e))
    }
}

#[cfg(feature = "coder")]
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(feature = "coder")]
fn parse_ls_output(ls_output: &str) -> Vec<DirEntry> {
    ls_output
        .lines()
        .skip(1) // skip "total" line
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 9 {
                return None;
            }
            let is_dir = parts[0].starts_with('d');
            let name = parts[8..].join(" ");
            Some(DirEntry { name, is_dir })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_local_transport_read_write() {
        let dir = tempdir().unwrap();
        let transport = LocalTransport::new(dir.path());

        transport
            .write_file("test.txt", "hello world")
            .await
            .unwrap();
        let content = transport.read_file("test.txt").await.unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn test_local_transport_write_creates_dirs() {
        let dir = tempdir().unwrap();
        let transport = LocalTransport::new(dir.path());

        transport
            .write_file("nested/dir/test.txt", "nested content")
            .await
            .unwrap();
        let content = transport.read_file("nested/dir/test.txt").await.unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn test_local_transport_execute() {
        let dir = tempdir().unwrap();
        let transport = LocalTransport::new(dir.path());

        let output = transport.execute("echo hello").await.unwrap();
        assert_eq!(output.stdout.trim(), "hello");
        assert_eq!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn test_local_transport_list_directory() {
        let dir = tempdir().unwrap();
        let transport = LocalTransport::new(dir.path());

        transport.write_file("a.txt", "a").await.unwrap();
        transport.write_file("b.txt", "b").await.unwrap();

        let entries = transport.list_directory(".").await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[tokio::test]
    async fn test_local_transport_read_nonexistent() {
        let dir = tempdir().unwrap();
        let transport = LocalTransport::new(dir.path());

        let result = transport.read_file("nonexistent.txt").await;
        assert!(result.is_err());
    }
}
