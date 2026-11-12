// crates/coder-client/src/bootstrap.rs
//! Coder bootstrapper — idempotent setup on startup.
//!
//! Creates admin user, obtains API token, pushes workspace templates, and can
//! materialize the long-lived Nexus workspace used by the orchestrator.
//! Safe to call on every restart.

use crate::{CoderClient, CreateWorkspaceRequest};
use anyhow::{Context, Result};
use config::CoderConfig;
use envconfig::Envconfig;
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn};

/// Bootstrapper for Coder integration.
pub struct CoderBootstrapper {
    client: CoderClient,
    admin_email: String,
    admin_password: String,
    admin_username: String,
    /// Configuration loaded once at startup, reused across the bootstrap flow
    env: Option<config::EnvConfig>,
}

/// Prefix of the per-tenant Nexus workspace name. `scripts/setup.sh` looks the
/// workspace up by the same `openflows-nexus-<tenant>` name.
const NEXUS_WORKSPACE_PREFIX: &str = "openflows-nexus-";

/// Coder rejects workspace names longer than this.
const CODER_WORKSPACE_NAME_MAX: usize = 32;

/// Longest tenant name whose Nexus workspace name still fits in Coder's limit.
pub const MAX_TENANT_NAME_LEN: usize = CODER_WORKSPACE_NAME_MAX - NEXUS_WORKSPACE_PREFIX.len();

/// Build the Nexus workspace name for a tenant, checking it against Coder's
/// workspace name rule (letters, digits and single hyphens, at most 32
/// characters) so a bad name fails before any Coder call instead of as a
/// 400 from workspace creation.
pub fn nexus_workspace_name(tenant_name: &str) -> Result<String> {
    anyhow::ensure!(
        tenant_name.len() <= MAX_TENANT_NAME_LEN,
        "tenant name '{tenant_name}' is {} characters; Coder limits workspace names to {CODER_WORKSPACE_NAME_MAX}, \
         so with the '{NEXUS_WORKSPACE_PREFIX}' prefix the tenant name can be at most {MAX_TENANT_NAME_LEN}. \
         Pass a shorter --name",
        tenant_name.len()
    );
    anyhow::ensure!(
        !tenant_name.is_empty()
            && tenant_name
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric())),
        "tenant name '{tenant_name}' is not a valid Coder workspace name: use only ASCII letters, digits and \
         single hyphens, not starting or ending with a hyphen. Pass a different --name"
    );
    Ok(format!("{NEXUS_WORKSPACE_PREFIX}{tenant_name}"))
}

/// Default admin password that meets Coder's security requirements.
const SECURE_DEFAULT_PASSWORD: &str = "Op3nFl0ws!";

