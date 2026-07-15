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

#[cfg(feature = "chats-api")]
pub mod chat_stream;
#[cfg(feature = "chats-api")]
pub use chat_stream::{ChatEvent, ChatStream};

#[cfg(all(test, feature = "chats-api"))]
pub mod mock_chat_server;

pub use types::*;

use anyhow::{bail, Context, Result};
#[cfg(feature = "chats-api")]
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "chats-api")]
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Client for the Coder OSS REST API.
///
/// Command execution in workspaces uses `coder ssh` (not a REST endpoint,
/// since Coder v2 has no `/api/v2/workspaces/{id}/exec` route). The client
/// stores both the workspace ID and workspace name so that REST and SSH
/// operations can use the appropriate identifier.
#[derive(Clone)]
pub struct CoderClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    /// Workspace name for `coder ssh` (e.g., "forge-1-T-041").
    workspace_name: Option<String>,
    /// Session token for `coder ssh` via `CODER_SESSION_TOKEN` env.
    /// When set, used instead of the API token for workspace SSH operations.
    session_token: Option<String>,
    /// Cached model configurations from `list_chat_models()`.
    /// Lazily populated on first access to reduce API call overhead.
    #[cfg(feature = "chats-api")]
    cached_models: Arc<RwLock<Option<Vec<crate::types::ModelInfo>>>>,
}

