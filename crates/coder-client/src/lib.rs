// crates/coder-client/src/lib.rs
//! Client library for the Coder OSS REST API.
//!
//! Provides types and methods for:
//! - Waiting for Coder server health
//! - Creating the first admin user
//! - Logging in and obtaining API tokens
//! - Pushing workspace templates
//! - Creating, starting, stopping, and deleting workspaces
//! - Executing commands in workspaces
//!
//! File I/O in workspaces uses the `exec` API (run `cat`/`tee`/`mkdir` commands).

pub mod bootstrap;
pub mod types;

pub use types::*;

use anyhow::{bail, Context, Result};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Client for the Coder OSS REST API.
pub struct CoderClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl CoderClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Create an unauthenticated client (for bootstrap/login operations).
    pub fn new_unauthenticated(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: String::new(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Create a client with a different token (e.g., after bootstrap).
    pub fn with_token(&self, token: String) -> Self {
        Self {
            base_url: self.base_url.clone(),
            token,
            http: self.http.clone(),
        }
    }

    /// Get the base URL of the Coder server.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the current API token.
    pub fn token(&self) -> &str {
        &self.token
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authenticated_request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, &self.url(path));
        if !self.token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.token));
        }
        req
    }

    /// Wait for the Coder server to be healthy (respond to /api/v2/buildinfo).
    /// Reuses the existing HTTP client with a per-request timeout.
    pub async fn wait_for_healthy(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();

        info!(
            "Waiting for Coder server to be healthy at {}",
            self.base_url
        );
        while start.elapsed() < timeout {
            match self
                .http
                .get(&self.url("/api/v2/buildinfo"))
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    info!("Coder server is healthy");
                    return Ok(());
                }
                Ok(resp) => {
                    warn!(
                        status = resp.status().as_u16(),
                        "Coder server responded with non-200 status, retrying..."
                    );
                }
                Err(e) => {
                    debug!(error = %e, "Coder server not reachable, retrying...");
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!(
            "Coder server at {} did not become healthy within {:?}",
            self.base_url,
            timeout
        )
    }

    /// Create the first user (admin) on a fresh Coder instance.
    /// Idempotent: returns existing user if already created.
    pub async fn create_first_user(
        &self,
        email: &str,
        username: &str,
        password: &str,
    ) -> Result<CoderUser> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/users/first")
            .json(&serde_json::json!({
                "email": email,
                "username": username,
                "password": password,
            }))
            .send()
            .await
            .context("Failed to send create_first_user request")?;

        if resp.status().is_success() {
            let user: CoderUser = resp.json().await?;
            info!(user_id = %user.id, username = %user.username, "Created first user");
            Ok(user)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 409 {
                info!("First user already exists, looking up admin user");
                let users: Vec<CoderUser> = self
                    .authenticated_request(reqwest::Method::GET, "/api/v2/users")
                    .send()
                    .await?
                    .json()
                    .await?;
                users
                    .into_iter()
                    .find(|u| u.username == username || u.email == email)
                    .context(format!(
                        "Admin user '{}' not found after 409 conflict",
                        username
                    ))
            } else {
                bail!("Failed to create first user ({}): {}", status, body)
            }
        }
    }

    /// Login with email/password and get a session token.
    pub async fn login_with_password(&self, email: &str, password: &str) -> Result<String> {
        let resp = self
            .http
            .post(&self.url("/api/v2/users/login"))
            .json(&serde_json::json!({
                "email": email,
                "password": password,
            }))
            .send()
            .await
            .context("Failed to send login request")?;

        if resp.status().is_success() {
            let login_resp: serde_json::Value = resp.json().await?;
            login_resp
                .get("session_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .context("No session_token in login response")
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Login failed ({}): {}", status, body)
        }
    }

    /// Create an API token for a user. Attempts to reuse an existing token
    /// with the same name first for idempotency.
    pub async fn create_api_token(&self, user_id: &str, name: &str) -> Result<CoderApiKey> {
        // Try to find an existing token with the same name first
        let list_resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/users/{}/keys/tokens", user_id),
            )
            .send()
            .await;

        if let Ok(resp) = list_resp {
            if resp.status().is_success() {
                let keys: Vec<CoderApiKey> = resp.json().await.unwrap_or_default();
                if let Some(existing) = keys.into_iter().find(|k| k.name == name) {
                    info!(key_name = %existing.name, "Reusing existing API token");
                    return Ok(existing);
                }
            }
        }

        // No existing token found — create a new one
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/users/{}/keys/tokens", user_id),
            )
            .json(&serde_json::json!({
                "name": name,
                "lifetime": "87600h",
            }))
            .send()
            .await
            .context("Failed to create API token")?;

        if resp.status().is_success() {
            let key: CoderApiKey = resp.json().await?;
            info!(key_name = %key.name, "Created API token");
            Ok(key)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create API token ({}): {}", status, body)
        }
    }

    /// Push a template (upload a .tar.gz archive). Creates or updates.
    pub async fn push_template(&self, name: &str, archive: &[u8]) -> Result<CoderTemplate> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/templates?name={}", name),
            )
            .header("Content-Type", "application/x-tar")
            .body(archive.to_vec())
            .send()
            .await
            .context("Failed to push template")?;

        if resp.status().is_success() {
            let template: CoderTemplate = resp.json().await?;
            info!(template_name = %template.name, "Pushed template");
            Ok(template)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to push template ({}): {}", status, body)
        }
    }

    /// List available templates.
    pub async fn list_templates(&self) -> Result<Vec<CoderTemplate>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/templates")
            .send()
            .await
            .context("Failed to list templates")?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            bail!("Failed to list templates: {}", resp.status())
        }
    }

    /// Create a workspace from a template.
    pub async fn create_workspace(&self, req: &CreateWorkspaceRequest) -> Result<CoderWorkspace> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/workspaces")
            .json(&serde_json::json!({
                "template_name": req.template_name,
                "name": req.name,
                "parameters": req.parameters,
            }))
            .send()
            .await
            .context("Failed to create workspace")?;

        if resp.status().is_success() {
            let workspace: CoderWorkspace = resp.json().await?;
            info!(workspace_id = %workspace.id, workspace_name = %workspace.name, "Created workspace");
            Ok(workspace)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create workspace ({}): {}", status, body)
        }
    }

    /// Get workspace by ID.
    pub async fn get_workspace(&self, id: &str) -> Result<CoderWorkspace> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/workspaces/{}", id))
            .send()
            .await
            .context("Failed to get workspace")?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            bail!("Failed to get workspace: {}", resp.status())
        }
    }

    /// Start a stopped workspace.
    pub async fn start_workspace(&self, id: &str) -> Result<CoderWorkspace> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/workspaces/{}/start", id),
            )
            .send()
            .await
            .context("Failed to start workspace")?;

        if resp.status().is_success() {
            info!(workspace_id = id, "Started workspace");
            Ok(resp.json().await?)
        } else {
            bail!("Failed to start workspace: {}", resp.status())
        }
    }

    /// Stop a running workspace.
    pub async fn stop_workspace(&self, id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/workspaces/{}/stop", id),
            )
            .send()
            .await
            .context("Failed to stop workspace")?;

        if resp.status().is_success() {
            info!(workspace_id = id, "Stopped workspace");
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to stop workspace: {}", body)
        }
    }

    /// Delete a workspace.
    pub async fn delete_workspace(&self, id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::DELETE,
                &format!("/api/v2/workspaces/{}", id),
            )
            .send()
            .await
            .context("Failed to delete workspace")?;

        if resp.status().is_success() {
            info!(workspace_id = id, "Deleted workspace");
            Ok(())
        } else {
            bail!("Failed to delete workspace: {}", resp.status())
        }
    }

    /// Wait until a workspace's agent is ready (accepting connections).
    pub async fn wait_for_workspace_ready(&self, id: &str, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        info!(workspace_id = id, "Waiting for workspace to be ready");
        while start.elapsed() < timeout {
            match self.get_workspace(id).await {
                Ok(ws) if ws.is_running() => {
                    info!(workspace_id = id, "Workspace is ready");
                    return Ok(());
                }
                Ok(ws) => {
                    debug!(
                        workspace_id = id,
                        status = ?ws.status,
                        "Workspace not yet ready, retrying..."
                    );
                }
                Err(e) => {
                    debug!(error = %e, "Error checking workspace status, retrying...");
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        bail!("Workspace {} did not become ready within {:?}", id, timeout)
    }

    /// Execute a command in a workspace via the Coder exec API.
    pub async fn workspace_exec(
        &self,
        workspace_id: &str,
        command: &str,
    ) -> Result<crate::types::CommandOutput> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/workspaces/{}/exec", workspace_id),
            )
            .json(&serde_json::json!({
                "command": command,
                "timeout": 60,
            }))
            .send()
            .await
            .context("Failed to exec command in workspace")?;

        if resp.status().is_success() {
            let exec_resp: serde_json::Value = resp.json().await?;
            let stdout = exec_resp
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let stderr = exec_resp
                .get("stderr")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let exit_code = exec_resp
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1) as i32;
            Ok(crate::types::CommandOutput {
                stdout,
                stderr,
                exit_code,
            })
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to exec in workspace ({}): {}", status, body)
        }
    }

    /// Read a file from a workspace via exec (cat).
    pub async fn workspace_read_file(&self, workspace_id: &str, path: &str) -> Result<String> {
        // Use base64 encoding to avoid shell injection through file paths
        let escaped_path = shell_escape(path);
        let output = self
            .workspace_exec(workspace_id, &format!("cat {}", escaped_path))
            .await?;
        if output.exit_code != 0 {
            anyhow::bail!("read_file failed: {}", output.stderr);
        }
        Ok(output.stdout)
    }

    /// Write content to a file in a workspace via exec (heredoc with base64).
    pub async fn workspace_write_file(
        &self,
        workspace_id: &str,
        path: &str,
        content: &str,
    ) -> Result<()> {
        // Encode content as base64 to avoid heredoc delimiter collision and
        // shell injection. Decode on the remote side.
        let escaped_path = shell_escape(path);
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        let cmd = format!(
            "mkdir -p $(dirname {}) && echo '{}' | base64 -d > {}",
            escaped_path, b64, escaped_path
        );
        let output = self.workspace_exec(workspace_id, &cmd).await?;
        if output.exit_code != 0 {
            anyhow::bail!("write_file failed: {}", output.stderr);
        }
        Ok(())
    }
}

/// Shell-escape a path to prevent command injection.
/// Wraps the path in single quotes and escapes any embedded single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