/// Check whether a password meets Coder's minimum security requirements.
///
/// Coder requires at least: uppercase, lowercase, digit, special character,
/// and a minimum length of 8 characters.
fn password_meets_coder_requirements(password: &str) -> bool {
    if password.len() < 8 {
        return false;
    }
    let has_uppercase = password.chars().any(|c| c.is_uppercase());
    let has_lowercase = password.chars().any(|c| c.is_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_special = password.chars().any(|c| !c.is_alphanumeric());
    has_uppercase && has_lowercase && has_digit && has_special
}

impl CoderBootstrapper {
    /// Create a bootstrapper from environment variables.
    ///
    /// Reads:
    /// - `CODER_URL`: Coder server URL (default: http://localhost:7080)
    /// - `CODER_ADMIN_EMAIL`: Admin email (default: admin@openflows.dev)
    /// - `CODER_ADMIN_PASSWORD`: Admin password (default: Op3nFl0ws!)
    /// - `CODER_ADMIN_USERNAME`: Admin username (default: admin)
    ///
    /// If `CODER_ADMIN_PASSWORD` does not meet Coder's security requirements
    /// (uppercase, lowercase, digit, special character, min 8 chars), it is
    /// replaced with the secure default and a warning is logged.
    pub fn from_env() -> Result<Self> {
        let env = config::EnvConfig::from_env()?;
        let url = env.coder.url.clone();
        let email = env.coder.admin_email.clone();
        let raw_password = env.coder.admin_password.clone().unwrap_or_default();
        let username = env.coder.admin_username.clone();

        let password = if password_meets_coder_requirements(&raw_password) {
            raw_password
        } else {
            warn!(
                "CODER_ADMIN_PASSWORD does not meet Coder security requirements \
                 (needs uppercase, lowercase, digit, special char, min 8 chars). \
                 Falling back to default secure password."
            );
            SECURE_DEFAULT_PASSWORD.to_string()
        };

        let client = CoderClient::new_unauthenticated(&url);

        Ok(Self {
            client,
            admin_email: email,
            admin_password: password,
            admin_username: username,
            env: Some(env),
        })
    }

    /// Create a bootstrapper with explicit parameters.
    pub fn new(url: &str, email: &str, username: &str, password: &str) -> Self {
        let client = CoderClient::new_unauthenticated(url);
        Self {
            client,
            admin_email: email.to_string(),
            admin_password: password.to_string(),
            admin_username: username.to_string(),
            env: None,
        }
    }

    /// Bootstrap Coder: wait for healthy → create admin → get API token → push templates
    /// → optionally create the Nexus workspace.
    ///
    /// Idempotent: safe to call on every startup.
    pub async fn bootstrap(&self) -> Result<CoderClient> {
        info!("Bootstrapping Coder...");

        // 1. Wait for Coder server to be healthy
        self.client
            .wait_for_healthy(Duration::from_secs(120))
            .await?;
        info!("  ✓ Coder server healthy");

        // 1a. If a valid session token is already configured, reuse it and
        //     operate as that user instead of creating/logging in as admin.
        //     This lets the system run under any pre-existing Coder user
        //     (e.g. a GitHub-authenticated user) rather than hardcoding admin.
        let existing_token = self
            .env
            .as_ref()
            .and_then(|e| e.coder.session_token.clone())
            .or_else(|| {
                CoderConfig::init_from_env()
                    .ok()
                    .and_then(|c| c.session_token)
            });
        if let Some(existing_token) = existing_token.filter(|t| !t.is_empty()) {
            let probe_client = self
                .client
                .with_token(existing_token.clone())
                .with_session_token(&existing_token);
            if let Ok(me) = probe_client.get_me().await {
                info!(
                    username = %me.username,
                    user_id = %me.id,
                    "  ✓ Reusing existing CODER_SESSION_TOKEN for user '{}' — skipping admin bootstrap",
                    me.username
                );
                let api_key = probe_client.create_api_token(&me.id, "openflows").await?;
                let client = probe_client
                    .with_token(api_key.key.clone())
                    .with_session_token(&existing_token);
                info!("  ✓ API token generated for '{}'", me.username);
                return Self::push_templates(client).await;
            }
        }

        // 2. Create first user (idempotent)
        let user = self
            .client
            .create_first_user(
                &self.admin_email,
                &self.admin_username,
                &self.admin_password,
            )
            .await?;
        info!(
            "  ✓ Admin user resolved (id: {}, username: {})",
            user.id, user.username
        );

        // 3. Login and get session token, then create API token
        let session_token = self
            .client
            .login_with_password(&self.admin_email, &self.admin_password)
            .await?;

        // Persist session token so coder ssh can authenticate later.
        // 1. Set as environment variable for the current process and children
        // 2. Save to file for subsequent process restarts
        std::env::set_var("CODER_SESSION_TOKEN", &session_token);

        if let Ok(tenant) = config::TenantConfig::init_from_env() {
            let directory = tenant.openflows_home();
            let session_file = directory.join("coder-session-token");
            if std::fs::create_dir_all(&directory).is_ok() {
                let _ = std::fs::write(&session_file, &session_token);
                info!(session_file = %session_file.display(), "Session token persisted to file");
            }
        }

        let client_with_session = self
            .client
            .with_token(session_token.clone())
            .with_session_token(&session_token);

        // Resolve the real user ID (needed when create_first_user returned a stub)
        let user_id = if !user.id.is_empty() {
            user.id.clone()
        } else {
            let me = client_with_session.get_me().await?;
            info!("  ✓ Resolved admin user from /users/me (id: {})", me.id);
            me.id
        };

        let api_key = client_with_session
            .create_api_token(&user_id, "openflows")
            .await?;
        let client = client_with_session
            .with_token(api_key.key.clone())
            .with_session_token(&session_token);
        info!("  ✓ API token generated");

        Self::push_templates(client).await
    }

    /// Shared post-auth logic: push templates. Used by both the "reuse existing
    /// token" fast path and the full admin bootstrap path.
    async fn push_templates(client: CoderClient) -> Result<CoderClient> {
        let resolved_user = client.resolve_current_user().await?;
        info!(
            username = %resolved_user,
            "  ✓ Current user resolved from auth token"
        );

        // Resolve the .dev-binaries host path so workspace templates can
        // bind-mount the local openflows binary for local dev/testing.
        // Set as a TF_VAR_* env var — `coder templates push` runs Terraform
        // under the hood and inherits the parent environment.
        Self::set_dev_binary_host_path();

        // Track template push failures — bootstrap must fail if required
        // templates cannot be pushed, so the caller knows provisioning is
        // incomplete. `None` from push_template_silently means the push failed;
        // `Some(false)` means it was skipped (hash matched), which is not an
        // error.
        let mut template_errors = Vec::new();

        let forge_result = push_template_silently(
            &client,
            "openflows-forge",
            include_bytes!("../templates/openflows-forge.tar.gz"),
        )
        .await;
        if forge_result.is_none() {
            template_errors.push("openflows-forge");
        }

        let sentinel_result = push_template_silently(
            &client,
            "openflows-sentinel",
            include_bytes!("../templates/openflows-sentinel.tar.gz"),
        )
        .await;
        if sentinel_result.is_none() {
            template_errors.push("openflows-sentinel");
        }

        let nexus_result = push_template_silently(
            &client,
            "openflows-nexus",
            include_bytes!("../templates/openflows-nexus.tar.gz"),
        )
        .await;
        if nexus_result.is_none() {
            template_errors.push("openflows-nexus");
        }

        let vessel_result = push_template_silently(
            &client,
            "openflows-vessel",
            include_bytes!("../templates/openflows-vessel.tar.gz"),
        )
        .await;
        if vessel_result.is_none() {
            template_errors.push("openflows-vessel");
        }

        let lore_result = push_template_silently(
            &client,
            "openflows-lore",
            include_bytes!("../templates/openflows-lore.tar.gz"),
        )
        .await;
        if lore_result.is_none() {
            template_errors.push("openflows-lore");
        }

        // Fail fast if critical templates could not be pushed. This prevents
        // bootstrap from silently succeeding after CLI or Terraform errors.
        if !template_errors.is_empty() {
            anyhow::bail!(
                "Failed to push {} template(s): {}. \
                 See the template push diagnostics above for the underlying error.",
                template_errors.len(),
                template_errors.join(", ")
            );
        }

        info!("  ✓ Coder bootstrapped");
        Ok(client)
    }

    /// Set the TF_VAR_dev_binary_host_path for local dev/testing template pushes.
    fn set_dev_binary_host_path() {
        if std::env::var("TF_VAR_dev_binary_host_path").is_ok() {
            return;
        }
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let dev_bin = cwd.join(".dev-binaries");
        if !dev_bin.is_dir() {
            return;
        }
        let canonical = std::fs::canonicalize(&dev_bin)
            .unwrap_or(dev_bin)
            .to_string_lossy()
            .into_owned();
        info!(
            host_path = %canonical,
            "Setting TF_VAR_dev_binary_host_path for template push"
        );
        std::env::set_var("TF_VAR_dev_binary_host_path", &canonical);
    }

    pub async fn verify_llm_configured(client: &CoderClient) -> Result<()> {
        match client.list_chat_models().await {
            Ok(models) if !models.is_empty() => {
                info!("  ✓ {} LLM model(s) configured in Coder", models.len());
                Ok(())
            }
            Ok(_) => {
                anyhow::bail!(
                    "No LLM models configured in Coder. \
                     Go to the Coder dashboard → AI Settings → Coder Agents → Models \
                     and configure at least one provider/model before adding tenants."
                )
            }
            Err(e) => {
                warn!(error = %e, "Could not verify LLM configuration (Chats API may not be enabled yet)");
                info!("  ⚠ Could not verify LLM config — ensure Coder Agents/AI is enabled and at least one model is configured (dashboard → AI Settings → Coder Agents → Models)");
                Ok(())
            }
        }
    }

    /// Provision a tenant's nexus workspace using the current session user
    /// (admin) identity. Returns the workspace ID.
    pub async fn ensure_tenant(
        &self,
        client: &CoderClient,
        tenant_name: &str,
        github_repo: &str,
        registry_json: &str,
    ) -> Result<String> {
        info!("Setting up tenant: {} (repo: {})", tenant_name, github_repo);
        let nexus_workspace_name = nexus_workspace_name(tenant_name)?;

        // Use the current session user (admin) as the tenant owner.
        let admin = client.get_me().await?;
        info!(
            "  ✓ Using session user '{}' as tenant owner",
            admin.username
        );
        let coder_url = client.base_url();

        // 2. Create the nexus workspace under the session user (admin).
        let redis_url = "redis://redis:6379".to_string();
        let repo_url = config::GithubConfig::init_from_env()?.repository_clone_url(github_repo);

        // Re-running `tenant add` for an existing tenant returns the existing
        // workspace without changing its build parameters (see
        // create_workspace_for_user 409 handling). Existing workspaces keep
        // their original `start_controller` value and must be recreated once
        // to pick up `start_controller=true` (new tenants get it automatically).
        if let Ok(workspaces) = client.list_workspaces(&admin.id).await {
            // The listing is deployment-wide, so match both owner and name.
            if workspaces
                .iter()
                .any(|w| w.name == nexus_workspace_name && w.owner_name == admin.username)
            {
                println!(
                    "  ⚠ Tenant workspace '{}' already exists — its build parameters (including fleet) are unchanged. \
                     Recreate it once to apply `--fleet` / enable the in-workspace controller \
                     (`start_controller=true`); new tenants get these automatically.",
                    nexus_workspace_name
                );
            }
        }

        let hook_secret = self
            .env
            .as_ref()
            .and_then(|e| e.hooks.chat_hook_secret.clone())
            .or_else(|| {
                config::EnvConfig::from_env()
                    .ok()
                    .and_then(|c| c.hooks.chat_hook_secret)
            })
            .filter(|s| !s.trim().is_empty())
            .context("CODER_CHAT_HOOK_SECRET must be set to 32+ random bytes before creating the tenant Nexus workspace")?;
        let hook_url = self
            .env
            .as_ref()
            .and_then(|e| e.hooks.chat_hook_url_effective())
            .or_else(|| {
                config::EnvConfig::from_env()
                    .ok()
                    .and_then(|c| c.hooks.chat_hook_url_effective())
            })
            .unwrap_or_default();
        // The docs gate secrets go to the controller only (never to workers).
        let docs_gate = self
            .env
            .as_ref()
            .map(|e| e.docs_gate.clone())
            .or_else(|| config::EnvConfig::from_env().ok().map(|c| c.docs_gate));
        let (docs_gate_secret, docs_gate_previous) = docs_gate
            .map(|g| (g.secret, g.previous_secret))
            .unwrap_or_default();
        let workspace = client
            .create_workspace_for_user(
                &admin.id,
                &CreateWorkspaceRequest {
                    template_name: "openflows-nexus".to_string(),
                    name: nexus_workspace_name.clone(),
                    parameters: json!({
                        "repo_url": repo_url,
                        "redis_url": redis_url,
                        "coder_url": coder_url,
                        "coder_session_token": client.session_token(),
                        "tenant": tenant_name,
                        "github_repository": github_repo,
                        "coder_chat_hook_secret": hook_secret,
                        "coder_chat_hook_url": hook_url,
                        "docs_gate_secret": docs_gate_secret.unwrap_or_default(),
                        "docs_gate_secret_previous": docs_gate_previous.unwrap_or_default(),
                        "registry_json": registry_json,
                        "start_controller": true,
                    }),
                },
            )
            .await?;

        client
            .wait_for_workspace_ready(&workspace.id, Duration::from_secs(300))
            .await?;

        info!(
            workspace_id = %workspace.id,
            workspace_name = %workspace.name,
            tenant = tenant_name,
            "  ✓ Tenant nexus workspace created"
        );

        Ok(workspace.id)
    }
}

/// Fingerprint both the archive and the variables stored by server-side Terraform.
fn template_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let variables = serde_json::to_vec(&crate::template_variable_values())
        .expect("template variable strings are JSON serializable");
    hasher.update(variables);
    hex_encode(&hasher.finalize())
}

