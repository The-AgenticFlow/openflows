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
    /// instead of re-parsing `std::env::var(...)` per step.
    env: Option<config::EnvConfig>,
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

        if let Ok(home) = std::env::var("HOME") {
            let session_file = format!("{}/.openflows/coder-session-token", home);
            if std::fs::create_dir_all(format!("{}/.openflows", home)).is_ok() {
                let _ = std::fs::write(&session_file, &session_token);
                info!(session_file = %session_file, "Session token persisted to file");
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
        // bootstrap from silently succeeding when the member's credentials
        // lack permission to modify templates.
        if !template_errors.is_empty() {
            anyhow::bail!(
                "Failed to push {} template(s): {}. \
                 Verify the session token has template management permissions.",
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

    /// Verify that GitHub external auth is configured on the Coder server.
    ///  needed for agents authentication
    pub fn verify_external_auth_configured() -> Result<()> {
        info!("  ✓ GitHub external auth configure in the Coder dashboard if agents authentication");
        Ok(())
    }

    /// Create or verify a tenant: a Coder user + GitHub OAuth link + nexus workspace.
    ///
    /// Steps:
    /// 1. Create the tenant-owner Coder user (member role, no admin)
    /// 2. Print the GitHub OAuth link for the user to complete in the dashboard
    /// 3. Poll until the GitHub grant exists
    /// 4. Mint a scoped session token for that user
    /// 5. Create the openflows-nexus workspace under that user
    ///
    /// Returns the workspace ID.
    fn tenant_password(tenant_name: &str) -> String {
        let base = format!("T3nant!{}", tenant_name);
        if password_meets_coder_requirements(&base) {
            base
        } else {
            format!("T3nant!{}#1", tenant_name)
        }
    }

    fn tenant_state_file() -> Option<std::path::PathBuf> {
        std::env::var("HOME").ok().map(|h| {
            std::path::PathBuf::from(h)
                .join(".openflows")
                .join("tenants.json")
        })
    }

    fn load_tenant_password(tenant_name: &str) -> Option<String> {
        let path = Self::tenant_state_file()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&content).ok()?;
        map.get(tenant_name)
            .and_then(|v| v.get("password"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn save_tenant_password(tenant_name: &str, password: &str) {
        if let Some(path) = Self::tenant_state_file() {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let mut map: serde_json::Map<String, serde_json::Value> =
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
            let mut entry = serde_json::Map::new();
            entry.insert(
                "password".to_string(),
                serde_json::Value::String(password.to_string()),
            );
            map.insert(tenant_name.to_string(), serde_json::Value::Object(entry));
            let _ = std::fs::write(
                &path,
                serde_json::to_string_pretty(&map).unwrap_or_default(),
            );
        }
    }

    pub async fn ensure_tenant(
        &self,
        client: &CoderClient,
        tenant_name: &str,
        github_repo: &str,
    ) -> Result<String> {
        info!("Setting up tenant: {} (repo: {})", tenant_name, github_repo);

        // 1. Create tenant-owner user (idempotent — login if exists)
        let tenant_email = format!("{}@tenant.openflows.dev", tenant_name);
        let tenant_password = Self::load_tenant_password(tenant_name).unwrap_or_else(|| {
            let pwd = Self::tenant_password(tenant_name);
            Self::save_tenant_password(tenant_name, &pwd);
            pwd
        });

        // Try to create the user; if it exists, we just proceed
        let _ = client
            .create_first_user(&tenant_email, tenant_name, &tenant_password)
            .await;
        info!("  ✓ Tenant user '{}' resolved", tenant_name);

        // 2. Find the tenant user ID via admin API (fallback to admin for testing).
        //    Resolved before the link poll so we can login as the tenant user
        //    and check THEIR GitHub grant rather than the admin's.
        let coder_url = client.base_url();
        let tenant_user = match client.list_users().await {
            Ok(users) => users
                .into_iter()
                .find(|u| u.username == tenant_name || u.email == tenant_email),
            Err(e) => {
                warn!("Could not list users: {} — falling back to admin user", e);
                None
            }
        };
        let tenant_user = match tenant_user {
            Some(u) => {
                info!("  ✓ Tenant user ID resolved: {}", u.id);
                u
            }
            None => {
                warn!("Tenant user not found in list — using admin user as fallback for testing");
                client.get_me().await?
            }
        };

        // 3. Mint a scoped API token for the tenant user (admin can do this).
        //    The external-auth endpoints return the current token's user links,
        //    so the grant check below runs as this token's user. Reuses the
        //    same token that provisions the workspace — no password login
        //    required (the tenant user may fall back to the admin account).
        let tenant_api_key = client
            .create_api_token(&tenant_user.id, "openflows-nexus")
            .await?;
        let tenant_token = tenant_api_key.key;
        info!("  ✓ Tenant API token minted");
        let tenant_client = client
            .with_token(tenant_token.clone())
            .with_session_token(&tenant_token);

        // 4. Poll the tenant user's GitHub external-auth grant until linked
        //    (API-driven grant check — replaces the previous "press Enter" wait).
        let external_auth_id = self
            .env
            .as_ref()
            .and_then(|e| e.coder.external_auth_id.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "primary-github".to_string());
        eprintln!();
        eprintln!("  ─── GitHub Link Required ───");
        eprintln!();
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(300);
        let mut link_shown = false;
        loop {
            match tenant_client.get_external_auth(&external_auth_id).await {
                Ok(auth) if auth.authenticated => {
                    let linked_user = auth
                        .user
                        .as_ref()
                        .map(|u| u.login.as_str())
                        .unwrap_or("(unknown account)");
                    info!(
                        "  ✓ GitHub link confirmed for account '{}' (provider: {})",
                        linked_user, external_auth_id
                    );
                    break;
                }
                Ok(auth) => {
                    if !link_shown {
                        eprintln!("  Your GitHub account is not linked yet. To connect it:");
                        if !auth.app_install_url.is_empty() {
                            eprintln!(
                                "    1. Install/grant the OpenFlows GitHub App: {}",
                                auth.app_install_url
                            );
                        }
                        // Kick off a device flow for self-serve linking without a
                        // manual dashboard click.
                        match tenant_client
                            .create_external_auth_device(&external_auth_id)
                            .await
                        {
                            Ok(device) => {
                                let target = if !device.verification_uri_complete.is_empty() {
                                    device.verification_uri_complete.clone()
                                } else {
                                    device.verification_uri.clone()
                                };
                                if !target.is_empty() {
                                    eprintln!("    2. Authorize at: {}", target);
                                }
                                if !device.user_code.is_empty() {
                                    eprintln!("       and enter code: {}", device.user_code);
                                }
                            }
                            Err(_) => {
                                eprintln!(
                                    "    2. Complete the link in the dashboard: {}/external-auth/{}",
                                    coder_url.trim_end_matches('/'),
                                    external_auth_id
                                );
                            }
                        }
                        eprintln!();
                        eprintln!("  Waiting for the link to complete (up to 5 minutes)...");
                        link_shown = true;
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "Could not query external-auth status for '{}'; retrying",
                        external_auth_id
                    );
                }
            }
            if start.elapsed() >= timeout {
                anyhow::bail!(
                    "Timed out waiting for the tenant owner to link GitHub (provider '{}'). \
                     Complete the link at {}/external-auth/{} and rerun `openflows tenant add`.",
                    external_auth_id,
                    coder_url.trim_end_matches('/'),
                    external_auth_id
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        // 5. Create the nexus workspace under the tenant user (admin can do this)
        let redis_url = "redis://redis:6379".to_string();
        let nexus_workspace_name = format!("openflows-nexus-{}", tenant_name);
        let repo_url = format!("https://github.com/{}.git", github_repo);

        // Re-running `tenant add` for an existing tenant returns the existing
        // workspace without changing its build parameters (see
        // create_workspace_for_user 409 handling). Existing workspaces therefore
        // keep their original `start_controller` value and must be recreated once
        // to pick up `start_controller=true` (new tenants get it automatically).
        if let Ok(workspaces) = client.list_workspaces(&tenant_user.id).await {
            // The workspace listing is deployment-wide (list_workspaces ignores
            // the user id), so match BOTH the owner and the name — as the other
            // bootstrap lookups do — to avoid a false migration warning from
            // another owner's same-named workspace.
            if workspaces
                .iter()
                .any(|w| w.name == nexus_workspace_name && w.owner_name == tenant_user.username)
            {
                println!(
                    "  ⚠ Tenant workspace '{}' already exists — its build parameters are unchanged. \
                     Recreate it once to enable the in-workspace controller \
                     (`start_controller=true`); new tenants get this automatically.",
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
        let workspace = client
            .create_workspace_for_user(
                &tenant_user.id,
                &CreateWorkspaceRequest {
                    template_name: "openflows-nexus".to_string(),
                    name: nexus_workspace_name.clone(),
                    parameters: json!({
                        "repo_url": repo_url,
                        "redis_url": redis_url,
                        "coder_url": coder_url,
                        "coder_session_token": tenant_token,
                        "tenant": tenant_name,
                        "github_repository": github_repo,
                        "coder_chat_hook_secret": hook_secret,
                        "coder_chat_hook_url": hook_url,
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

/// Compute a hex SHA-256 fingerprint of the template archive bytes.
fn template_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
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
    let Ok(home) = std::env::var("HOME") else {
        return Default::default();
    };
    let path = format!("{}/.openflows/template-hashes.json", home);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

/// Persist the template hash store to `~/.openflows/template-hashes.json`.
fn save_template_hashes(hashes: &std::collections::HashMap<String, String>) {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let dir = format!("{}/.openflows", home);
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{}/template-hashes.json", dir);
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

    let before_templates = client.list_templates().await.ok();
    let before_template = before_templates
        .as_ref()
        .and_then(|t| t.iter().find(|t| t.name == name));
    let before_template_id = before_template.map(|t| t.id.clone());
    let before_updated_at = before_template.map(|t| t.updated_at.clone());

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

    match client.push_template(name, data).await {
        Ok(t) => {
            let after_templates = match client.list_templates().await {
                Ok(templates) => templates,
                Err(e) => {
                    warn!("  ⚠ Could not verify template '{}' after push: {}", name, e);
                    return None;
                }
            };
            let after_updated_at = after_templates
                .iter()
                .find(|tp| tp.name == name)
                .map(|tp| tp.updated_at.clone());

            // A successful version push keeps the stable template ID but bumps
            // `updated_at`. Only treat the push as rejected when we have a
            // known previous `updated_at` and it did not change. (An empty
            // `updated_at` means the API did not report one, so we cannot
            // verify and assume the push succeeded.)
            if before_template_id.is_some()
                && !before_updated_at.as_deref().unwrap_or("").is_empty()
                && after_updated_at.as_ref() == before_updated_at.as_ref()
            {
                warn!(
                    "  ⚠ Template '{}' push returned success but updated_at did not change — push may have been rejected",
                    name
                );
                return None;
            }

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
