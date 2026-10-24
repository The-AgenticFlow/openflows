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
use config::{GithubConfig, TenantConfig};
use envconfig::Envconfig;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as SyncRwLock;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Typed HTTP rejection from Coder, without copying its response body into
/// application logs or API responses.
#[derive(Debug)]
pub struct CoderHttpError {
    pub operation: &'static str,
    pub status: reqwest::StatusCode,
}

impl std::fmt::Display for CoderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Coder {} returned HTTP {}", self.operation, self.status)
    }
}

impl std::error::Error for CoderHttpError {}

fn check_template_push_output(name: &str, output: &std::process::Output) -> Result<()> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(
            "coder templates push failed for '{}' ({}):\n{}\n{}",
            name,
            output.status,
            stderr,
            stdout
        );
    } else {
        info!(name, stderr = %stderr, "coder templates push succeeded");
    }
    Ok(())
}

/// Values forwarded to server-side Terraform and included in bootstrap caching.
/// Terraform variable values configured by the operator for template imports.
/// They are sent to Coder with each managed template version so its server-side
/// provisioner sees the same configuration as the local bootstrap CLI.
pub fn template_variable_values() -> Vec<(&'static str, String)> {
    [
        ("TF_VAR_dev_binary_host_path", "dev_binary_host_path"),
        ("TF_VAR_docker_network", "docker_network"),
        ("TF_VAR_workspace_image", "workspace_image"),
        ("TF_VAR_github_api_base", "github_api_base"),
        ("TF_VAR_github_git_base", "github_git_base"),
    ]
    .into_iter()
    .filter_map(|(env_name, name)| {
        std::env::var(env_name)
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| (name, value))
    })
    .collect()
}

fn resolve_coder_cli() -> PathBuf {
    if let Some(path) = std::env::var_os("CODER_CLI_PATH").map(PathBuf::from) {
        if path.is_file() {
            return path;
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("coder");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    if let Some(bin_dir) = std::env::var_os("CODER_SCRIPT_BIN_DIR").map(PathBuf::from) {
        let candidate = bin_dir.join("coder");
        if candidate.is_file() {
            return candidate;
        }
    }

    // Coder workspace startup places its helper CLI under /tmp/coder.*/coder
    // without always adding it to PATH. The controller runs inside that same
    // workspace, so discover it before falling back to a plain PATH lookup.
    if let Ok(entries) = std::fs::read_dir("/tmp") {
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join("coder"))
            .filter(|path| path.is_file())
            .collect();
        candidates.sort();
        if let Some(candidate) = candidates.into_iter().next() {
            return candidate;
        }
    }

    PathBuf::from("coder")
}

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
    /// Shared, rotated platform credential. Only Manager clients set this.
    shared_token: Option<Arc<SyncRwLock<String>>>,
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
    /// Cached default organization ID.
    /// Lazily populated on first access to reduce API call overhead.
    /// Not gated behind `chats-api`: `get_default_organization_id` is used by
    /// agent-nexus regardless of that feature, and the field is unconditional.
    cached_org_id: Arc<RwLock<Option<String>>>,
    /// Cached current username, resolved via `get_me()` from the active token.
    /// Lazily populated on first access; prevents the "admin" hardcode from
    /// being used when a different user's token is configured.
    cached_username: Arc<RwLock<Option<String>>>,
}

