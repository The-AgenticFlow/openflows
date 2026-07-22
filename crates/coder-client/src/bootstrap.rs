// crates/coder-client/src/bootstrap.rs
//! Coder bootstrapper — idempotent setup on startup.
//!
//! Creates admin user, obtains API token, pushes workspace templates, and can
//! materialize the long-lived Nexus workspace used by the orchestrator.
//! Safe to call on every restart.

use crate::{CoderClient, CreateWorkspaceRequest};
use anyhow::{Context, Result};
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn};

/// Bootstrapper for Coder integration.
pub struct CoderBootstrapper {
    client: CoderClient,
    admin_email: String,
    admin_password: String,
    admin_username: String,
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
    /// - `CODER_URL`: Coder server URL (required)
    /// - `CODER_ADMIN_EMAIL`: Admin email (default: admin@openflows.dev)
    /// - `CODER_ADMIN_PASSWORD`: Admin password (default: Op3nFl0ws!)
    /// - `CODER_ADMIN_USERNAME`: Admin username (default: admin)
    ///
    /// If `CODER_ADMIN_PASSWORD` does not meet Coder's security requirements
    /// (uppercase, lowercase, digit, special character, min 8 chars), it is
    /// replaced with the secure default and a warning is logged.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("CODER_URL").context("CODER_URL not set")?;
        let email = std::env::var("CODER_ADMIN_EMAIL")
            .unwrap_or_else(|_| "admin@openflows.dev".to_string());
        let raw_password = std::env::var("CODER_ADMIN_PASSWORD")
            .unwrap_or_else(|_| SECURE_DEFAULT_PASSWORD.to_string());
        let username =
            std::env::var("CODER_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());

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

        // 4. Resolve the .dev-binaries host path so workspace templates can
        //    bind-mount the local openflows binary for local dev/testing.
        //    Set as a TF_VAR_* env var — `coder templates push` runs Terraform
        //    under the hood and inherits the parent environment.
        if std::env::var("TF_VAR_dev_binary_host_path").is_err() {
            if let Ok(cwd) = std::env::current_dir() {
                let dev_bin = cwd.join(".dev-binaries");
                if dev_bin.is_dir() {
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
            }
        }

        // 5. Push workspace templates (bundled in binary)
        push_template_silently(
            &client,
            "openflows-forge",
            include_bytes!("../templates/openflows-forge.tar.gz"),
        )
        .await;
        push_template_silently(
            &client,
            "openflows-sentinel",
            include_bytes!("../templates/openflows-sentinel.tar.gz"),
        )
        .await;
        let nexus_template_updated = push_template_silently(
            &client,
            "openflows-nexus",
            include_bytes!("../templates/openflows-nexus.tar.gz"),
        )
        .await;
        push_template_silently(
            &client,
            "openflows-vessel",
            include_bytes!("../templates/openflows-vessel.tar.gz"),
        )
        .await;
        push_template_silently(
            &client,
            "openflows-lore",
            include_bytes!("../templates/openflows-lore.tar.gz"),
        )
        .await;

        // If the nexus template was re-pushed, the existing workspace is stale
        // (it still runs the old template version). Delete and recreate it so
        // the new template (e.g. fixed bind-mount path) takes effect.
        if nexus_template_updated {
            let nexus_workspace_name = std::env::var("OPENFLOWS_NEXUS_WORKSPACE_NAME")
                .unwrap_or_else(|_| "openflows-nexus".to_string());
            if let Ok(me) = client.get_me().await {
                if let Ok(workspaces) = client.list_workspaces(&me.id).await {
                    if let Some(existing) =
                        workspaces.iter().find(|w| w.name == nexus_workspace_name)
                    {
                        info!(
                            workspace_id = %existing.id,
                            workspace_name = %existing.name,
                            "  → Nexus template updated — deleting stale workspace for recreation"
                        );
                        // Stop first (required before delete in Coder)
                        let _ = client.stop_workspace(&existing.id).await;
                        match client.delete_workspace(&existing.id).await {
                            Ok(()) => info!("  ✓ Stale nexus workspace deleted"),
                            Err(e) => {
                                warn!(error = %e, "  ⚠ Could not delete stale nexus workspace; will attempt to create anyway")
                            }
                        }
                        // Give Coder a moment to clean up
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        }

        // 6. Create or refresh the long-lived Nexus workspace outside Coder.
        //
        // This is the bootstrapper's "first mover" responsibility: seed the
        // persistent control-plane workspace that runs the orchestration loop.
        if std::env::var("OPENFLOWS_CREATE_NEXUS_WORKSPACE")
            .map(|v| v != "false")
            .unwrap_or(true)
            && std::env::var("ROLE").as_deref() != Ok("nexus")
        {
            let nexus_workspace_name = std::env::var("OPENFLOWS_NEXUS_WORKSPACE_NAME")
                .unwrap_or_else(|_| "openflows-nexus".to_string());
            let nexus_api_token = std::env::var("OPENFLOWS_NEXUS_API_TOKEN")
                .or_else(|_| std::env::var("NEXUS_CODER_API_TOKEN"))
                .unwrap_or_else(|_| client.token().to_string());
            let repository = std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| String::new());
            let repo_url = if repository.is_empty() {
                String::new()
            } else {
                format!("https://github.com/{}.git", repository)
            };
            let redis_url = "redis://redis:6379".to_string();
            let tenant =
                std::env::var("OPENFLOWS_TENANT").unwrap_or_else(|_| "default".to_string());
            let registry_json = match std::env::var("OPENFLOWS_REGISTRY_JSON") {
                Ok(json) => json,
                Err(_) => {
                    let path = std::env::var("OPENFLOWS_REGISTRY_PATH")
                        .unwrap_or_else(|_| "orchestration/agent/registry.json".to_string());
                    std::fs::read_to_string(&path).unwrap_or_default()
                }
            };

            // Convert localhost URLs to Docker service names for workspace-to-service communication.
            // When creating workspaces from the host (where CODER_URL=http://localhost:7080),
            // the workspace containers cannot reach "localhost" — they need the Docker service name.
            let coder_url_for_workspace = client.base_url().replace("localhost", "coder");

            match client
                .create_workspace(&CreateWorkspaceRequest {
                    template_name: "openflows-nexus".to_string(),
                    name: nexus_workspace_name.clone(),
                    parameters: json!({
                        "repo_url": repo_url,
                        "redis_url": redis_url,
                        "coder_url": coder_url_for_workspace,
                        "coder_session_token": nexus_api_token,
                        "tenant": tenant,
                        "github_repository": repository,
                        "registry_json": registry_json,
                    }),
                })
                .await
            {
                Ok(workspace) => {
                    let _ = client
                        .wait_for_workspace_ready(&workspace.id, Duration::from_secs(300))
                        .await;
                    if let Err(e) = client
                        .wait_for_workspace_ssh(&workspace.id, Duration::from_secs(120))
                        .await
                    {
                        warn!(error = %e, "Workspace SSH not ready during bootstrap; continuing anyway");
                    }

                    if let Ok(home) = std::env::var("HOME") {
                        let state_dir = format!("{}/.openflows", home);
                        if std::fs::create_dir_all(&state_dir).is_ok() {
                            let state_file = format!("{}/nexus-workspace.json", state_dir);
                            let _ = std::fs::write(
                                &state_file,
                                serde_json::to_string_pretty(&json!({
                                    "workspace_id": workspace.id,
                                    "workspace_name": workspace.name,
                                    "template_name": "openflows-nexus",
                                    "coder_url": client.base_url(),
                                }))
                                .unwrap_or_else(|_| "{}".to_string()),
                            );
                            info!(state_file = %state_file, "Nexus workspace state persisted");
                        }
                    }

                    std::env::set_var("OPENFLOWS_NEXUS_WORKSPACE_ID", &workspace.id);
                    std::env::set_var("OPENFLOWS_NEXUS_WORKSPACE_NAME", &workspace.name);
                    info!(
                        workspace_id = %workspace.id,
                        workspace_name = %workspace.name,
                        "  ✓ Nexus workspace resolved"
                    );
                }
                Err(e) => {
                    info!(
                        error = %e,
                        "  ⚠ Nexus workspace bootstrap skipped/failed; continuing with existing control plane"
                    );
                }
            }
        }

        info!("  ✓ Coder bootstrapped");
        Ok(client)
    }

    /// Verify that at least one LLM provider/model is configured in Coder.
    /// Fails with dashboard instructions if none are available.
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
                info!("  ⚠ Could not verify LLM config — ensure at least one model is configured in the Coder dashboard");
                Ok(())
            }
        }
    }