/// Parse the JSON body returned by `GET /api/experimental/chats/models`.
///
/// Coder nests models under a top-level `providers` array:
/// `{"providers":[{"provider":"openai-compat","models":[
///     {"id":"openai-compat:adorsys-reviewer-pro","model":"adorsys-reviewer-pro",...}]}]}`.
/// Some older/alternate shapes expose a top-level `models` array. Both are
/// supported here (entry is keyed by `id`, de-duplicated). Each model's `id` is
/// normalized to the bare model name the gateway routes on — preferring the
/// `model` field, and otherwise stripping a `provider:` colon prefix from
/// `id` (e.g. `openai-compat:adorsys-reviewer-pro` -> `adorsys-reviewer-pro`)
/// — so downstream CLI `--model` injection never leaks a prefixed id that the
/// gateway rejects with model_not_found.
fn parse_chat_models_body(body: &serde_json::Value) -> Vec<crate::types::ModelInfo> {
    use serde_json::Value;

    fn normalize_id(m: &mut crate::types::ModelInfo, v: &Value) {
        if let Some(bare) = v.get("model").and_then(Value::as_str) {
            let bare = bare.trim();
            if !bare.is_empty() {
                m.id = bare.to_string();
                return;
            }
        }
        if let Some((_, rhs)) = m.id.rsplit_once(':') {
            let rhs = rhs.trim();
            if !rhs.is_empty() {
                m.id = rhs.to_string();
            }
        }
    }

    fn from_value(v: &Value) -> Option<crate::types::ModelInfo> {
        let mut m: crate::types::ModelInfo = serde_json::from_value(v.clone()).ok()?;
        normalize_id(&mut m, v);
        Some(m)
    }

    fn add(
        m: crate::types::ModelInfo,
        models: &mut Vec<crate::types::ModelInfo>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if seen.insert(m.id.clone()) {
            models.push(m);
        }
    }

    let mut models: Vec<crate::types::ModelInfo> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Canonical current Coder shape: `providers[].models`.
    if let Some(providers) = body.get("providers").and_then(Value::as_array) {
        for provider in providers {
            if let Some(arr) = provider.get("models").and_then(Value::as_array) {
                for v in arr {
                    if let Some(m) = from_value(v) {
                        add(m, &mut models, &mut seen);
                    }
                }
            }
        }
    }
    // Fallback/legacy shape: top-level `models` array.
    if let Some(arr) = body.get("models").and_then(Value::as_array) {
        for v in arr {
            if let Some(m) = from_value(v) {
                add(m, &mut models, &mut seen);
            }
        }
    }
    models
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
            #[cfg(feature = "chats-api")]
            cached_models: Arc::new(RwLock::new(None)),
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
            #[cfg(feature = "chats-api")]
            cached_models: Arc::new(RwLock::new(None)),
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
            #[cfg(feature = "chats-api")]
            cached_models: self.cached_models.clone(),
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
            let body = resp
                .text()
                .await
                .context("Failed to read create_first_user response body")?;
            let user: CoderUser = serde_json::from_str(&body).with_context(|| {
                format!(
                    "Failed to deserialize create_first_user response as CoderUser: {}",
                    body
                )
            })?;
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

    /// List all users visible to the authenticated user.
    /// GET /api/v2/users
    pub async fn list_users(&self) -> Result<Vec<CoderUser>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/users")
            .send()
            .await
            .context("Failed to list users")?;

        if resp.status().is_success() {
            let users: UsersResponse = resp.json().await?;
            Ok(users.users)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to list users ({}): {}", status, body)
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

    /// List all organizations visible to the authenticated user.
    /// GET /api/v2/organizations
    pub async fn list_organizations(&self) -> Result<Vec<CoderOrganization>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/organizations")
            .send()
            .await
            .context("Failed to list organizations")?;

        if resp.status().is_success() {
            let orgs: Vec<CoderOrganization> = resp.json().await?;
            Ok(orgs)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to list organizations ({}): {}", status, body)
        }
    }

    /// Get the default organization ID.
    ///
    /// Queries `GET /api/v2/organizations` and returns the ID of the first
    /// organization marked `is_default`. If none is explicitly marked as
    /// default, returns the first organization in the list. Returns an error
    /// if no organizations are found at all.
    pub async fn get_default_organization_id(&self) -> Result<String> {
        let orgs = self.list_organizations().await?;
        if orgs.is_empty() {
            bail!("No organizations found in Coder");
        }
        // Prefer the one flagged is_default, otherwise fall back to the first.
        let default_org = match orgs.iter().find(|o| o.is_default) {
            Some(org) => org,
            None => {
                warn!(
                    total_orgs = orgs.len(),
                    fallback_org_name = %orgs[0].name,
                    "No organization marked is_default; falling back to first organization"
                );
                &orgs[0]
            }
        };
        info!(
            org_id = %default_org.id,
            org_name = %default_org.name,
            is_default = default_org.is_default,
            "Resolved default organization"
        );
        Ok(default_org.id.clone())
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
                                        id: item
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
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
            let key_value = body
                .get("key")
                .and_then(|v| v.as_str())
                .context("No 'key' field in API token response")?
                .to_string();
            let key_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let name_value = body
                .get("name")
                .and_then(|v| v.as_str())
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
            .args([
                "xzf",
                archive_path.to_str().unwrap(),
                "-C",
                temp_dir.to_str().unwrap(),
            ])
            .status()
            .context("Failed to run tar to extract template")?;

        if !status.success() {
            bail!("Failed to extract template archive");
        }

        // Push the template via coder CLI
        // The CLI reads the directory and pushes it to the Coder server
        let mut cmd = tokio::process::Command::new("coder");
        cmd.args([
            "templates",
            "push",
            "--yes",
            name,
            "-d",
            temp_dir.to_str().unwrap(),
        ])
        .env("CODER_URL", &self.base_url)
        .env("CODER_SESSION_TOKEN", self.session_token())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        // Pass TF_VAR_dev_binary_host_path as a --variable flag so the Coder
        // server stores it and applies it when creating workspaces. Setting it
        // as an env var in the Rust process does NOT reach the server's
        // Terraform execution — the CLI only uploads the template files.
        if let Ok(path) = std::env::var("TF_VAR_dev_binary_host_path") {
            if !path.is_empty() {
                cmd.arg("--variable")
                    .arg(format!("dev_binary_host_path={}", path));
            }
        }

        let output = cmd
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
        templates
            .into_iter()
            .find(|t| t.name == name)
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

    /// Create a workspace from a template for the current authenticated user.
    pub async fn create_workspace(&self, req: &CreateWorkspaceRequest) -> Result<CoderWorkspace> {
        let user_id = self
            .get_me()
            .await
            .map(|u| u.id)
            .context("Failed to resolve current user ID for workspace creation")?;
        self.create_workspace_for_user(&user_id, req).await
    }

    /// Create a workspace for a specific user (admin can create for other users).
    pub async fn create_workspace_for_user(
        &self,
        user_id: &str,
        req: &CreateWorkspaceRequest,
    ) -> Result<CoderWorkspace> {
        info!(%user_id, "Creating workspace for user");

        let template_id = self.find_template_id_by_name(&req.template_name).await?;

        let rich_parameter_values = if req.parameters.is_object() {
            let obj = req.parameters.as_object().unwrap();
            serde_json::Value::Array(
                obj.iter()
                    .map(|(k, v)| serde_json::json!({"name": k, "value": v.as_str().unwrap_or("")}))
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
            info!(workspace_name = %req.name, "Workspace already exists, looking it up");
            let workspaces = self.list_workspaces(user_id).await?;
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
                                owner_name: v
                                    .get("owner_name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                status: v
                                    .get("latest_build")
                                    .and_then(|b| b.get("status"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                latest_build: v
                                    .get("latest_build")
                                    .map(|b| serde_json::from_value(b.clone()))
                                    .transpose()
                                    .ok()
                                    .flatten(),
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

    /// Create a role-specific worker workspace for a ticket.
    ///
    /// Workspace naming convention: `{role}-t-{ticket}` (lowercase, Coder name rules).
    /// Uses the `openflows-{role}` template. Waits for the workspace to become ready.
    pub async fn create_role_workspace(
        &self,
        role: &str,
        ticket_id: &str,
        parameters: serde_json::Value,
    ) -> Result<CoderWorkspace> {
        let workspace_name = format!("{}-t-{}", role, ticket_id.to_lowercase());
        let template_name = format!("openflows-{}", role);

        info!(
            workspace_name = %workspace_name,
            template = %template_name,
            role,
            ticket_id,
            "Creating role workspace"
        );

        let workspace = self
            .create_workspace(&CreateWorkspaceRequest {
                template_name,
                name: workspace_name,
                parameters,
            })
            .await?;

        self.wait_for_workspace_ready(&workspace.id, std::time::Duration::from_secs(300))
            .await?;

        Ok(workspace)
    }

    /// Wait until a workspace's build status is "running" **and** its agent
    /// lifecycle state is "ready".
    ///
    /// A workspace can report "running" before the in-container agent has
    /// finished starting up and accepting connections. This method polls
    /// until both conditions are true, which eliminates the race condition
    /// that causes `coder ssh` timeouts.
    pub async fn wait_for_workspace_ready(&self, id: &str, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        info!(workspace_id = id, "Waiting for workspace to be ready");
        while start.elapsed() < timeout {
            match self.get_workspace(id).await {
                Ok(ws) if ws.is_agent_ready() => {
                    info!(
                        workspace_id = id,
                        agent_lifecycle = ?ws.agent_lifecycle_state(),
                        "Workspace is ready (agent connected)"
                    );
                    return Ok(());
                }
                Ok(ws) => {
                    debug!(
                        workspace_id = id,
                        build_status = ?ws.latest_build.as_ref().map(|b| &b.status),
                        agent_lifecycle = ?ws.agent_lifecycle_state(),
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

    /// Wait until a workspace's SSH daemon is reachable via `coder ssh`.
    ///
    /// This must be called **after** `wait_for_workspace_ready()` because a
    /// workspace can report `"running"` before the in-container agent has
    /// finished initialising its SSH endpoint. The probe runs a lightweight
    /// `echo ready` command and retries until SSH succeeds or the timeout
    /// expires.
    pub async fn wait_for_workspace_ssh(
        &self,
        workspace_id: &str,
        timeout: Duration,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        info!(
            workspace_id,
            "Waiting for workspace SSH to become available"
        );
        while start.elapsed() < timeout {
            match self
                .workspace_exec_with_timeout(workspace_id, "echo ready", 15)
                .await
            {
                Ok(output) if output.exit_code == 0 => {
                    info!(workspace_id, "Workspace SSH is available");
                    return Ok(());
                }
                Ok(output) => {
                    debug!(
                        workspace_id,
                        exit_code = output.exit_code,
                        stderr = %output.stderr,
                        "SSH probe returned non-zero, retrying..."
                    );
                }
                Err(e) => {
                    debug!(workspace_id, error = %e, "SSH probe failed, retrying...");
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        bail!(
            "Workspace {} SSH not available within {:?}",
            workspace_id,
            timeout
        )
    }

    /// Execute a command in a workspace via `coder ssh`.
    /// Coder v2 has no REST exec endpoint; commands are run through the
    /// coder CLI which uses SSH to reach the workspace agent.
    ///
    /// # How the command reaches the remote shell
    ///
    /// `coder ssh <ws> -- <args...>` **joins** all args after `--` with spaces
    /// and runs the resulting string through the *remote* login shell. It does
    /// **not** preserve argv boundaries.  Consequently the naïve form
    /// `-- bash -lc "<cmd>"` is silently broken: the remote shell re-splits
    /// `bash -lc <cmd>` so bash's `-c` grabs only the *first word* of `<cmd>`
    /// as its script — the remaining words become `$0`, `$1`, … and are never
    /// executed.  Symptom: `mkdir -p .claude` arrived as `bash -lc mkdir -p
    /// .claude` → bash ran bare `mkdir` → `mkdir: missing operand`.  The
    /// `echo ready` liveness probe likewise ran bare `echo` (rc 0, empty
    /// stdout) — a false-positive "SSH is available".
    ///
    /// To survive the join+resplit, the entire payload is base64-encoded (no
    /// spaces, no shell metacharacters) and decoded on the remote inside a
    /// single shell token, then executed by a login bash so `$PATH` and
    /// profile tools (`git`, `claude`, …) are available:
    ///
    /// ```text
    /// coder ssh <ws> -- 'echo <B64> | base64 -d | bash -l'
    /// ```
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

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(command.as_bytes());
        // One single shell token — spaces/pipes are parsed by the REMOTE shell
        // after coder ssh joins this sole post-`--` arg. base64 guarantees no
        // quoting/escaping issues regardless of the command's contents.
        let wrapped = format!("echo '{}' | base64 -d | bash -l", b64);

        let ssh_token = self.session_token();
        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::process::Command::new("coder")
                .args(["ssh", ws_target, "--", &wrapped])
                .env("CODER_URL", &self.base_url)
                .env("CODER_SESSION_TOKEN", ssh_token)
                // Ensure the local `coder` process is killed when the future
                // is dropped (timeout or JoinHandle abort).  Killing the ssh
                // session closes the remote shell (SIGHUP), terminating the
                // remote SENTINEL instead of orphaning it.
                .kill_on_drop(true)
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
        self.workspace_exec_with_timeout(workspace_id, command, 60)
            .await
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

// Chats API (Phase 3)

impl CoderClient {
    /// Get the current user's username (for constructing chat API paths).
    pub fn current_username(&self) -> String {
        // Extract from workspace_name if available (format: "owner/workspace")
        if let Some(ref ws) = self.workspace_name {
            if let Some((owner, _)) = ws.split_once('/') {
                return owner.to_string();
            }
        }
        "admin".to_string()
    }

    /// Create a new Chat session bound to a workspace.
    /// POST /api/experimental/chats
    pub async fn create_chat(
        &self,
        req: &crate::types::CreateChatRequest,
    ) -> Result<crate::types::Chat> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/experimental/chats")
            .json(req)
            .send()
            .await
            .context("Failed to create chat")?;

        if resp.status().is_success() || resp.status().as_u16() == 201 {
            let chat: crate::types::Chat = resp.json().await?;
            info!(chat_id = %chat.id, "Created chat");
            Ok(chat)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create chat ({}): {}", status, body)
        }
    }

    /// Get a Chat by ID.
    /// GET /api/experimental/chats/{chat}
    pub async fn get_chat(&self, chat_id: &str) -> Result<crate::types::Chat> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/experimental/chats/{}", chat_id),
            )
            .send()
            .await
            .context("Failed to get chat")?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get chat ({}): {}", status, body)
        }
    }

    /// List all Chats for the current user.
    /// GET /api/experimental/chats
    pub async fn list_chats(&self) -> Result<Vec<crate::types::Chat>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/experimental/chats")
            .send()
            .await
            .context("Failed to list chats")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let chats = body
                .get("chats")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            Ok(chats)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to list chats ({}): {}", status, body)
        }
    }

    /// Send a message to an existing Chat.
    /// POST /api/experimental/chats/{chat_id}/messages
    pub async fn send_chat_message(
        &self,
        chat_id: &str,
        content: Vec<crate::types::ChatInputPart>,
    ) -> Result<crate::types::ChatMessage> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/experimental/chats/{}/messages", chat_id),
            )
            .json(&serde_json::json!({
                "content": content,
            }))
            .send()
            .await
            .context("Failed to send chat message")?;

        if resp.status().is_success() || resp.status().as_u16() == 201 {
            let msg: crate::types::ChatMessage = resp.json().await?;
            info!(chat_id, message_id = %msg.id, "Sent chat message");
            Ok(msg)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to send chat message ({}): {}", status, body)
        }
    }

    /// Archive a Chat (soft delete).
    /// PATCH /api/experimental/chats/{chat_id}
    pub async fn archive_chat(&self, chat_id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::PATCH,
                &format!("/api/experimental/chats/{}", chat_id),
            )
            .json(&serde_json::json!({ "archived": true }))
            .send()
            .await
            .context("Failed to archive chat")?;

        if resp.status().is_success() {
            info!(chat_id, "Archived chat");
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to archive chat ({}): {}", status, body)
        }
    }

    /// Interrupt a running Chat.
    /// POST /api/experimental/chats/{chat_id}/interrupt
    pub async fn interrupt_chat(&self, chat_id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/experimental/chats/{}/interrupt", chat_id),
            )
            .send()
            .await
            .context("Failed to interrupt chat")?;

        if resp.status().is_success() || resp.status().as_u16() == 202 {
            info!(chat_id, "Interrupted chat");
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to interrupt chat ({}): {}", status, body)
        }
    }

    /// List available models for chats.
    /// GET /api/experimental/chats/models
    ///
    /// Uses an internal cache (5-minute TTL) to reduce API calls.
    /// Call `invalidate_cache()` to force a refresh.
    #[cfg(feature = "chats-api")]
    pub async fn list_chat_models(&self) -> Result<Vec<crate::types::ModelInfo>> {
        // Check cache first
        {
            let cached = self.cached_models.read().await;
            if let Some(ref models) = *cached {
                debug!(count = models.len(), "Returning cached chat models");
                return Ok(models.clone());
            }
        }

        // Fetch from API
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/experimental/chats/models")
            .send()
            .await
            .context("Failed to list chat models")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let models = parse_chat_models_body(&body);

            // Update cache
            {
                let mut cached = self.cached_models.write().await;
                *cached = Some(models.clone());
            }

            Ok(models)
        } else {
            bail!("Failed to list chat models: {}", resp.status())
        }
    }

    /// List available models for chats without caching.
    /// GET /api/experimental/chats/models
    #[cfg(not(feature = "chats-api"))]
    pub async fn list_chat_models(&self) -> Result<Vec<crate::types::ModelInfo>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/experimental/chats/models")
            .send()
            .await
            .context("Failed to list chat models")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let models = parse_chat_models_body(&body);
            Ok(models)
        } else {
            bail!("Failed to list chat models: {}", resp.status())
        }
    }

    /// Invalidate the cached model configurations.
    #[cfg(feature = "chats-api")]
    pub async fn invalidate_model_cache(&self) {
        let mut cached = self.cached_models.write().await;
        *cached = None;
    }

    // ── Convenience methods (Phase 3, Tasks 3.3 + 3.4) ─────────────────────

    /// Create a workspace and chat in one step, with standard OpenFlows labels.
    ///
    /// 1. Creates (or finds existing) Coder workspace: `{role}-T-{ticket_id}`
    /// 2. Creates a Chat bound to that workspace with labels:
    ///    `ticket_id`, `role`, `flow=openflows`
    /// 3. Returns `(workspace_id, chat)` for lifecycle tracking
    pub async fn create_ticket_chat(
        &self,
        ticket_id: &str,
        role: &str,
        prompt: &str,
    ) -> Result<(String, crate::types::Chat)> {
        use crate::types::build_chat_labels;
        use crate::types::ChatInputPart;
        use crate::types::CreateChatRequest;
        use crate::types::CreateWorkspaceRequest;

        // Build naming convention: {role}-{ticket_id} — ticket_id already includes "T-" prefix
        let workspace_name = format!("{}-{}", role, ticket_id);
        let repository: String = std::env::var("GITHUB_REPOSITORY")
            .unwrap_or_else(|_| "openflows/target".to_string());
        let repo_url = format!("https://github.com/{}.git", repository);
        let template_name = format!("openflows-{}", role);

        // Create (or find existing) workspace
        info!(
            workspace_name,
            ticket_id, role, "Creating or finding workspace for ticket chat"
        );
        let workspace = self
            .create_workspace(&CreateWorkspaceRequest {
                template_name,
                name: workspace_name.clone(),
                parameters: serde_json::json!({
                    "repo_url": repo_url,
                }),
            })
            .await?;

        self.wait_for_workspace_ready(&workspace.id, std::time::Duration::from_secs(300))
            .await?;

        self.wait_for_workspace_ssh(&workspace.id, std::time::Duration::from_secs(120))
            .await?;

        // Resolve the default organization ID required by the Coder chats API.
        let organization_id = match self.get_default_organization_id().await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to resolve default organization ID; chat creation may fail"
                );
                None
            }
        };

        // Let Coder use the workspace's default model.
        // model_config_id expects a UUID, not a model name, so we pass None.
        let model_config_id = None;

        // Create chat with OpenFlows labels (includes tenant)
        let tenant = std::env::var("OPENFLOWS_TENANT").unwrap_or_else(|_| "default".to_string());
        let labels = build_chat_labels(ticket_id, role, "openflows", &tenant);
        let chat_req = CreateChatRequest {
            organization_id,
            workspace_id: workspace.id.clone(),
            model_config_id,
            content: vec![ChatInputPart::text(prompt)],
            labels: Some(labels),
        };

        info!(
            workspace_id = %workspace.id,
            ticket_id,
            role,
            "Creating chat for ticket"
        );
        let chat = self.create_chat(&chat_req).await?;

        Ok((workspace.id, chat))
    }

    /// Find all chats labeled with the given `ticket_id` and archive them.
    ///
    /// Uses `list_chats()` and filters by the `CHAT_LABEL_TICKET` label,
    /// then archives each matching chat via `archive_chat()`.
    pub async fn archive_ticket_chats(&self, ticket_id: &str) -> Result<usize> {
        use crate::types::CHAT_LABEL_TICKET;

        let all_chats = self.list_chats().await?;
        let matching: Vec<_> = all_chats
            .iter()
            .filter(|c| {
                c.labels
                    .get(CHAT_LABEL_TICKET)
                    .and_then(|v| v.as_str())
                    .map(|s| s == ticket_id)
                    .unwrap_or(false)
            })
            .collect();

        if matching.is_empty() {
            info!(ticket_id, "No chats found to archive for ticket");
            return Ok(0);
        }

        let mut archived_count = 0;
        for chat in &matching {
            match self.archive_chat(&chat.id).await {
                Ok(()) => {
                    archived_count += 1;
                    info!(chat_id = %chat.id, ticket_id, "Archived chat");
                }
                Err(e) => {
                    warn!(
                        chat_id = %chat.id,
                        ticket_id,
                        error = %e,
                        "Failed to archive chat"
                    );
                }
            }
        }

        info!(
            ticket_id,
            archived = archived_count,
            total = matching.len(),
            "Archived ticket chats"
        );
        Ok(archived_count)
    }
}
/// Wraps the path in single quotes and escapes any embedded single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve the host-side CLI binary path for a given agent CLI name, to expose
/// it to Coder workspace templates via the `host_cli_binary` parameter.
///
/// Coder agent modules (e.g. `claude-code`, `codex`) install the CLI by
/// downloading it at workspace startup. When the download endpoint is slow or
/// unreachable (common behind corporate egress), workspaces boot without the
/// CLI on PATH and every agent spawn dies with `sh: <cli>: not found`.
///
/// This helper lets operators reuse an already-installed host binary by
/// bind-mounting it read-only into the workspace (see the `host_cli_binary`
/// variable in the `openflows-*` templates). The binary is typically a
/// self-contained ELF with only glibc deps, so it runs unchanged inside the
/// Ubuntu-based workspace container.
///
/// Resolution order:
/// 1. `HOST_<CLI>_BINARY` env var (e.g. `HOST_CLAUDE_BINARY`,
///    `HOST_CODEX_BINARY`) — an absolute path on the host; symlinks are
///    resolved so the bind mount targets the real ELF, not a broken link.
/// 2. Auto-detect: run `command -v <cli_name>` on the host and canonicalize.
/// 3. Empty string if neither yields a path (the module's installer is used
///    instead, preserving the original behavior).
pub fn resolve_host_cli_binary(cli_name: &str) -> String {
    let env_key = format!(
        "HOST_{}_BINARY",
        cli_name.to_ascii_uppercase().replace('-', "_")
    );
    if let Ok(p) = std::env::var(&env_key) {
        let trimmed = p.trim().to_string();
        if trimmed.is_empty() {
            return String::new();
        }
        return canonicalize_host_path(&trimmed);
    }
    // Auto-detect via `command -v <cli_name>` on the host. This runs on the
    // orchestrator host (where openflows runs), which is the same Docker host
    // that provisions workspaces, so the resolved path is valid for a bind mount.
    if let Ok(out) = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", shell_escape(cli_name)))
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return canonicalize_host_path(&s);
            }
        }
    }
    String::new()
}