/// Parse the JSON body returned by `GET /api/v2/organizations/{org}/chats/models`.
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
            shared_token: None,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            workspace_name: None,
            session_token: None,
            #[cfg(feature = "chats-api")]
            cached_models: Arc::new(RwLock::new(None)),
            cached_org_id: Arc::new(RwLock::new(None)),
            cached_username: Arc::new(RwLock::new(None)),
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

    /// Use a credential that the Manager can rotate while workers keep running.
    pub fn with_shared_token(mut self, token: Arc<SyncRwLock<String>>) -> Self {
        self.shared_token = Some(token);
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
            shared_token: None,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            workspace_name: None,
            session_token: None,
            #[cfg(feature = "chats-api")]
            cached_models: Arc::new(RwLock::new(None)),
            cached_org_id: Arc::new(RwLock::new(None)),
            cached_username: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a client with a different token (e.g., after bootstrap).
    pub fn with_token(&self, token: String) -> Self {
        Self {
            base_url: self.base_url.clone(),
            token,
            shared_token: None,
            http: self.http.clone(),
            workspace_name: self.workspace_name.clone(),
            session_token: self.session_token.clone(),
            #[cfg(feature = "chats-api")]
            cached_models: self.cached_models.clone(),
            cached_org_id: self.cached_org_id.clone(),
            cached_username: self.cached_username.clone(),
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

    /// Get the session token (if set). This is used for both REST API
    /// authentication and SSH operations. Falls back to the API token
    /// when no session token has been configured.
    pub fn session_token(&self) -> &str {
        self.session_token.as_deref().unwrap_or(&self.token)
    }

    fn authenticated_request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, self.url(path));
        let token = self.shared_token.as_ref().map(|token| {
            token
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        });
        let token = token.as_deref().unwrap_or(&self.token);
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
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
                .get(self.url("/api/v2/buildinfo"))
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
            .post(self.url("/api/v2/users/login"))
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

    /// Perform an unauthenticated GET returning the raw JSON value, used by the
    /// manager adapter for read-only probes such as `GET /api/v2/buildinfo`.
    pub async fn raw_get(&self, path: &str) -> Result<serde_json::Value> {
        let resp = self
            .http
            .get(self.url(path))
            .send()
            .await
            .context("Failed to perform raw GET")?;
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            bail!("raw GET {} failed with {}", path, status)
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

    /// Get the default organization ID with caching.
    ///
    /// Queries `GET /api/v2/organizations` on first call and caches the result.
    /// Subsequent calls return the cached value to reduce API overhead.
    /// Returns an error if no organizations are found at all.
    pub async fn get_default_organization_id(&self) -> Result<String> {
        // Check cache first
        {
            let cache = self.cached_org_id.read().await;
            if let Some(ref cached_id) = *cache {
                debug!("Using cached organization ID: {}", cached_id);
                return Ok(cached_id.clone());
            }
        }

        // Fetch from API
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
        let org_id = default_org.id.clone();
        info!(
            org_id = %org_id,
            org_name = %default_org.name,
            is_default = default_org.is_default,
            "Resolved and cached default organization"
        );

        // Cache the result
        {
            let mut cache = self.cached_org_id.write().await;
            *cache = Some(org_id.clone());
        }

        Ok(org_id)
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
        let mut cmd = tokio::process::Command::new(resolve_coder_cli());
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

        // Pass selected TF_VAR_* values as --variable flags so the Coder server
        // stores them and applies them when creating workspaces. Setting them
        // as env vars in the Rust process does NOT reach the server's Terraform
        // execution — the CLI only uploads the template files.
        for (variable_name, value) in template_variable_values() {
            cmd.arg("--variable")
                .arg(format!("{}={}", variable_name, value));
        }

        let output = match cmd.output().await {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!(
                    "Failed to run coder templates push: the `coder` CLI is not on your PATH. \
                     Install it (curl -fsSL https://coder.com/install.sh | sh) and re-run bootstrap."
                )
            }
            Err(e) => {
                anyhow::bail!("Failed to run coder templates push: {e}")
            }
        };

        // Clean up even when the CLI rejects the template.
        let _ = std::fs::remove_dir_all(&temp_dir);
        check_template_push_output(name, &output)?;

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
                            updated_at: v
                                .get("updated_at")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            organization_id: v
                                .get("organization_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            active_version_id: v
                                .get("active_version_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
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

    /// Create a Coder organization. Hosted provisioning uses a
    /// deterministic internal name derived from the Openflows organization id.
    pub async fn create_organization(
        &self,
        req: &crate::types::CreateOrganizationRequest,
    ) -> Result<CoderOrganization> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/organizations")
            .json(req)
            .send()
            .await
            .context("Failed to create Coder organization")?;
        if resp.status().is_success() {
            let org: CoderOrganization = resp.json().await?;
            if org.id.is_empty() {
                bail!("Coder organization created without an id");
            }
            Ok(org)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create Coder organization ({}): {}", status, body)
        }
    }

    /// Fetch a single organization by id and verify the authenticated token can
    /// see it. Returns `None` when the org is absent or not visible.
    pub async fn get_organization(
        &self,
        org_id: &CoderOrganizationId,
    ) -> Result<Option<CoderOrganization>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}", org_id.0),
            )
            .send()
            .await
            .context("Failed to get Coder organization")?;
        if resp.status().is_success() {
            let org: CoderOrganization = resp.json().await?;
            if org.id != org_id.0 {
                bail!(
                    "Coder returned organization '{}' for requested '{}'",
                    org.id,
                    org_id.0
                );
            }
            Ok(Some(org))
        } else if resp.status().as_u16() == 404 {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get Coder organization ({}): {}", status, body)
        }
    }

    /// List templates within an explicit organization. The org is always
    /// supplied; hosted code never resolves the default organization.
    pub async fn list_templates_in_org(
        &self,
        org_id: &CoderOrganizationId,
    ) -> Result<Vec<CoderTemplate>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}/templates", org_id.0),
            )
            .send()
            .await
            .context("Failed to list Coder templates in org")?;
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(CoderHttpError {
                operation: "list organization templates",
                status: resp.status(),
            }
            .into())
        }
    }

    /// Fetch a template by id within an explicit organization, verifying the
    /// returned resource belongs to that organization. Returns `None` when
    /// absent or outside the org.
    pub async fn get_template(
        &self,
        org_id: &CoderOrganizationId,
        template_id: &CoderTemplateId,
    ) -> Result<Option<CoderTemplate>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!(
                    "/api/v2/organizations/{}/templates/{}",
                    org_id.0, template_id.0
                ),
            )
            .send()
            .await
            .context("Failed to get Coder template")?;
        if resp.status().is_success() {
            let tpl: CoderTemplate = resp.json().await?;
            if !tpl.organization_id.is_empty() && tpl.organization_id != org_id.0 {
                bail!(
                    "Coder template '{}' belongs to org '{}', not '{}'",
                    tpl.id,
                    tpl.organization_id,
                    org_id.0
                );
            }
            Ok(Some(tpl))
        } else if resp.status().as_u16() == 404 {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get Coder template ({}): {}", status, body)
        }
    }

    /// List template versions by template id and verify both organization and
    /// template identity. Coder's list endpoint is global by template UUID;
    /// there is no organization-prefixed list route in the v2 API.
    pub async fn list_template_versions(
        &self,
        org_id: &CoderOrganizationId,
        template_id: &CoderTemplateId,
    ) -> Result<Vec<CoderTemplateVersion>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/templates/{}/versions", template_id.0),
            )
            .send()
            .await
            .context("Failed to list Coder template versions")?;
        if resp.status().is_success() {
            let versions: Vec<CoderTemplateVersion> = resp.json().await?;
            for version in &versions {
                if !version.organization_id.is_empty() && version.organization_id != org_id.0 {
                    bail!(
                        "Coder template version belongs to organization '{}', expected '{}'",
                        version.organization_id,
                        org_id.0
                    );
                }
                if !version.template_id.is_empty() && version.template_id != template_id.0 {
                    bail!(
                        "Coder template version belongs to template '{}', expected '{}'",
                        version.template_id,
                        template_id.0
                    );
                }
            }
            Ok(versions)
        } else {
            Err(CoderHttpError {
                operation: "list template versions",
                status: resp.status(),
            }
            .into())
        }
    }

    /// Upload a template archive and create a template version in an explicit
    /// organization. The version name is deterministic (the release id) so
    /// publication is idempotent. Returns the created version (which carries the
    /// import job id) or the import job id.
    pub async fn create_template_version(
        &self,
        org_id: &CoderOrganizationId,
        template_id: Option<&CoderTemplateId>,
        version_name: &str,
        archive: &[u8],
    ) -> Result<CoderTemplateVersion> {
        self.create_template_version_with_variables(org_id, template_id, version_name, archive, &[])
            .await
    }

    /// Create a template version with Terraform variables persisted by Coder
    /// for its server-side provisioner.
    pub async fn create_template_version_with_variables(
        &self,
        org_id: &CoderOrganizationId,
        template_id: Option<&CoderTemplateId>,
        version_name: &str,
        archive: &[u8],
        variables: &[crate::types::TemplateVariableValue],
    ) -> Result<CoderTemplateVersion> {
        let mut decoder = flate2::read::GzDecoder::new(archive);
        let mut tar_archive = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut tar_archive)
            .context("Failed to decode packaged Coder template")?;
        let upload_response = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/files")
            .header("Content-Type", "application/x-tar")
            .body(tar_archive)
            .send()
            .await
            .context("Failed to upload Coder template archive")?;
        if !upload_response.status().is_success() {
            return Err(CoderHttpError {
                operation: "upload template archive",
                status: upload_response.status(),
            }
            .into());
        }
        let uploaded: CoderUploadedFile = upload_response.json().await?;
        if uploaded.id.is_empty() {
            bail!("Coder file upload returned no archive hash");
        }
        let request = CreateTemplateVersionRequest {
            name: version_name.to_string(),
            message: format!("OpenFlows managed template release {version_name}"),
            template_id: template_id.map(|id| id.0.clone()),
            storage_method: "file".into(),
            file_id: uploaded.id,
            provisioner: "terraform".into(),
            tags: std::collections::HashMap::new(),
            user_variable_values: variables.to_vec(),
        };
        let response = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/organizations/{}/templateversions", org_id.0),
            )
            .json(&request)
            .send()
            .await
            .context("Failed to create Coder template version")?;
        if !response.status().is_success() {
            return Err(CoderHttpError {
                operation: "create template version",
                status: response.status(),
            }
            .into());
        }
        let version: CoderTemplateVersion = response.json().await?;
        if version.id.is_empty() {
            bail!("Coder template version created without an id");
        }
        Ok(version)
    }

    /// Fetch a template version by id and verify it belongs to the expected org.
    /// Coder exposes the import/provisioner job nested in this response; there
    /// is no organization-scoped `/imports/{job_id}` endpoint in the pinned API.
    pub async fn get_template_version(
        &self,
        org_id: &CoderOrganizationId,
        version_id: &str,
    ) -> Result<Option<CoderTemplateVersion>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/templateversions/{version_id}"),
            )
            .send()
            .await
            .context("Failed to get Coder template version")?;
        if resp.status().is_success() {
            let version: CoderTemplateVersion = resp.json().await?;
            if !version.organization_id.is_empty() && version.organization_id != org_id.0 {
                bail!(
                    "Coder template version belongs to organization '{}', expected '{}'",
                    version.organization_id,
                    org_id.0
                );
            }
            Ok(Some(version))
        } else if resp.status().as_u16() == 404 {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to get Coder template version ({}): {}",
                status,
                body
            )
        }
    }

    /// Create a template within an explicit organization, optionally binding it
    /// to an initial active version. Returns the created template.
    pub async fn create_template(
        &self,
        org_id: &CoderOrganizationId,
        req: &crate::types::CreateTemplateRequest,
    ) -> Result<CoderTemplateCreated> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/organizations/{}/templates", org_id.0),
            )
            .json(req)
            .send()
            .await
            .context("Failed to create Coder template")?;
        if resp.status().is_success() {
            let created: CoderTemplateCreated = resp.json().await?;
            if created.id.is_empty() {
                bail!("Coder template created without an id");
            }
            Ok(created)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create Coder template ({}): {}", status, body)
        }
    }

    /// Create an organization-scoped provisioner key. The returned secret is
    /// treated as a credential and is returned only once.
    pub async fn create_provisioner_key(
        &self,
        org_id: &CoderOrganizationId,
        name: &str,
    ) -> Result<CoderProvisionerKey> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/organizations/{}/provisionerkeys", org_id.0),
            )
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("Failed to create Coder provisioner key")?;
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to create Coder provisioner key ({}): {}",
                status,
                body
            )
        }
    }

    /// Remove a provisioner key reserved by the caller. Missing keys are already
    /// reconciled; other failures must not be mistaken for absence.
    pub async fn delete_provisioner_key(
        &self,
        org: &CoderOrganizationId,
        name: &str,
    ) -> Result<()> {
        let response = self
            .authenticated_request(
                reqwest::Method::DELETE,
                &format!("/api/v2/organizations/{}/provisionerkeys/{}", org.0, name),
            )
            .send()
            .await
            .context("Failed to delete Coder provisioner key")?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            bail!(
                "Coder provisioner key cleanup failed ({})",
                response.status()
            )
        }
    }

    /// List provisioner daemons registered in an explicit organization, used to
    /// verify a provisioner's heartbeat/readiness before marking the org ready.
    pub async fn list_provisioner_daemons(
        &self,
        org_id: &CoderOrganizationId,
    ) -> Result<Vec<CoderProvisionerDaemon>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}/provisionerdaemons", org_id.0),
            )
            .send()
            .await
            .context("Failed to list Coder provisioner daemons")?;
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to list Coder provisioner daemons ({}): {}",
                status,
                body
            )
        }
    }

    /// List chat models scoped to an explicit organization (never the default
    /// org). Used by the readiness reconciliation to verify org-scoped model
    /// availability and Agents/chat capability.
    pub async fn list_chat_models_in_org(
        &self,
        org_id: &CoderOrganizationId,
    ) -> Result<Vec<crate::types::ModelInfo>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}/chats/models", org_id.0),
            )
            .send()
            .await
            .context("Failed to list Coder chat models in org")?;
        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            Ok(parse_chat_models_body(&body))
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to list Coder chat models in org ({}): {}",
                status,
                body
            )
        }
    }

    /// Create a workspace with explicit organization, template, version, and
    /// owner. Never falls back to the default org or current user.
    pub async fn create_workspace_explicit(
        &self,
        owner_user_id: &str,
        req: &crate::types::CreateWorkspaceRequestExplicit,
    ) -> Result<CoderWorkspaceDetail> {
        let payload = serde_json::json!({
            // Coder requires exactly one of template_id and
            // template_version_id. Tenant provisioning is pinned to the
            // already-resolved version, so send only that field.
            "template_version_id": req.template_version_id,
            "name": req.name,
            "rich_parameter_values": req.rich_parameter_values.as_deref().unwrap_or_default(),
        });
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!(
                    "/api/v2/organizations/{}/members/{}/workspaces",
                    req.organization_id, owner_user_id
                ),
            )
            .json(&payload)
            .send()
            .await
            .context("Failed to create workspace with explicit scope")?;
        if resp.status().is_success() {
            let ws: CoderWorkspaceDetail = resp.json().await?;
            if !ws.organization_id.is_empty() && ws.organization_id != req.organization_id {
                bail!(
                    "Coder returned workspace in org '{}', expected '{}'",
                    ws.organization_id,
                    req.organization_id
                );
            }
            Ok(ws)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create workspace ({}): {}", status, body)
        }
    }

    /// Fetch a workspace by its immutable owner/name route. Hosted retry
    /// recovery uses this instead of creating a second workspace after a
    /// timeout whose response may have been lost.
    pub async fn get_workspace_by_owner_and_name(
        &self,
        owner_user_id: &str,
        workspace_name: &str,
    ) -> Result<Option<CoderWorkspaceDetail>> {
        let owner: String =
            url::form_urlencoded::byte_serialize(owner_user_id.as_bytes()).collect();
        let name: String =
            url::form_urlencoded::byte_serialize(workspace_name.as_bytes()).collect();
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/users/{owner}/workspace/{name}"),
            )
            .send()
            .await
            .context("Failed to find workspace by owner and name")?;
        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else if matches!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE
        ) {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to find workspace ({status}): {body}")
        }
    }

    /// Fetch a Coder user detail including organization membership, used to
    /// verify a machine owner belongs only to its mapped organization.
    pub async fn get_user_detail(&self, user_id: &str) -> Result<Option<CoderUserDetail>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/users/{}", user_id))
            .send()
            .await
            .context("Failed to get Coder user detail")?;
        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else if resp.status().as_u16() == 404 {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get Coder user detail ({}): {}", status, body)
        }
    }

    /// Verify a linked human account and grant only basic organization membership.
    pub async fn ensure_personal_org_member(
        &self,
        org: &CoderOrganizationId,
        user_id: &str,
    ) -> Result<()> {
        let response = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/users/{user_id}"))
            .send()
            .await?
            .error_for_status()?;
        let user: serde_json::Value = response.json().await?;
        if user["id"].as_str() != Some(user_id)
            || user["is_service_account"].as_bool().unwrap_or(false)
            || user["login_type"].as_str() != Some("github")
            || user["status"].as_str() != Some("active")
        {
            bail!("CODER_OWNER_INACTIVE: linked owner must be an active GitHub user");
        }
        if user["organization_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&org.0)))
        {
            return Ok(());
        }
        let response = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/organizations/{}/members/{user_id}", org.0),
            )
            .send()
            .await?;
        if !response.status().is_success() && response.status().as_u16() != 409 {
            bail!("CODER_OWNER_MEMBERSHIP_FAILED: {}", response.status());
        }
        let verified = self
            .get_user_detail(user_id)
            .await?
            .context("Coder owner disappeared")?;
        if !verified.organization_ids.contains(&org.0) {
            bail!("CODER_OWNER_MEMBERSHIP_FAILED");
        }
        Ok(())
    }

    /// Create an organization-scoped service account. The caller must verify
    /// the returned organization membership before using it as a workspace
    /// owner.
    pub async fn create_service_account(
        &self,
        request: &crate::types::CreateServiceAccountRequest,
    ) -> Result<CoderUserDetail> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/users")
            .json(request)
            .send()
            .await
            .context("Failed to create Coder service account")?;
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to create Coder service account ({}): {}",
                status,
                body
            )
        }
    }

    /// Delete a service account after all of its tenant workspaces are gone.
    /// Missing users are treated as already reconciled.
    pub async fn delete_user(&self, user_id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(reqwest::Method::DELETE, &format!("/api/v2/users/{user_id}"))
            .send()
            .await
            .context("Failed to delete Coder user")?;
        if resp.status().is_success()
            || matches!(
                resp.status(),
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE
            )
        {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to delete Coder user ({status}): {body}")
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
                    .map(|(k, v)| {
                        let value_str = if v.is_string() {
                            v.as_str().unwrap_or("").to_string()
                        } else {
                            // For non-string values, convert to JSON string representation
                            v.to_string().trim_matches('"').to_string()
                        };
                        serde_json::json!({"name": k, "value": value_str})
                    })
                    .collect(),
            )
        } else {
            serde_json::Value::Array(vec![])
        };

        let payload = serde_json::json!({
            "template_id": template_id,
            "name": req.name,
            "rich_parameter_values": rich_parameter_values,
        });

        debug!(
            workspace_name = %req.name,
            "Creating workspace with payload"
        );

        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/users/{}/workspaces", user_id),
            )
            .json(&payload)
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

    /// Restart through a stop build followed by a start build. A start-only
    /// Terraform apply can retain the existing container and never rerun its
    /// startup script. Wait for pending builds before submitting a transition.
    pub async fn restart_workspace(&self, id: &str) -> Result<()> {
        let status = self.wait_workspace_build_terminal(id).await?;
        if matches!(status.as_str(), "deleted" | "deleting") {
            bail!("Cannot restart a deleted workspace");
        }
        if status != "stopped" {
            self.submit_workspace_transition(id, "stop").await?;
            let status = self.wait_workspace_build_terminal(id).await?;
            if status != "stopped" {
                bail!("Workspace stop did not succeed: {}", status);
            }
        }
        self.submit_workspace_transition(id, "start").await
    }

    async fn wait_workspace_build_terminal(&self, id: &str) -> Result<String> {
        tokio::time::timeout(Duration::from_secs(15 * 60), async {
            loop {
                let workspace = self.get_workspace(id).await?;
                let status = workspace
                    .latest_build
                    .as_ref()
                    .map(|build| build.status.as_str())
                    .filter(|status| !status.is_empty())
                    .unwrap_or(&workspace.status);
                match status {
                    "running" | "stopped" | "failed" | "canceled" | "deleted" => {
                        return Ok(status.to_string())
                    }
                    "pending" | "starting" | "stopping" | "canceling" | "deleting" => (),
                    _ => bail!("Unexpected workspace build status: {}", status),
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        })
        .await
        .context("Timed out waiting for workspace build")?
    }

    async fn submit_workspace_transition(&self, id: &str, transition: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/workspaces/{}/builds", id),
            )
            .json(&serde_json::json!({ "transition": transition }))
            .send()
            .await
            .context("Failed to restart workspace")?;

        if resp.status().is_success() {
            info!(workspace_id = id, "Restarted workspace");
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to restart workspace ({}): {}", status, body)
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

        if resp.status().is_success()
            || resp.status() == reqwest::StatusCode::NOT_FOUND
            || resp.status() == reqwest::StatusCode::GONE
        {
            info!(
                workspace_id = id,
                status = %resp.status(),
                "Stopped workspace"
            );
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to stop workspace: {}", body)
        }
    }

    /// Delete a workspace by triggering a delete build and waiting for deletion to complete.
    pub async fn delete_workspace(&self, id: &str) -> Result<()> {
        let current = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/workspaces/{id}"))
            .send()
            .await
            .context("Failed to check workspace before deletion")?;
        if matches!(current.status().as_u16(), 404 | 410) {
            info!(workspace_id = id, status = %current.status(), "Workspace already deleted");
            return Ok(());
        }
        if !current.status().is_success() {
            return Err(CoderHttpError {
                operation: "check workspace before deletion",
                status: current.status(),
            }
            .into());
        }
        // A prior attempt may have completed after Manager gave up waiting.
        // Confirm Coder's resource state before asking it to start another
        // delete build; some Coder versions reject duplicate transitions.
        let workspace: CoderWorkspace = current
            .json()
            .await
            .context("Failed to read workspace before deletion")?;
        if workspace.workspace_status() == WorkspaceStatus::Deleted
            || workspace.latest_build.as_ref().is_some_and(|build| {
                build.transition.as_deref() == Some("delete") && build.status == "succeeded"
            })
        {
            info!(workspace_id = id, "Workspace already deleted");
            return Ok(());
        }
        if workspace.latest_build.as_ref().is_some_and(|build| {
            build.transition.as_deref() == Some("delete")
                && matches!(build.status.as_str(), "pending" | "running")
        }) {
            return self
                .wait_for_workspace_deleted(id, Duration::from_secs(60))
                .await;
        }
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/workspaces/{}/builds", id),
            )
            .json(&serde_json::json!({
                "transition": "delete"
            }))
            .send()
            .await
            .context("Failed to delete workspace")?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND
            || resp.status() == reqwest::StatusCode::GONE
        {
            info!(
                workspace_id = id,
                status = %resp.status(),
                "Workspace already deleted"
            );
            return Ok(());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            // Coder can reject a repeated delete build even though an earlier
            // attempt already deleted the workspace. Confirm the final state
            // before treating a conflict or validation rejection as failure.
            if matches!(status.as_u16(), 400 | 409 | 422) {
                let check = self
                    .authenticated_request(
                        reqwest::Method::GET,
                        &format!("/api/v2/workspaces/{id}"),
                    )
                    .send()
                    .await
                    .context("Failed to verify workspace deletion")?;
                if matches!(check.status().as_u16(), 404 | 410) {
                    info!(workspace_id = id, %status, "Workspace already deleted");
                    return Ok(());
                }
                if check.status().is_success() {
                    let workspace: CoderWorkspace = check
                        .json()
                        .await
                        .context("Failed to verify workspace deletion")?;
                    if workspace.workspace_status() == WorkspaceStatus::Deleted
                        || workspace.latest_build.as_ref().is_some_and(|build| {
                            build.transition.as_deref() == Some("delete")
                                && build.status == "succeeded"
                        })
                    {
                        info!(workspace_id = id, %status, "Workspace already deleted");
                        return Ok(());
                    }
                    if workspace.latest_build.as_ref().is_some_and(|build| {
                        build.transition.as_deref() == Some("delete")
                            && matches!(build.status.as_str(), "pending" | "running")
                    }) {
                        return self
                            .wait_for_workspace_deleted(id, Duration::from_secs(60))
                            .await;
                    }
                }
            }
            return Err(CoderHttpError {
                operation: "delete workspace",
                status,
            }
            .into());
        }

        self.wait_for_workspace_deleted(id, Duration::from_secs(60))
            .await
    }

    /// Wait until a workspace is confirmed deleted (returns 404, 410, or reports status "deleted").
    /// If the delete build fails or cancels, this method returns an error.
    pub async fn wait_for_workspace_deleted(&self, id: &str, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        info!(
            workspace_id = id,
            "Waiting for workspace deletion to complete"
        );
        while start.elapsed() < timeout {
            let resp = self
                .authenticated_request(reqwest::Method::GET, &format!("/api/v2/workspaces/{}", id))
                .send()
                .await;

            match resp {
                Ok(r)
                    if r.status() == reqwest::StatusCode::NOT_FOUND
                        || r.status() == reqwest::StatusCode::GONE =>
                {
                    info!(
                        workspace_id = id,
                        status = %r.status(),
                        "Workspace confirmed deleted"
                    );
                    return Ok(());
                }
                Ok(r) if r.status().is_success() => match r.json::<CoderWorkspace>().await {
                    Ok(ws) => {
                        let status = ws.workspace_status();
                        let build_status = ws.latest_build.as_ref().map(|b| b.status.as_str());
                        let is_delete_build = ws
                            .latest_build
                            .as_ref()
                            .and_then(|b| b.transition.as_deref())
                            == Some("delete");

                        if status == WorkspaceStatus::Deleted
                            || ws.status == "deleted"
                            || (is_delete_build && build_status == Some("succeeded"))
                        {
                            info!(workspace_id = id, "Workspace confirmed deleted");
                            return Ok(());
                        }

                        if status == WorkspaceStatus::Failed
                            || (is_delete_build
                                && matches!(build_status, Some("failed" | "canceled")))
                        {
                            bail!(
                                    "Delete build failed for workspace {}: workspace status is {:?}, build status is {:?}",
                                    id,
                                    status,
                                    build_status
                                );
                        }

                        debug!(
                            workspace_id = id,
                            status = ?status,
                            build_status = ?build_status,
                            "Workspace deletion in progress, waiting..."
                        );
                    }
                    Err(e) => {
                        debug!(
                            error = %e,
                            "Failed to parse workspace status while waiting for deletion"
                        );
                    }
                },
                Ok(r) => {
                    debug!(
                        workspace_id = id,
                        status = %r.status(),
                        "Unexpected status code checking workspace deletion, retrying..."
                    );
                }
                Err(e) => {
                    debug!(
                        error = %e,
                        "Error checking workspace deletion status, retrying..."
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!(
            "Workspace {} deletion did not complete within {:?}",
            id,
            timeout
        )
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
        let wrapped = format!(
            "cd /home/coder/workspace && echo '{}' | base64 -d | bash -l",
            b64
        );

        let ssh_token = self.session_token();
        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::process::Command::new(resolve_coder_cli())
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

// External-auth (GitHub link) API

impl CoderClient {
    /// List the external-auth links for the current token's user.
    /// `GET /api/v2/external-auth`.
    pub async fn list_external_auth_links(&self) -> Result<Vec<crate::ExternalAuthLink>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/external-auth")
            .send()
            .await
            .context("Failed to list external-auth links")?;

        if resp.status().is_success() {
            let links: Vec<crate::ExternalAuthLink> = resp.json().await?;
            Ok(links)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to list external-auth links ({}): {}", status, body)
        }
    }

    /// Get the link status for a single external-auth provider (e.g.
    /// `primary-github`). `GET /api/v2/external-auth/{externalauth}`.
    pub async fn get_external_auth(&self, id: &str) -> Result<crate::ExternalAuth> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/external-auth/{}", id),
            )
            .send()
            .await
            .context("Failed to get external-auth status")?;

        if resp.status().is_success() {
            let auth: crate::ExternalAuth = resp.json().await?;
            Ok(auth)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get external-auth status ({}): {}", status, body)
        }
    }

    /// Retrieve the pending device-flow link details for a provider. Returns
    /// the verification URI and codes the user needs to complete linking
    /// without a dashboard click. `GET /api/v2/external-auth/{externalauth}/device`.
    ///
    /// The POST variant of this endpoint performs the *exchange* of a device
    /// code for a token (a 204 with an empty body); the device-details payload
    /// is only returned by GET, so the device flow is initiated with GET here.
    pub async fn get_external_auth_device(&self, id: &str) -> Result<crate::ExternalAuthDevice> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/external-auth/{}/device", id),
            )
            .send()
            .await
            .context("Failed to retrieve external-auth device flow")?;

        if resp.status().is_success() {
            let device: crate::ExternalAuthDevice = resp.json().await?;
            Ok(device)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to retrieve external-auth device flow ({}): {}",
                status,
                body
            )
        }
    }
}