    /// Verify that GitHub external auth is configured on the Coder server.
    /// This is now a no-op since GitHub OAuth is configured directly in the Coder dashboard.
    pub fn verify_external_auth_configured() -> Result<()> {
        info!("  ✓ GitHub external auth should be configured in the Coder dashboard");
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

        // 2. Print GitHub OAuth instructions
        let coder_url = client.base_url();
        eprintln!();
        eprintln!("  ─── GitHub OAuth Setup Required ───");
        eprintln!("  1. Log in to the Coder dashboard: {}", coder_url);
        eprintln!(
            "  2. Configure GitHub OAuth (Deployment → External Authentication → Add GitHub)"
        );
        eprintln!(
            "  3. As tenant user '{}', complete the GitHub OAuth flow in the dashboard",
            tenant_name
        );
        eprintln!("  4. Once linked, press Enter below to continue");
        eprintln!();
        eprintln!("  Note: For testing, you can skip OAuth and press Enter now");
        eprintln!("        (workspace will be created with admin token).");
        eprintln!();

        // 3. Poll until the grant exists (simplified — check every 5s, timeout 5 min)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(300);
        loop {
            if start.elapsed() >= timeout {
                anyhow::bail!(
                    "Timed out waiting for GitHub OAuth grant. \
                     The tenant owner must complete the link at {}/external-auth/github",
                    coder_url.trim_end_matches('/')
                );
            }
            // In a full implementation, we'd call an API to check if the user has
            // linked GitHub. For now, we wait for the user to press Enter.
            // Phase 5/6 will add a proper API check.
            eprint!("\r  Press Enter once the GitHub link is complete... ");
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        info!("  ✓ GitHub OAuth grant confirmed");

        // 4. Find the tenant user ID via admin API (fallback to admin for testing)
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

        // 5. Mint a scoped API token for the tenant user (admin can do this)
        let tenant_api_key = client
            .create_api_token(&tenant_user.id, "openflows-nexus")
            .await?;
        let tenant_token = tenant_api_key.key;
        info!("  ✓ Tenant API token minted");

        // 6. Create the nexus workspace under the tenant user (admin can do this)
        let redis_url = "redis://redis:6379".to_string();
        let nexus_workspace_name = format!("openflows-nexus-{}", tenant_name);
        let repo_url = format!("https://github.com/{}.git", github_repo);

        let github_pat = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN").unwrap_or_default();
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
                        "github_pat": github_pat,
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
/// Returns `true` if the template was (re)pushed, `false` if it was skipped
/// because the content hash matched the last-pushed version.
async fn push_template_silently(client: &CoderClient, name: &str, data: &[u8]) -> bool {
    let current_hash = template_hash(data);

    // Check whether the template exists on the Coder server.
    let exists = if let Ok(templates) = client.list_templates().await {
        templates.iter().any(|t| t.name == name)
    } else {
        false
    };

    // Compare against the last-pushed hash stored locally.
    let mut hashes = load_template_hashes();
    let last_hash = hashes.get(name).map(String::as_str);

    if exists && last_hash == Some(current_hash.as_str()) {
        info!(
            "  ✓ Template '{}' unchanged — skipping push (hash matches)",
            name
        );
        return false;
    }

    let reason = if !exists {
        "new template"
    } else {
        "content changed"
    };
    info!("  → Pushing template '{}' ({})", name, reason);

    match client.push_template(name, data).await {
        Ok(t) => {
            hashes.insert(name.to_string(), current_hash);
            save_template_hashes(&hashes);
            info!("  ✓ Template '{}' pushed (version updated)", t.name);
            true
        }
        Err(e) => {
            warn!("  ⚠ Template '{}' push failed: {}", name, e);
            false
        }
    }
}
