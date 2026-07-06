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

        // 4. Push workspace templates (bundled in binary)
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
        push_template_silently(
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

        // 5. Create or refresh the long-lived Nexus workspace outside Coder.
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
            let repository = std::env::var("OPENFLOWS_REPOSITORY")
                .or_else(|_| std::env::var("AGENTFLOW_REPOSITORY"))
                .unwrap_or_else(|_| String::new());
            let repo_url = if repository.is_empty() {
                String::new()
            } else {
                format!("https://github.com/{}.git", repository)
            };
            let redis_url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://redis:6379".to_string());
            let litellm_proxy_url = std::env::var("LITELLM_PROXY_URL")
                .unwrap_or_else(|_| "http://proxy:4000".to_string());
            let use_ai_gateway = std::env::var("USE_AI_GATEWAY").unwrap_or_else(|_| "true".into());
            let registry_json = match std::env::var("OPENFLOWS_REGISTRY_JSON") {
                Ok(json) => json,
                Err(_) => {
                    let path = std::env::var("OPENFLOWS_REGISTRY_PATH")
                        .unwrap_or_else(|_| "orchestration/agent/registry.json".to_string());
                    std::fs::read_to_string(&path).unwrap_or_default()
                }
            };

            match client
                .create_workspace(&CreateWorkspaceRequest {
                    template_name: "openflows-nexus".to_string(),
                    name: nexus_workspace_name.clone(),
                    parameters: json!({
                        "repo_url": repo_url,
                        "redis_url": redis_url,
                        "litellm_proxy_url": litellm_proxy_url,
                        "use_ai_gateway": use_ai_gateway,
                        "coder_url": client.base_url(),
                        "coder_api_token": nexus_api_token,
                        "registry_json": registry_json,
                    }),
                })
                .await
            {
                Ok(workspace) => {
                    let _ = client
                        .wait_for_workspace_ready(&workspace.id, Duration::from_secs(180))
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
}

async fn push_template_silently(client: &CoderClient, name: &str, data: &[u8]) {
    match client.push_template(name, data).await {
        Ok(t) => info!("  ✓ Template '{}' pushed", t.name),
        Err(e) => {
            info!(
                "  ⚠ Template '{}' push failed (may already exist): {}",
                name, e
            );
        }
    }
}