// Chats API (Phase 3)

impl CoderClient {
    /// Resolve the real username from the Coder server using the current token.
    /// Caches the result so `current_username()` and subsequent calls are free.
    pub async fn resolve_current_user(&self) -> Result<String> {
        {
            let guard = self.cached_username.read().await;
            if let Some(ref name) = *guard {
                return Ok(name.clone());
            }
        }
        let me = self.get_me().await?;
        let mut guard = self.cached_username.write().await;
        guard.replace(me.username.clone());
        Ok(me.username)
    }

    /// Get the current user's username (for constructing chat API paths).
    ///
    /// Resolution order:
    /// 1. Cached username from a previous [`resolve_current_user`] call.
    /// 2. Owner portion of `workspace_name` ("owner/workspace" format).
    /// 3. Fallback to `"admin"` (legacy / unauthenticated clients).
    ///
    /// Callers that need a guaranteed-correct identity should call
    /// [`resolve_current_user`] before using this function.
    ///
    /// [`resolve_current_user`]: CoderClient::resolve_current_user
    pub fn current_username(&self) -> String {
        if let Ok(guard) = self.cached_username.try_read() {
            if let Some(ref name) = *guard {
                return name.clone();
            }
        }
        if let Some(ref ws) = self.workspace_name {
            if let Some((owner, _)) = ws.split_once('/') {
                return owner.to_string();
            }
        }
        "admin".to_string()
    }

