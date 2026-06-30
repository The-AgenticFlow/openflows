// crates/coder-client/src/bootstrap.rs
//! Coder bootstrapper — idempotent setup on startup.
//!
//! Creates admin user, obtains API token, and pushes workspace templates.
//! Safe to call on every restart.

use crate::CoderClient;
use anyhow::{Context, Result};
use std::time::Duration;
use tracing::info;

/// Bootstrapper for Coder integration.
pub struct CoderBootstrapper {
    client: CoderClient,
    admin_email: String,
    admin_password: String,
    admin_username: String,
}

impl CoderBootstrapper {
    /// Create a bootstrapper from environment variables.
    ///
    /// Reads:
    /// - `CODER_URL`: Coder server URL (required)
    /// - `CODER_ADMIN_EMAIL`: Admin email (default: admin@openflows.dev)
    /// - `CODER_ADMIN_PASSWORD`: Admin password (default: Op3nFl0ws!)
    /// - `CODER_ADMIN_USERNAME`: Admin username (default: admin)
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("CODER_URL").context("CODER_URL not set")?;
        let email = std::env::var("CODER_ADMIN_EMAIL")
            .unwrap_or_else(|_| "admin@openflows.dev".to_string());
        let password =
            std::env::var("CODER_ADMIN_PASSWORD").unwrap_or_else(|_| "Op3nFl0ws!".to_string());
        let username =
            std::env::var("CODER_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());

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

    /// Bootstrap Coder: wait for healthy → create admin → get API token → push templates.
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

        let client_with_session = self.client
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
        push_template_silently(&client, "openflows-forge", include_bytes!("../templates/openflows-forge.tar.gz")).await;
        push_template_silently(&client, "openflows-sentinel", include_bytes!("../templates/openflows-sentinel.tar.gz")).await;
        push_template_silently(&client, "openflows-nexus", include_bytes!("../templates/openflows-nexus.tar.gz")).await;
        push_template_silently(&client, "openflows-vessel", include_bytes!("../templates/openflows-vessel.tar.gz")).await;
        push_template_silently(&client, "openflows-lore", include_bytes!("../templates/openflows-lore.tar.gz")).await;

        info!("  ✓ Coder bootstrapped");
        Ok(client)
    }
}

async fn push_template_silently(client: &CoderClient, name: &str, data: &[u8]) {
    match client.push_template(name, data).await {
        Ok(t) => info!("  ✓ Template '{}' pushed", t.name),
        Err(e) => {
            info!("  ⚠ Template '{}' push failed (may already exist): {}", name, e);
        }
    }
}