/// Minimal hex encoder (avoids pulling in another crate just for this).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Load the persisted template hash store from `~/.openflows/template-hashes.json`.
/// Returns an empty map if the file doesn't exist or can't be parsed.
fn load_template_hashes() -> std::collections::HashMap<String, String> {
    let Ok(tenant) = config::TenantConfig::init_from_env() else {
        return Default::default();
    };
    let path = tenant.openflows_home().join("template-hashes.json");
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

/// Persist the template hash store to `~/.openflows/template-hashes.json`.
fn save_template_hashes(hashes: &std::collections::HashMap<String, String>) {
    let Ok(tenant) = config::TenantConfig::init_from_env() else {
        return;
    };
    let dir = tenant.openflows_home();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("template-hashes.json");
    if let Ok(json) = serde_json::to_string_pretty(hashes) {
        let _ = std::fs::write(&path, json);
    }
}

/// Push a template only when its content hash has changed (or it doesn't exist
/// on the Coder server yet). After a successful push, the hash is persisted so
/// subsequent bootstrap calls skip unchanged templates.
///
/// Returns `Some(true)` if the template was (re)pushed, `Some(false)` if it was
/// skipped because the content hash matched the last-pushed version, or `None`
/// if the push attempt failed. Callers use `None` to determine whether bootstrap
/// should fail due to a template management error.
async fn push_template_silently(client: &CoderClient, name: &str, data: &[u8]) -> Option<bool> {
    let current_hash = template_hash(data);

    let before_templates = client.list_templates_in_default_org().await.ok();
    let before_template = before_templates
        .as_ref()
        .and_then(|t| t.iter().find(|t| t.name == name));
    let before_template_id = before_template.map(|t| t.id.clone());

    let mut hashes = load_template_hashes();
    let last_hash = hashes.get(name).map(String::as_str);

    if before_template_id.is_some() && last_hash == Some(current_hash.as_str()) {
        info!(
            "  ✓ Template '{}' unchanged — skipping push (hash matches)",
            name
        );
        return Some(false);
    }

    let reason = if before_template_id.is_none() {
        "new template"
    } else {
        "content changed"
    };
    info!("  → Pushing template '{}' ({})", name, reason);

    let mut push_result = Err(anyhow::anyhow!("template push not attempted"));
    for attempt in 1..=5u32 {
        match client.push_template(name, data).await {
            Ok(t) => {
                push_result = Ok(t);
                break;
            }
            Err(e) => {
                if attempt < 5 {
                    warn!(
                        attempt,
                        "  ⚠ Template '{}' push attempt failed ({}); retrying...", name, e
                    );
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt) * 3)).await;
                }
                push_result = Err(e);
            }
        }
    }

    match push_result {
        Ok(t) => {
            hashes.insert(name.to_string(), current_hash);
            save_template_hashes(&hashes);
            info!("  ✓ Template '{}' pushed (version updated)", t.name);
            Some(true)
        }
        Err(e) => {
            warn!("  ⚠ Template '{}' push failed: {}", name, e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{nexus_workspace_name, MAX_TENANT_NAME_LEN};

    #[test]
    fn nexus_workspace_name_fits_coder_limit() {
        assert_eq!(MAX_TENANT_NAME_LEN, 16);
        let longest = "a".repeat(MAX_TENANT_NAME_LEN);
        assert_eq!(nexus_workspace_name(&longest).unwrap().len(), 32);
        assert_eq!(
            nexus_workspace_name("rustlib").unwrap(),
            "openflows-nexus-rustlib"
        );
        assert_eq!(
            nexus_workspace_name("Jagoum-rust").unwrap(),
            "openflows-nexus-Jagoum-rust"
        );
    }

    #[test]
    fn nexus_workspace_name_rejects_names_coder_would_reject() {
        for bad in [
            "Jagoum-rust-library", // 35-character workspace name
            "",
            "my_team",
            "my.team",
            "-team",
            "team-",
            "my--team",
        ] {
            assert!(
                nexus_workspace_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
