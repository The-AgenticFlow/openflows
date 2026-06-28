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
///
/// Command execution in workspaces uses `coder ssh` (not a REST endpoint,
/// since Coder v2 has no `/api/v2/workspaces/{id}/exec` route). The client
/// stores both the workspace ID and workspace name so that REST and SSH
/// operations can use the appropriate identifier.
pub struct CoderClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    /// Workspace name for `coder ssh` (e.g., "forge-1-T-041").
    workspace_name: Option<String>,
    /// Session token for `coder ssh` via `CODER_SESSION_TOKEN` env.
    /// When set, used instead of the API token for workspace SSH operations.
    session_token: Option<String>,
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
            workspace_name: None,
            session_token: None,
        }
    }

    /// Set the workspace name for `coder ssh` operations.
    pub fn with_workspace_name(mut self, name: &str) -> Self {
        self.workspace_name = Some(name.to_string());
        self
    }

    /// Set the session token for workspace SSH operations.
    /// Session tokens are used by the `coder` CLI for authentication
    /// via the `CODER_SESSION_TOKEN` environment variable.
    pub fn with_session_token(mut self, token: &str) -> Self {
        self.session_token = Some(token.to_string());
        self
    }

    /// Set the workspace name from a known workspace ID by looking it up via the API.
    /// The SSH target is set to `owner_name/workspace_name` for unambiguous resolution.
    pub async fn set_workspace_name_from_id(&mut self, id: &str) -> Result<()> {
        match self.get_workspace(id).await {
            Ok(ws) => {
                let ssh_target = if !ws.owner_name.is_empty() {
                    format!("{}/{}", ws.owner_name, ws.name)
                } else {
                    ws.name.clone()
                };
                debug!(workspace_id = id, ssh_target = %ssh_target, "Resolved workspace SSH target for coder ssh");
                self.workspace_name = Some(ssh_target);
                Ok(())
            }
            Err(e) => {
                warn!(workspace_id = id, error = %e, "Failed to look up workspace name, will use ID as fallback for coder ssh");
                Ok(())
            }
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
            workspace_name: None,
            session_token: None,
        }
    }

    /// Create a client with a different token (e.g., after bootstrap).
    pub fn with_token(&self, token: String) -> Self {
        Self {
            base_url: self.base_url.clone(),
            token,
            http: self.http.clone(),
            workspace_name: self.workspace_name.clone(),
            session_token: self.session_token.clone(),
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

    /// Get the session token (if set). Falls back to the API token.
    pub fn session_token(&self) -> &str {
        self.session_token.as_deref().unwrap_or(&self.token)
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
                // The users listing endpoint may require authentication or return
                // a paginated response. Try to extract the user ID from whatever
                // we get back so bootstrap can proceed with login.
                let users_resp_result = self
                    .authenticated_request(reqwest::Method::GET, "/api/v2/users")
                    .send()
                    .await;
                match users_resp_result {
                    Ok(resp) if resp.status().is_success() => {
                        let users_resp: UsersResponse = resp.json().await.unwrap_or_default();
                        if !users_resp.users.is_empty() {
                            return users_resp
                                .users
                                .into_iter()
                                .find(|u| u.username == username || u.email == email)
                                .context(format!(
                                    "Admin user '{}' not found after 409 conflict",
                                    username
                                ));
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "Failed to list users during 409 recovery");
                    }
                    other => {
                        debug!(response = ?other, "Users listing returned non-success status");
                    }
                }
                info!("Could not list users (requires auth); returning stub user for login");
                Ok(CoderUser {
                    id: String::new(),
                    username: username.to_string(),
                    email: email.to_string(),
                })
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

    /// Get the current authenticated user (/api/v2/users/me).
    pub async fn get_me(&self) -> Result<CoderUser> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/users/me")
            .send()
            .await
            .context("Failed to fetch current user")?;

        if resp.status().is_success() {
            let user: CoderUser = resp.json().await?;
            Ok(user)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get current user ({}): {}", status, body)
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
                let list_body: serde_json::Value = resp.json().await.unwrap_or_default();
                if let Some(items) = list_body.as_array() {
                    for item in items {
                        if let Some(item_name) = item.get("name").and_then(|v| v.as_str()) {
                            if item_name == name {
                                if let Some(item_key) = item.get("key").and_then(|v| v.as_str()) {
                                    let api_key = CoderApiKey {
                                        id: item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        name: item_name.to_string(),
                                        key: item_key.to_string(),
                                    };
                                    info!(key_name = %api_key.name, "Reusing existing API token");
                                    return Ok(api_key);
                                }
                            }
                        }
                    }
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
                "lifetime": 168 * 3600 * 1_000_000_000_i64,
            }))
            .send()
            .await
            .context("Failed to create API token")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let key_value = body.get("key").and_then(|v| v.as_str())
                .context("No 'key' field in API token response")?
                .to_string();
            let key_id = body.get("id").and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let name_value = body.get("name").and_then(|v| v.as_str())
                .unwrap_or(name)
                .to_string();
            let api_key = CoderApiKey {
                id: key_id,
                name: name_value,
                key: key_value,
            };
            info!(key_name = %api_key.name, "Created API token");
            Ok(api_key)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create API token ({}): {}", status, body)
        }
    }

    /// Push a template. Since Coder v2 doesn't support template version
    /// updates via REST, this writes the tar.gz archive to a temp directory,
    /// extracts it, and uses `coder templates push` CLI to create/update
    /// the template.
    pub async fn push_template(&self, name: &str, archive: &[u8]) -> Result<CoderTemplate> {
        use std::process::Stdio;

        // Check if template already exists
        let templates = self.list_templates().await?;
        let template_exists = templates.iter().any(|t| t.name == name);

        if template_exists {
            info!(template_name = %name, "Template exists, updating via coder CLI");
        } else {
            info!(template_name = %name, "Creating new template via coder CLI");
        }

        // Write archive to temp directory
        let temp_dir = std::env::temp_dir().join(format!("coder-template-{}", name));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).context("Failed to create temp template directory")?;

        // Extract tar.gz using flate2 + tar crate or system tar command
        let archive_path = temp_dir.join(format!("{}.tar.gz", name));
        std::fs::write(&archive_path, archive).context("Failed to write archive")?;

        // Extract the tar.gz
        let status = std::process::Command::new("tar")
            .args(["xzf", archive_path.to_str().unwrap(), "-C", temp_dir.to_str().unwrap()])
            .status()
            .context("Failed to run tar to extract template")?;

        if !status.success() {
            bail!("Failed to extract template archive");
        }

        // Push the template via coder CLI
        // The CLI reads the directory and pushes it to the Coder server
        let output = tokio::process::Command::new("coder")
            .args(["templates", "push", "--yes", name, "-d", temp_dir.to_str().unwrap()])
            .env("CODER_URL", &self.base_url)
            .env("CODER_SESSION_TOKEN", self.session_token())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run coder templates push")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            warn!(
                name,
                stderr = %stderr,
                stdout = %stdout,
                "coder templates push returned non-zero exit code"
            );
        } else {
            info!(name, stderr = %stderr, "coder templates push succeeded");
        }

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Return the template info by listing again
        let templates = self.list_templates().await?;
        templates.into_iter().find(|t| t.name == name)
            .ok_or_else(|| anyhow::anyhow!("Template '{}' not found after push", name))
    }

    /// List available templates.
    pub async fn list_templates(&self) -> Result<Vec<CoderTemplate>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/templates")
            .send()
            .await
            .context("Failed to list templates")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let templates = if let Some(arr) = body.as_array() {
                arr.iter()
                    .filter_map(|v| {
                        Some(CoderTemplate {
                            id: v.get("id")?.as_str()?.to_string(),
                            name: v.get("name")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            } else {
                vec![]
            };
            Ok(templates)
        } else {
            bail!("Failed to list templates: {}", resp.status())
        }
    }

    /// Create a workspace from a template.
    ///
    /// Resolves the authenticated user's ID via `/users/me`, looks up the template
    /// ID by name, then creates the workspace at `/api/v2/users/{user_id}/workspaces`.
    pub async fn create_workspace(&self, req: &CreateWorkspaceRequest) -> Result<CoderWorkspace> {
        // Resolve the current user ID
        let user_id = self.get_me().await.map(|u| u.id)
            .context("Failed to resolve current user ID for workspace creation")?;
        info!(%user_id, "Resolved user ID for workspace creation");

        // Look up the template by name to get its ID
        let template_id = self.find_template_id_by_name(&req.template_name).await?;

        // Convert parameters from {"key":"value"} to [{"name":"key","value":"value"}]
        let rich_parameter_values = if req.parameters.is_object() {
            let obj = req.parameters.as_object().unwrap();
            serde_json::Value::Array(
                obj.iter()
                    .map(|(k, v)| {
                        serde_json::json!({"name": k, "value": v.as_str().unwrap_or("")})
                    })
                    .collect(),
            )
        } else {
            serde_json::Value::Array(vec![])
        };

        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/users/{}/workspaces", user_id),
            )
            .json(&serde_json::json!({
                "template_id": template_id,
                "name": req.name,
                "rich_parameter_values": rich_parameter_values,
            }))
            .send()
            .await
            .context("Failed to create workspace")?;

        if resp.status().is_success() {
            let workspace: CoderWorkspace = resp.json().await?;
            info!(workspace_id = %workspace.id, workspace_name = %workspace.name, "Created workspace");
            Ok(workspace)
        } else if resp.status().as_u16() == 409 {
            // Workspace already exists — find it by name and return it
            info!(workspace_name = %req.name, "Workspace already exists, looking it up");
            let workspaces = self.list_workspaces(&user_id).await?;
            workspaces
                .into_iter()
                .find(|w| w.name == req.name)
                .context(format!(
                    "Workspace '{}' exists but not found in listing",
                    req.name
                ))
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create workspace ({}): {}", status, body)
        }
    }

    /// Look up a template ID by name.
    async fn find_template_id_by_name(&self, name: &str) -> Result<String> {
        let templates = self.list_templates().await?;
        templates
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.id)
            .ok_or_else(|| anyhow::anyhow!("Template '{}' not found", name))
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

    /// List workspaces across the deployment (filtered by current user when authenticated).
    async fn list_workspaces(&self, _user_id: &str) -> Result<Vec<CoderWorkspace>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/workspaces")
            .send()
            .await
            .context("Failed to list workspaces")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let workspaces = body
                .get("workspaces")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| {
                            Some(CoderWorkspace {
                                id: v.get("id")?.as_str()?.to_string(),
                                name: v.get("name")?.as_str()?.to_string(),
                                owner_name: v.get("owner_name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                status: v.get("latest_build")
                                    .and_then(|b| b.get("status"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                latest_build: v.get("latest_build")
                                    .map(|b| serde_json::from_value(b.clone())).transpose().ok().flatten(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(workspaces)
        } else {
            bail!("Failed to list workspaces: {}", resp.status())
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

    /// Execute a command in a workspace via `coder ssh`.
    /// Coder v2 has no REST exec endpoint; commands are run through the
    /// coder CLI which uses SSH to reach the workspace agent.
    pub async fn workspace_exec_with_timeout(
        &self,
        _workspace_id: &str,
        command: &str,
        timeout_secs: u64,
    ) -> Result<crate::types::CommandOutput> {
        let ws_target = self.workspace_name.as_deref().unwrap_or(_workspace_id);

        debug!(
            workspace = ws_target,
            command = %command,
            "Executing command via coder ssh"
        );

        let ssh_token = self.session_token();
        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::process::Command::new("coder")
                .args(["ssh", ws_target, "--", "bash", "-lc", command])
                .env("CODER_URL", &self.base_url)
                .env("CODER_SESSION_TOKEN", ssh_token)
                .output(),
        )
        .await
        .context(format!(
            "coder ssh timed out after {}s for workspace {}",
            timeout_secs, ws_target
        ))??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        debug!(
            exit_code,
            stdout_len = stdout.len(),
            stderr_len = stderr.len(),
            "coder ssh completed"
        );

        Ok(crate::types::CommandOutput {
            stdout,
            stderr,
            exit_code,
        })
    }

    /// Execute a command in a workspace via `coder ssh` with a 60s default timeout.
    pub async fn workspace_exec(
        &self,
        workspace_id: &str,
        command: &str,
    ) -> Result<crate::types::CommandOutput> {
        self.workspace_exec_with_timeout(workspace_id, command, 60).await
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
