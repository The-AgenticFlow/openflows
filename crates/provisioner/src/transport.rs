//! Workspace transport abstraction for Coder-based operations.
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use tracing::info;

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

/// Trait for workspace transport operations.
#[async_trait]
pub trait WorkspaceTransport: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    async fn execute(&self, command: &str) -> Result<CommandOutput>;
    async fn list_directory(&self, path: &str) -> Result<Vec<DirEntry>>;
    async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()>;
    async fn create_dir_all(&self, path: &str) -> Result<()>;
    async fn path_exists(&self, path: &str) -> bool;
    async fn remove_dir_all(&self, path: &str) -> Result<()>;
    async fn copy_file(&self, source_local: &Path, target: &str) -> Result<()>;
}

/// Coder workspace transport — executes commands and reads/writes files
/// via the Coder REST API's exec endpoint.
pub struct CoderTransport {
    client: coder_client::CoderClient,
    workspace_id: String,
    verbose: bool,
}

impl CoderTransport {
    pub fn new(client: coder_client::CoderClient, workspace_id: &str) -> Self {
        Self {
            client,
            workspace_id: workspace_id.to_string(),
            verbose: std::env::var("CODER_TRANSPORT_VERBOSE")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
        }
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    fn verbose_log(&self, msg: &str) {
        if self.verbose {
            info!(workspace_id = %self.workspace_id, "{}", msg);
        }
    }

    fn copy_dir_recursive<'a>(
        &'a self,
        source_dir: &'a Path,
        target_dir: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        let verbose = self.verbose;
        Box::pin(async move {
            if verbose {
                info!(source = %source_dir.display(), target = %target_dir, "copy_dir_recursive");
            }
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

#[async_trait]
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
        let content_len = content.len();
        self.verbose_log(&format!("write_file: {} ({} bytes)", path, content_len));
        match self
            .client
            .workspace_write_file(&self.workspace_id, path, content)
            .await
        {
            Ok(_) => {
                self.verbose_log(&format!("write_file success: {}", path));
                Ok(())
            }
            Err(e) => {
                tracing::error!(path = %path, error = %e, "write_file failed");
                Err(anyhow::anyhow!("CoderTransport write_file failed: {}", e))
            }
        }
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
            .map_err(|e| anyhow::anyhow!("list_directory failed: {}", e))?;
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
        self.verbose_log(&format!("create_dir_all: {}", path));
        let escaped_path = shell_escape(path);
        let output = self
            .client
            .workspace_exec(&self.workspace_id, &format!("mkdir -p {}", escaped_path))
            .await
            .map_err(|e| anyhow::anyhow!("create_dir_all failed: {}", e))?;
        if output.exit_code != 0 {
            tracing::error!(path = %path, stderr = %output.stderr, "create_dir_all failed");
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
            .map_err(|e| anyhow::anyhow!("remove_dir_all failed: {}", e))?;
        if output.exit_code != 0 {
            anyhow::bail!("remove_dir_all failed: {}", output.stderr);
        }
        Ok(())
    }

    async fn copy_file(&self, source_local: &Path, target: &str) -> Result<()> {
        let content = std::fs::read_to_string(source_local)
            .map_err(|e| anyhow::anyhow!("copy_file: failed to read local file: {}", e))?;
        let content_len = content.len();
        self.verbose_log(&format!(
            "copy_file: {} -> {} ({} bytes)",
            source_local.display(),
            target,
            content_len
        ));
        match self
            .client
            .workspace_write_file(&self.workspace_id, target, &content)
            .await
        {
            Ok(_) => {
                self.verbose_log(&format!(
                    "copy_file success: {} -> {}",
                    source_local.display(),
                    target
                ));
                Ok(())
            }
            Err(e) => {
                tracing::error!(source = %source_local.display(), target = %target, error = %e, "copy_file failed");
                Err(anyhow::anyhow!(
                    "copy_file: workspace_write_file failed: {}",
                    e
                ))
            }
        }
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn parse_ls_output(ls_output: &str) -> Vec<DirEntry> {
    ls_output
        .lines()
        .skip(1)
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