    /// Create a new Chat session bound to a workspace.
    /// POST /api/v2/chats
    pub async fn create_chat(
        &self,
        req: &crate::types::CreateChatRequest,
    ) -> Result<crate::types::Chat> {
        let resp = self
            .authenticated_request(reqwest::Method::POST, "/api/v2/chats")
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
    /// GET /api/v2/chats/{chat}
    ///
    /// Returns `Err` for transient failures (timeouts, rate limits, 5xx,
    /// network errors). Callers that need to distinguish "the chat no longer
    /// exists" from "the API is temporarily unavailable" should use
    /// [`get_chat_opt`], which maps a 404 to `Ok(None)` so a deleted chat can
    /// be safely rotated without dropping an active chat on a transient blip.
    ///
    /// [`get_chat_opt`]: CoderClient::get_chat_opt
    pub async fn get_chat(&self, chat_id: &str) -> Result<crate::types::Chat> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/chats/{}", chat_id))
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

    /// Like [`get_chat`], but a 404 ("chat not found") is returned as
    /// `Ok(None)` instead of `Err`. A 404 means the chat resource has been
    /// deleted and the stored ID is permanently stale — callers should rotate
    /// it. Any other non-success status (timeout/rate-limit/5xx) is returned
    /// as `Err` so callers can leave the existing chat ID in place and retry
    /// on the next poll, rather than creating a duplicate chat.
    ///
    /// [`get_chat`]: CoderClient::get_chat
    pub async fn get_chat_opt(&self, chat_id: &str) -> Result<Option<crate::types::Chat>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, &format!("/api/v2/chats/{}", chat_id))
            .send()
            .await
            .context("Failed to get chat")?;