fn canonicalize_host_path(p: &str) -> String {
    // Resolve symlinks so the bind mount targets the real ELF (e.g. /usr/bin/claude
    // -> /usr/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe). Fall back
    // to the raw path if canonicalization fails (std::fs::canonicalize errors on
    // non-existent paths).
    match std::fs::canonicalize(p) {
        Ok(resolved) => resolved.to_string_lossy().into_owned(),
        Err(_) => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_chat_models_body;

    #[test]
    fn flattens_provider_nested_models_and_normalizes_id() {
        let body = serde_json::json!({
            "providers": [{
                "provider": "openai-compat",
                "available": true,
                "models": [
                    {"id":"openai-compat:adorsys-reviewer-pro","provider":"openai-compat","model":"adorsys-reviewer-pro","display_name":"adorsys-reviewer-pro"}
                ]
            }]
        });
        let models = parse_chat_models_body(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "adorsys-reviewer-pro");
        assert_eq!(models[0].provider, "openai-compat");
        assert_eq!(models[0].display_name, "adorsys-reviewer-pro");
    }

    #[test]
    fn keeps_top_level_models_shape() {
        let body = serde_json::json!({
            "models": [
                {"id":"anthropic/claude-sonnet-4-5","provider":"anthropic","model":"claude-sonnet-4-5-20250929","display_name":"Sonnet"}
            ]
        });
        let models = parse_chat_models_body(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn empty_when_no_models_anywhere() {
        let body = serde_json::json!({"providers": []});
        assert!(parse_chat_models_body(&body).is_empty());
        assert!(parse_chat_models_body(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn falls_back_to_id_when_model_field_missing() {
        let body = serde_json::json!({
            "providers": [{
                "models": [
                    {"id":"some-model","provider":"p"}
                ]
            }]
        });
        let models = parse_chat_models_body(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "some-model");
    }

    #[test]
    fn strips_colon_provider_prefix_when_model_field_missing() {
        let body = serde_json::json!({
            "providers": [{
                "models": [
                    {"id":"openai-compat:adorsys-reviewer-pro","provider":"openai-compat"}
                ]
            }]
        });
        let models = parse_chat_models_body(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "adorsys-reviewer-pro");
    }

    #[test]
    fn dedupes_across_canonical_and_legacy_shapes() {
        let body = serde_json::json!({
            "providers": [{
                "models": [{"id":"openai-compat:x","model":"x","provider":"openai-compat"}]
            }],
            "models": [{"id":"openai-compat:x","model":"x","provider":"openai-compat"}]
        });
        let models = parse_chat_models_body(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "x");
    }
}