        let status = resp.status();
        if status.is_success() {
            Ok(Some(resp.json().await?))
        } else if status.as_u16() == reqwest::StatusCode::NOT_FOUND.as_u16() {
            Ok(None)
        } else {
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get chat ({}): {}", status, body)
        }
    }

    /// List all Chats for the current user.
    /// GET /api/v2/chats
    pub async fn list_chats(&self) -> Result<Vec<crate::types::Chat>> {
        let resp = self
            .authenticated_request(reqwest::Method::GET, "/api/v2/chats")
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
    /// A running chat may accept the message into its queue. In that case the
    /// returned message contains the queue entry ID, not a conversation message ID.
    /// POST /api/v2/chats/{chat_id}/messages
    pub async fn send_chat_message(
        &self,
        chat_id: &str,
        content: Vec<crate::types::ChatInputPart>,
    ) -> Result<crate::types::ChatMessage> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/chats/{}/messages", chat_id),
            )
            .json(&serde_json::json!({
                "content": content,
            }))
            .send()
            .await
            .context("Failed to send chat message")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let queued = body.get("queued").and_then(|value| value.as_bool()) == Some(true);
            // Coder v2.37 returns an envelope for both immediate and queued sends.
            // Prefer the queue entry when present: an envelope can also include
            // older messages that were promoted from the queue during this send.
            let payload = if queued {
                body.get("queued_message")
                    .context("Accepted queued chat message response is missing queued_message")?
            } else {
                body.get("message").unwrap_or(&body)
            };
            let mut msg: crate::types::ChatMessage = serde_json::from_value(payload.clone())
                .context("Invalid accepted chat message response")?;
            if queued {
                msg.role = "user".to_string();
            }
            info!(chat_id, message_id = %msg.id, queued, "Sent chat message");
            Ok(msg)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to send chat message ({}): {}", status, body)
        }
    }

    /// Get recent messages from a Chat.
    /// GET /api/v2/chats/{chat_id}/messages?after_id={id}&limit={n}
    pub async fn get_chat_messages(
        &self,
        chat_id: &str,
        limit: u64,
    ) -> Result<Vec<crate::types::ChatMessage>> {
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/chats/{}/messages?limit={}", chat_id, limit),
            )
            .send()
            .await
            .context("Failed to get chat messages")?;

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            let messages = body
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            Ok(messages)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to get chat messages ({}): {}", status, body)
        }
    }

    /// Archive a Chat (soft delete).
    /// PATCH /api/v2/chats/{chat_id}
    pub async fn archive_chat(&self, chat_id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::PATCH,
                &format!("/api/v2/chats/{}", chat_id),
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
    /// POST /api/v2/chats/{chat_id}/interrupt
    pub async fn interrupt_chat(&self, chat_id: &str) -> Result<()> {
        let resp = self
            .authenticated_request(
                reqwest::Method::POST,
                &format!("/api/v2/chats/{}/interrupt", chat_id),
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
    /// GET /api/v2/organizations/{organization}/chats/models
    ///
    /// Models are organization-scoped in Coder v2.37+; the default organization
    /// is resolved via [`CoderClient::get_default_organization_id`].
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

        // Models are organization-scoped in Coder v2.37+.
        let organization_id = self.get_default_organization_id().await?;

        // Fetch from API
        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}/chats/models", organization_id),
            )
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
    /// GET /api/v2/organizations/{organization}/chats/models
    #[cfg(not(feature = "chats-api"))]
    pub async fn list_chat_models(&self) -> Result<Vec<crate::types::ModelInfo>> {
        // Models are organization-scoped in Coder v2.37+.
        let organization_id = self.get_default_organization_id().await?;

        let resp = self
            .authenticated_request(
                reqwest::Method::GET,
                &format!("/api/v2/organizations/{}/chats/models", organization_id),
            )
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
        let repository: String = GithubConfig::init_from_env()?
            .repository
            .unwrap_or_else(|| "openflows/target".to_string());
        let repo_url = format!("https://github.com/{}.git", repository);
        let template_name = format!("openflows-{}", role);

        // Resolve the default organization ID BEFORE provisioning the workspace.
        // In the Coder v2 GA Chats API the organization_id is REQUIRED — the caller
        // must be a member of the org the chat belongs to. Failing fast here avoids
        // consuming workspace resources (create/start/await) when the org cannot be
        // resolved, since the chat could never be created without it.
        let organization_id = self.get_default_organization_id().await.context(
            "Failed to resolve default organization ID (required by the Coder v2 Chats API)",
        )?;

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

        // Let Coder use the workspace's default model.
        // model_config_id expects a UUID, not a model name, so we pass None.
        let model_config_id = None;

        // Create chat with OpenFlows labels (includes tenant)
        let tenant = TenantConfig::init_from_env()?
            .effective_tenant()
            .to_string();
        let labels = build_chat_labels(ticket_id, role, "openflows", &tenant);
        let chat_req = CreateChatRequest {
            organization_id: Some(organization_id),
            owner_id: None,
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
mod http_mock {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Mutex;

    /// A minimal in-process HTTP/1.1 server for exercising the Coder client
    /// against mocked `/api/v2` routes without a real Coder deployment.
    pub struct HttpMock {
        addr: SocketAddr,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    #[derive(Debug, Clone)]
    pub struct RecordedRequest {
        pub method: String,
        pub path: String,
        pub body: String,
    }

    impl HttpMock {
        pub async fn new() -> anyhow::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            let requests = Arc::new(Mutex::new(Vec::new()));
            let handle = tokio::spawn(Self::run(listener, Arc::clone(&requests)));
            Ok(Self {
                addr,
                requests,
                handle: Some(handle),
            })
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        pub async fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().await.clone()
        }

        pub async fn shutdown(self) {
            if let Some(handle) = self.handle {
                handle.abort();
                let _ = handle.await;
            }
        }

        async fn run(listener: TcpListener, requests: Arc<Mutex<Vec<RecordedRequest>>>) {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let requests = Arc::clone(&requests);
                tokio::spawn(async move {
                    let _ = Self::handle_conn(stream, requests).await;
                });
            }
        }

        async fn handle_conn(
            mut stream: TcpStream,
            requests: Arc<Mutex<Vec<RecordedRequest>>>,
        ) -> std::io::Result<()> {
            let mut buf: Vec<u8> = Vec::with_capacity(4096);
            loop {
                // Read until a full header terminator is in the buffer.
                let header_end = loop {
                    if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                        break pos + 4;
                    }
                    let mut chunk = [0u8; 512];
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                };

                let head = String::from_utf8_lossy(&buf[..header_end]);
                let mut lines = head.lines();
                let mut req_parts = lines.next().unwrap_or("").split_whitespace();
                let method = req_parts.next().unwrap_or("").to_string();
                let path = req_parts.next().unwrap_or("").to_string();
                let mut content_length = 0usize;
                for line in lines {
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }

                while buf.len() < header_end + content_length {
                    let mut chunk = [0u8; 512];
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "unexpected EOF while reading body",
                        ));
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }

                let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length])
                    .to_string();
                requests.lock().await.push(RecordedRequest {
                    method: method.clone(),
                    path: path.clone(),
                    body,
                });

                let (status, content) = if path == "/api/v2/workspaces/ws-delete-running"
                    && method == "GET"
                {
                    let requests = requests.lock().await;
                    let checks = requests
                        .iter()
                        .filter(|r| r.method == "GET" && r.path == path)
                        .count();
                    let (state, build_state) = if checks > 1 {
                        ("deleted", "succeeded")
                    } else {
                        ("deleting", "running")
                    };
                    ("200 OK", serde_json::json!({"id":"ws-delete-running", "status":state, "latest_build":{"status":build_state, "transition":"delete"}}).to_string())
                } else if path == "/api/v2/workspaces/ws-delete-me" && method == "GET" {
                    let requests = requests.lock().await;
                    let submitted = requests.iter().any(|r| {
                        r.method == "POST" && r.path == "/api/v2/workspaces/ws-delete-me/builds"
                    });
                    let (state, build_state) = if submitted {
                        ("deleted", "succeeded")
                    } else {
                        ("running", "succeeded")
                    };
                    ("200 OK", serde_json::json!({"id":"ws-delete-me", "status":state, "latest_build":{"status":build_state, "transition": if submitted { "delete" } else { "start" }}}).to_string())
                } else if path == "/api/v2/workspaces/ws-restart" {
                    let requests = requests.lock().await;
                    let stopped = requests.iter().any(|r| {
                        r.method == "POST" && r.path == "/api/v2/workspaces/ws-restart/builds"
                    });
                    let state = if stopped { "stopped" } else { "running" };
                    (
                        "200 OK",
                        serde_json::json!({"id":"ws-restart", "latest_build":{"status":state}})
                            .to_string(),
                    )
                } else if path == "/api/v2/workspaces/ws-restart/builds" {
                    ("201 Created", "{}".to_string())
                } else {
                    route(&method, &path)
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                    status,
                    content.len(),
                    content
                );
                stream.write_all(response.as_bytes()).await?;
                stream.flush().await?;

                buf.drain(..header_end + content_length);
            }
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn route(method: &str, path: &str) -> (&'static str, String) {
        match (method, path) {
            ("GET", "/api/v2/organizations") => (
                "200 OK",
                r#"[{"id":"org-abc","name":"default","is_default":true}]"#.to_string(),
            ),
            ("GET", "/api/v2/organizations/org-abc/chats/models") => (
                "200 OK",
                r#"{"providers":[{"provider":"openai-compat","available":true,"models":[{"id":"openai-compat:reviewer-pro","provider":"openai-compat","model":"reviewer-pro","display_name":"Reviewer Pro"}]}]}"#
                    .to_string(),
            ),
            ("POST", "/api/v2/chats") => (
                "201 Created",
                r#"{"id":"chat-1","organization_id":"org-abc"}"#.to_string(),
            ),
            ("POST", "/api/v2/chats/queued/messages") => (
                "200 OK",
                r#"{"queued":true,"message":{"id":41,"chat_id":"queued","role":"user","content":[{"type":"text","text":"previous message"}]},"queued_message":{"id":42,"chat_id":"queued","content":[{"type":"text","text":"approval"}],"created_at":"2026-09-30T00:00:00Z"}}"#.to_string(),
            ),
            ("POST", "/api/v2/chats/immediate/messages") => (
                "200 OK",
                r#"{"queued":false,"message":{"id":43,"chat_id":"immediate","role":"user","content":[{"type":"text","text":"approval"}],"created_at":"2026-09-30T00:00:00Z"}}"#.to_string(),
            ),
            ("POST", "/api/v2/chats/legacy/messages") => (
                "201 Created",
                r#"{"id":"legacy-id","chat_id":"legacy","role":"user","content":[]}"#.to_string(),
            ),
            ("POST", "/api/v2/chats/malformed/messages") => (
                "200 OK", r#"{"queued":true}"#.to_string(),
            ),
            ("POST", "/api/v2/chats/rejected/messages") => (
                "403 Forbidden", r#"{"message":"chat access denied"}"#.to_string(),
            ),
            ("POST", "/api/v2/files") => (
                "201 Created",
                r#"{"hash":"file-hash-1"}"#.to_string(),
            ),
            ("POST", "/api/v2/organizations/org-abc/templateversions") => (
                "201 Created",
                r#"{"id":"version-1","name":"release-1","template_id":null,"organization_id":"org-abc","job":{"id":"job-1","status":"pending"}}"#.to_string(),
            ),
            ("GET", "/api/v2/templateversions/version-1") => (
                "200 OK",
                r#"{"id":"version-1","name":"release-1","template_id":null,"organization_id":"org-abc","job":{"id":"job-1","status":"succeeded"}}"#.to_string(),
            ),
            ("GET", "/api/v2/templates/template-abc/versions") => (
                "200 OK",
                r#"[{"id":"version-1","name":"release-1","template_id":"template-abc","organization_id":"org-abc","job":{"id":"job-1","status":"succeeded","error":null,"error_code":null}}]"#.to_string(),
            ),
            ("GET", "/api/v2/templates/template-foreign/versions") => (
                "200 OK",
                r#"[{"id":"version-foreign","name":"release-1","template_id":"template-foreign","organization_id":"other-org","job":{"id":"job-2","status":"succeeded"}}]"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-delete-me/builds") => (
                "201 Created",
                r#"{"id":"build-1","transition":"delete"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-delete-me") => (
                "200 OK",
                r#"{"id":"ws-delete-me","name":"ws-delete-me","status":"deleted","latest_build":{"status":"succeeded","transition":"delete"}}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-delete-fail/builds") => (
                "201 Created",
                r#"{"id":"build-fail","transition":"delete"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-delete-fail") => (
                "200 OK",
                r#"{"id":"ws-delete-fail","name":"ws-delete-fail","status":"failed","latest_build":{"status":"failed","transition":"delete"}}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-deleted/builds") => (
                "201 Created",
                r#"{"id":"build-2","transition":"delete"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-deleted") => (
                "200 OK",
                r#"{"id":"ws-deleted","name":"ws-deleted","status":"deleted","latest_build":{"status":"succeeded","transition":"delete"}}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-gone/builds") => (
                "410 Gone",
                r#"{"message":"Workspace ws-gone was deleted"}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-already-deleted/builds") => (
                "400 Bad Request",
                r#"{"message":"workspace has already been deleted"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-already-deleted") => (
                "404 Not Found",
                r#"{"message":"workspace was deleted"}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-delete-denied/builds") => (
                "403 Forbidden",
                r#"{"message":"operator credential expired"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-delete-denied") => (
                "200 OK",
                r#"{"id":"ws-delete-denied","name":"ws-delete-denied","status":"running"}"#.to_string(),
            ),
            ("POST", "/api/v2/workspaces/ws-wait-gone/builds") => (
                "201 Created",
                r#"{"id":"build-3","transition":"delete"}"#.to_string(),
            ),
            ("GET", "/api/v2/workspaces/ws-wait-gone") => (
                "410 Gone",
                r#"{"message":"Workspace ws-wait-gone was deleted"}"#.to_string(),
            ),
            _ => ("404 Not Found", r#"{"message":"not found"}"#.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_chat_models_body;

    use std::sync::{Arc, RwLock};

    #[test]
    fn manager_requests_use_the_latest_shared_operator_token() {
        let token = Arc::new(RwLock::new("first-key".to_owned()));
        let client = crate::CoderClient::new("https://coder.example.test", "first-key")
            .with_shared_token(token.clone());
        let first = client
            .authenticated_request(reqwest::Method::GET, "/api/v2/users/me")
            .build()
            .unwrap();
        assert_eq!(first.headers()["authorization"], "Bearer first-key");
        *token.write().unwrap() = "second-key".to_owned();
        let second = client
            .authenticated_request(reqwest::Method::GET, "/api/v2/users/me")
            .build()
            .unwrap();
        assert_eq!(second.headers()["authorization"], "Bearer second-key");
    }

    #[test]
    fn coder_cli_path_override_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let coder = dir.path().join("coder");
        std::fs::write(&coder, "#!/bin/sh\n").unwrap();

        let previous = std::env::var_os("CODER_CLI_PATH");
        std::env::set_var("CODER_CLI_PATH", &coder);
        let resolved = super::resolve_coder_cli();
        match previous {
            Some(value) => std::env::set_var("CODER_CLI_PATH", value),
            None => std::env::remove_var("CODER_CLI_PATH"),
        }

        assert_eq!(resolved, coder);
    }

    #[tokio::test]
    async fn send_chat_message_accepts_queued_and_immediate_envelopes() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        for (chat_id, expected_id) in [
            ("queued", "42"),
            ("immediate", "43"),
            ("legacy", "legacy-id"),
        ] {
            let message = client
                .send_chat_message(chat_id, vec![crate::types::ChatInputPart::text("approval")])
                .await
                .expect("accepted messages must not be reported as failures and retried");
            assert_eq!(message.id, expected_id);
            assert_eq!(message.chat_id, chat_id);
            assert_eq!(message.role, "user");
            if chat_id != "legacy" {
                assert_eq!(message.content_raw[0]["text"], "approval");
            }
        }
        server.shutdown().await;
    }

    #[tokio::test]
    async fn send_chat_message_retains_http_and_malformed_response_errors() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        assert!(client.send_chat_message("malformed", vec![]).await.is_err());
        let error = client
            .send_chat_message("rejected", vec![])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"));
        assert!(error.contains("chat access denied"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn template_version_upload_uses_coder_v237_api_contract() {
        use std::io::Write;

        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        let org = crate::CoderOrganizationId::new("org-abc");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"fake tar archive").unwrap();
        let archive = encoder.finish().unwrap();

        let created = client
            .create_template_version_with_variables(
                &org,
                None,
                "release-1",
                &archive,
                &[crate::types::TemplateVariableValue {
                    name: "dev_binary_host_path".into(),
                    value: "/opt/openflows/.dev-binaries".into(),
                }],
            )
            .await
            .expect("version upload and create must match Coder routes");
        assert_eq!(created.id, "version-1");
        assert_eq!(created.job.as_ref().unwrap().id, "job-1");
        assert_eq!(created.template_id, "");

        let fetched = client
            .get_template_version(&org, &created.id)
            .await
            .expect("fetch version")
            .expect("version exists");
        assert!(fetched.job.as_ref().unwrap().is_succeeded());

        let requests = server.requests().await;
        let upload = requests
            .iter()
            .find(|request| request.method == "POST" && request.path == "/api/v2/files")
            .expect("archive must be uploaded to /api/v2/files");
        assert!(!upload.body.is_empty());

        let create = requests
            .iter()
            .find(|request| {
                request.method == "POST"
                    && request.path == "/api/v2/organizations/org-abc/templateversions"
            })
            .expect("version must be created through the organization endpoint");
        let body: serde_json::Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(body["storage_method"], "file");
        assert_eq!(body["file_id"], "file-hash-1");
        assert_eq!(body["provisioner"], "terraform");
        assert!(body.get("template_id").is_none());
        assert_eq!(
            body["user_variable_values"],
            serde_json::json!([{
                "name": "dev_binary_host_path",
                "value": "/opt/openflows/.dev-binaries"
            }])
        );
        assert!(requests.iter().any(|request| {
            request.method == "GET" && request.path == "/api/v2/templateversions/version-1"
        }));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_template_version_rejects_cross_organization_response() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        let error = client
            .get_template_version(&crate::CoderOrganizationId::new("other-org"), "version-1")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected 'other-org'"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn list_template_versions_uses_global_template_route_and_checks_scope() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        let org = crate::CoderOrganizationId::new("org-abc");
        let template = crate::CoderTemplateId::new("template-abc");
        let versions = client
            .list_template_versions(&org, &template)
            .await
            .expect("list using Coder's template-id route");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, "version-1");
        assert_eq!(versions[0].job.as_ref().unwrap().error, "");
        assert_eq!(versions[0].job.as_ref().unwrap().error_code, "");

        let error = client
            .list_template_versions(&org, &crate::CoderTemplateId::new("template-foreign"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected 'org-abc'"));

        let requests = server.requests().await;
        assert!(requests.iter().any(|request| {
            request.method == "GET" && request.path == "/api/v2/templates/template-abc/versions"
        }));
        server.shutdown().await;
    }

    #[test]
    fn coder_canceled_status_is_terminal() {
        let job = crate::CoderProvisionerJob {
            id: "job-canceled".into(),
            status: "canceled".into(),
            error: String::new(),
            error_code: String::new(),
        };
        assert!(job.is_terminal());
    }

    #[test]
    fn template_push_failure_preserves_terraform_diagnostics() {
        let output = std::process::Command::new("sh")
            .args(["-c", "echo 'Extra characters after interpolation expression'; echo 'template import init error' >&2; exit 1"])
            .output().unwrap();
        let error = super::check_template_push_output("openflows-nexus", &output)
            .unwrap_err()
            .to_string();
        assert!(error.contains("openflows-nexus"));
        assert!(error.contains("Extra characters after interpolation expression"));
        assert!(error.contains("template import init error"));
    }

    #[test]
    fn template_push_success_allows_warnings() {
        let output = std::process::Command::new("sh")
            .args(["-c", "echo 'No lockfile found' >&2"])
            .output()
            .unwrap();
        assert!(super::check_template_push_output("openflows-nexus", &output).is_ok());
    }

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

    #[tokio::test]
    async fn list_chat_models_resolves_default_org_and_hits_org_scoped_path() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");

        let models = client
            .list_chat_models()
            .await
            .expect("list_chat_models should succeed against mock");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "reviewer-pro");
        assert_eq!(models[0].provider, "openai-compat");

        let requests = server.requests().await;
        let orgs_hit = requests
            .iter()
            .any(|r| r.method == "GET" && r.path == "/api/v2/organizations");
        assert!(
            orgs_hit,
            "expected org resolution to hit /api/v2/organizations"
        );
        let models_hit = requests
            .iter()
            .any(|r| r.method == "GET" && r.path == "/api/v2/organizations/org-abc/chats/models");
        assert!(
            models_hit,
            "expected models fetch to hit /api/v2/organizations/'{{org}}'/chats/models"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn create_chat_sends_required_non_empty_organization_id() {
        use crate::types::CreateChatRequest;

        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");

        let req = CreateChatRequest {
            organization_id: Some("org-abc".to_string()),
            owner_id: Some("tenant-owner".to_string()),
            workspace_id: "ws-1".to_string(),
            model_config_id: None,
            content: vec![crate::types::ChatInputPart::text("hello")],
            labels: None,
        };
        let chat = client
            .create_chat(&req)
            .await
            .expect("create_chat should succeed against mock");
        assert_eq!(chat.id, "chat-1");

        let requests = server.requests().await;
        let create = requests
            .iter()
            .find(|r| r.method == "POST" && r.path == "/api/v2/chats")
            .expect("expected a POST /api/v2/chats request");

        let body: serde_json::Value =
            serde_json::from_str(&create.body).expect("recorded body should be valid JSON");
        let org_id = body
            .get("organization_id")
            .and_then(|v| v.as_str())
            .expect("organization_id must be present and a string");
        assert!(!org_id.is_empty(), "organization_id must not be empty");
        assert_eq!(org_id, "org-abc");
        assert_eq!(body["owner_id"], "tenant-owner");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn restart_workspace_stops_before_starting() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        client.restart_workspace("ws-restart").await.unwrap();
        let requests = server.requests().await;
        let transitions: Vec<String> = requests
            .iter()
            .filter(|r| r.method == "POST")
            .map(|r| {
                serde_json::from_str::<serde_json::Value>(&r.body).unwrap()["transition"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(transitions, vec!["stop", "start"]);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn delete_workspace_sends_delete_build_transition() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");

        client
            .delete_workspace("ws-delete-me")
            .await
            .expect("delete_workspace should succeed against mock");

        let requests = server.requests().await;
        let delete_req = requests
            .iter()
            .find(|r| r.method == "POST" && r.path == "/api/v2/workspaces/ws-delete-me/builds")
            .expect("expected a POST to workspace builds");

        let body: serde_json::Value =
            serde_json::from_str(&delete_req.body).expect("recorded body should be valid JSON");
        assert_eq!(
            body.get("transition").and_then(|v| v.as_str()),
            Some("delete")
        );

        // 404 should also be treated as success (already deleted)
        client
            .delete_workspace("ws-not-found")
            .await
            .expect("404 should succeed");

        // Workspace reporting status 'deleted' should succeed
        client
            .delete_workspace("ws-deleted")
            .await
            .expect("workspace with deleted status should succeed");
        assert!(!server
            .requests()
            .await
            .iter()
            .any(|r| { r.method == "POST" && r.path == "/api/v2/workspaces/ws-deleted/builds" }));
        client
            .delete_workspace("ws-delete-running")
            .await
            .expect("an existing delete build should be waited on");
        assert!(!server.requests().await.iter().any(|r| {
            r.method == "POST" && r.path == "/api/v2/workspaces/ws-delete-running/builds"
        }));

        // 410 Gone on delete build should be treated as success (already deleted)
        client
            .delete_workspace("ws-gone")
            .await
            .expect("410 on build should succeed");

        client
            .delete_workspace("ws-already-deleted")
            .await
            .expect("a rejected repeat delete must succeed after confirming 404");
        assert!(!server.requests().await.iter().any(|r| {
            r.method == "POST" && r.path == "/api/v2/workspaces/ws-already-deleted/builds"
        }));

        // 410 Gone during wait should be treated as success (confirmed deleted)
        client
            .delete_workspace("ws-wait-gone")
            .await
            .expect("410 on wait should succeed");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn delete_workspace_fails_when_delete_build_fails() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");

        let err = client
            .delete_workspace("ws-delete-fail")
            .await
            .expect_err("delete_workspace must fail when the delete build fails");

        assert!(
            err.to_string().contains("Delete build failed"),
            "Error message should explain delete build failed: {}",
            err
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn delete_workspace_reports_coder_denial_without_response_body() {
        let server = super::http_mock::HttpMock::new().await.unwrap();
        let client = crate::CoderClient::new(&server.url(), "test-token");
        let err = client
            .delete_workspace("ws-delete-denied")
            .await
            .unwrap_err();
        let coder = err.downcast_ref::<crate::CoderHttpError>().unwrap();
        assert_eq!(coder.status, reqwest::StatusCode::FORBIDDEN);
        assert!(!err.to_string().contains("operator credential expired"));
        server.shutdown().await;
    }
}
