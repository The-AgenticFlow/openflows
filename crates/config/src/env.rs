//! Centralized environment configuration.
//!
//! All process-startup configuration is defined here as [`envconfig`]-derived
//! structs so that env-var reads are type-safe, validated, and initialized once
//! at startup instead of being scattered as inline `std::env::var(...)` calls
//! across the workspace.
//!
//! Secrets (tokens, passwords) are redacted from [`std::fmt::Debug`] output so
//! configuration never leaks credentials into logs or error diagnostics.

use envconfig::Envconfig;
use std::fmt;
use std::path::PathBuf;

/// Coder-related configuration.
///
/// `Debug` is implemented manually to redact credentials.
#[derive(Clone, Envconfig)]
pub struct CoderConfig {
    #[envconfig(from = "CODER_URL", default = "http://localhost:7080")]
    pub url: String,

    #[envconfig(from = "CODER_SESSION_TOKEN")]
    pub session_token: Option<String>,

    #[envconfig(from = "CODER_ADMIN_EMAIL", default = "admin@openflows.dev")]
    pub admin_email: String,

    /// Admin password for the initial Coder user. No default is baked in:
    /// the bootstrapper applies its own secure fallback only when the value is
    /// absent or fails Coder's password policy.
    #[envconfig(from = "CODER_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    #[envconfig(from = "CODER_ADMIN_USERNAME", default = "admin")]
    pub admin_username: String,

    #[envconfig(from = "CODER_IMAGE_TAG", default = "v2.37.1")]
    pub image_tag: String,

    #[envconfig(from = "CODER_GITHUB_TOKEN")]
    pub github_token: Option<String>,

    #[envconfig(from = "CODER_EXTERNAL_AUTH_0_ID")]
    pub external_auth_id: Option<String>,

    #[envconfig(from = "CODER_EXTERNAL_AUTH_0_CLIENT_ID")]
    pub external_auth_client_id: Option<String>,

    #[envconfig(from = "CODER_EXTERNAL_AUTH_0_CLIENT_SECRET")]
    pub external_auth_client_secret: Option<String>,
}

impl CoderConfig {
    /// Resolve the effective Coder auth token (the session token).
    pub fn effective_token(&self) -> Option<String> {
        self.session_token.clone()
    }
}

/// Coder Agent Lifecycle Hooks configuration (experimental).
///
/// These mirror the `coder server` flags that power the `agent-lifecycle-hooks`
/// experiment (see coder/coder `docs/admin/setup/chat-lifecycle-hooks.md`).
/// The OpenFlows Controller is the **consumer** of the deployment-wide webhook:
/// Coder `chatd` POSTs a JWT-signed lifecycle event to the hook URL for each
/// `session_start` / `user_prompt_submit` / `pre_tool_use` / `post_tool_use` /
/// `pre_compact` / `post_compact` / `stop` event, and this side verifies the
/// signature and observes (or denies/rewrites) the event.
///
/// The hook endpoint is a purely internal detail: the consumer derives it from
/// its bind address + the internal host label Coder must use to reach it. In
/// the bundled Docker topology the Nexus workspace has the stable network alias
/// `openflows-nexus`. The bundled stack sets the Coder-side URL/experiment
/// values; operators only override the shared secret for production or the bind
/// address/host for unusual topology.
///
/// On Coder's side, enabling the experiment looks like:
///   CODER_EXPERIMENTS=agent-lifecycle-hooks
///   CODER_CHAT_HOOK_URL=<derived internal URL>
///   CODER_CHAT_HOOK_SECRET=<at least 32 random bytes, HS256>
#[derive(Debug, Clone, Envconfig)]
pub struct CoderHooksConfig {
    /// Shared HS256 secret used to sign/verify hook JWTs (>= 32 bytes).
    #[envconfig(from = "CODER_CHAT_HOOK_SECRET")]
    pub chat_hook_secret: Option<String>,

    /// Per-request dispatch timeout Coder applies. We mirror it for awareness.
    #[envconfig(from = "CODER_CHAT_HOOK_TIMEOUT", default = "1500")]
    pub chat_hook_timeout_ms: u64,

    /// Break-glass switch (mirrored from Coder for observability).
    #[envconfig(from = "CODER_CHAT_HOOK_ENABLED", default = "true")]
    pub chat_hook_enabled: bool,

    /// Allow plain http (dev only).
    #[envconfig(from = "CODER_CHAT_HOOK_ALLOW_INSECURE", default = "false")]
    pub chat_hook_allow_insecure: bool,

    /// Bind address for the OpenFlows hook consumer endpoint. The default is
    /// Docker-reachable because the bundled Coder service posts from a container.
    #[envconfig(from = "OPENFLOWS_HOOK_ADDR", default = "0.0.0.0:3001")]
    pub hook_addr: String,

    /// Internal host label Coder must use to reach the consumer. Defaults to the
    /// Nexus workspace network alias in the bundled Docker topology.
    #[envconfig(from = "OPENFLOWS_HOOK_HOST", default = "openflows-nexus")]
    pub hook_host: String,

    /// The exact hook URL Coder is configured to POST to (Coder's
    /// `CODER_CHAT_HOOK_URL`). When set, this is the authoritative JWT audience
    /// the consumer validates against — it matches what Coder signs by
    /// construction. When unset, the audience is derived from
    /// [`CoderHooksConfig::hook_host`] + the [`CoderHooksConfig::hook_addr`]
    /// port. Keeping a single source of truth avoids `aud` drift between Coder's
    /// hook URL and the consumer's expectation.
    #[envconfig(from = "CODER_CHAT_HOOK_URL")]
    pub chat_hook_url: Option<String>,

    /// Alias knob used by docker-compose as the single source of truth for the
    /// hook endpoint. The controller process never sees Coder's
    /// `CODER_CHAT_HOOK_URL`, so honoring `OPENFLOWS_HOOK_URL` here lets a custom
    /// hook URL that an operator sets in `.env` propagate to the consumer via
    /// bootstrap — preventing `aud` drift (see [`CoderHooksConfig::chat_hook_url_effective`]).
    #[envconfig(from = "OPENFLOWS_HOOK_URL")]
    pub hook_url: Option<String>,

    /// Emit routine hook lifecycle logs. Warnings/errors remain visible.
    #[envconfig(from = "OPENFLOWS_HOOK_LOGS", default = "false")]
    pub hook_logs: bool,
}

impl CoderHooksConfig {
    /// Whether the OpenFlows consumer should be started at all. Coder's
    /// experiment flag is configured on the Coder service; this process only
    /// needs a shared secret to verify signed dispatches.
    pub fn enabled(&self) -> bool {
        self.chat_hook_enabled
            && self
                .chat_hook_secret
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
    }

    /// The port component of the bind address (e.g. `3001` from `0.0.0.0:3001`).
    pub fn port(&self) -> Option<String> {
        self.hook_addr.rsplit(':').next().map(|p| p.to_string())
    }

    /// The authoritative audience hint for the hook consumer. Prefers the exact
    /// Coder URL (`CODER_CHAT_HOOK_URL`), then the compose knob
    /// (`OPENFLOWS_HOOK_URL`), then `None` (callers derive from bind host/port).
    ///
    /// Preferring `OPENFLOWS_HOOK_URL` as a fallback keeps the bundled stack's
    /// single source of truth shared between Coder and the consumer even though
    /// the controller process never sees `CODER_CHAT_HOOK_URL`.
    pub fn chat_hook_url_effective(&self) -> Option<String> {
        let pick = |u: &Option<String>| {
            u.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        pick(&self.chat_hook_url).or_else(|| pick(&self.hook_url))
    }

    /// The internal hook URL Coder must POST to. When an authoritative hook URL
    /// is configured it is returned verbatim and used as the expected JWT `aud` —
    /// matching what Coder signs. Otherwise the URL is derived from the internal
    /// host label and the bind port, path `/experimental/hooks/chat` (the route
    /// the consumer registers). This is both what the consumer listens as its
    /// expected `aud` and what compose forwards to Coder — an internal detail
    /// the operator does not type.
    pub fn hook_public_url(&self) -> Option<String> {
        if let Some(url) = self.chat_hook_url_effective() {
            return Some(url);
        }
        let port = self.port()?;
        Some(format!(
            "http://{}:{}/experimental/hooks/chat",
            self.hook_host, port
        ))
    }
}

impl fmt::Debug for CoderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoderConfig")
            .field("url", &self.url)
            .field("session_token", &redact(&self.session_token))
            .field("admin_email", &self.admin_email)
            .field("admin_password", &"<redacted>")
            .field("admin_username", &self.admin_username)
            .field("image_tag", &self.image_tag)
            .field("github_token", &redact(&self.github_token))
            .field("external_auth_id", &self.external_auth_id)
            .field("external_auth_client_id", &self.external_auth_client_id)
            .field(
                "external_auth_client_secret",
                &redact(&self.external_auth_client_secret),
            )
            .finish()
    }
}

/// Infrastructure configuration (Redis, A2A relay).
///
/// `REDIS_URL` has no compile-time default: callers that accept a fallback use
/// [`InfraConfig::effective_redis_url`], while strict entry points (e.g. the
/// harness) require the variable to be explicitly set.
#[derive(Debug, Clone, Envconfig)]
pub struct InfraConfig {
    #[envconfig(from = "REDIS_URL")]
    pub redis_url: Option<String>,

    #[envconfig(from = "A2A_RELAY_ADDR", default = "127.0.0.1:3000")]
    pub a2a_relay_addr: String,
}

impl InfraConfig {
    /// Redis URL, falling back to the local stack default.
    pub fn effective_redis_url(&self) -> String {
        self.redis_url
            .clone()
            .unwrap_or_else(|| "redis://localhost:6379".to_string())
    }
}

/// OpenFlows tenant / namespace configuration.
///
/// `OPENFLOWS_TENANT` has no compile-time default so that the controller and
/// harness can detect when it was not explicitly configured. Callers that
/// accept the namespace fallback use [`TenantConfig::effective_tenant`].
#[derive(Clone, Envconfig)]
pub struct TenantConfig {
    #[envconfig(from = "OPENFLOWS_TENANT")]
    pub tenant: Option<String>,

    #[envconfig(from = "OPENFLOWS_TICKET")]
    pub ticket: Option<String>,

    #[envconfig(from = "OPENFLOWS_ROLE")]
    pub role: Option<String>,

    #[envconfig(from = "OPENFLOWS_HOME")]
    pub home: Option<String>,

    #[envconfig(from = "OPENFLOWS_REGISTRY_PATH")]
    pub registry_path: Option<String>,

    #[envconfig(from = "OPENFLOWS_REGISTRY_JSON")]
    pub registry_json: Option<String>,

    #[envconfig(from = "OPENFLOWS_NEXUS_WORKSPACE_ID")]
    pub nexus_workspace_id: Option<String>,

    #[envconfig(from = "OPENFLOWS_NEXUS_WORKSPACE_NAME")]
    pub nexus_workspace_name: Option<String>,

    #[envconfig(from = "OPENFLOWS_NEXUS_API_TOKEN")]
    pub nexus_api_token: Option<String>,

    #[envconfig(from = "OPENFLOWS_TAR", default = "tar")]
    pub tar: String,
}

impl TenantConfig {
    /// Tenant namespace, defaulting to `"default"` when not explicitly set.
    pub fn effective_tenant(&self) -> &str {
        self.tenant.as_deref().unwrap_or("default")
    }

    /// Resolve the OpenFlows home directory, defaulting to `~/.openflows`.
    pub fn openflows_home(&self) -> PathBuf {
        if let Some(home) = &self.home {
            return PathBuf::from(home);
        }
        let base = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join(".openflows")
    }
}

impl fmt::Debug for TenantConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantConfig")
            .field("tenant", &self.tenant)
            .field("ticket", &self.ticket)
            .field("role", &self.role)
            .field("home", &self.home)
            .field("registry_path", &self.registry_path)
            .field("registry_json", &self.registry_json)
            .field("nexus_workspace_id", &self.nexus_workspace_id)
            .field("nexus_workspace_name", &self.nexus_workspace_name)
            .field("nexus_api_token", &redact(&self.nexus_api_token))
            .field("tar", &self.tar)
            .finish()
    }
}

/// GitHub-related configuration.
///
/// `Debug` is implemented manually to redact tokens.
#[derive(Clone, Envconfig)]
pub struct GithubConfig {
    #[envconfig(from = "GITHUB_REPOSITORY")]
    pub repository: Option<String>,

    #[envconfig(from = "GITHUB_TOKEN")]
    pub token: Option<String>,

    #[envconfig(from = "GITHUB_PERSONAL_ACCESS_TOKEN")]
    pub personal_access_token: Option<String>,

    #[envconfig(from = "GITHUB_API_BASE", default = "https://api.github.com")]
    pub api_base: String,
}

impl GithubConfig {
    /// Effective GitHub token, preferring the personal access token and falling
    /// back to the generic `GITHUB_TOKEN`.
    pub fn effective_token(&self) -> Option<String> {
        self.personal_access_token
            .clone()
            .or_else(|| self.token.clone())
    }
}

impl fmt::Debug for GithubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubConfig")
            .field("repository", &self.repository)
            .field("token", &redact(&self.token))
            .field(
                "personal_access_token",
                &redact(&self.personal_access_token),
            )
            .finish()
    }
}

/// Agent/workspace configuration.
#[derive(Debug, Clone, Envconfig)]
pub struct AgentConfig {
    /// Preferred workspace root, sourced from `AGENTFLOW_WORKSPACE_ROOT`.
    #[envconfig(from = "AGENTFLOW_WORKSPACE_ROOT")]
    pub workspace_root: Option<String>,

    /// Fallback workspace root sourced from the legacy `WORKSPACE_ROOT`
    /// variable. [`AgentConfig::effective_workspace_root`] prefers
    /// [`Self::workspace_root`] and only falls back to this one.
    #[envconfig(from = "WORKSPACE_ROOT")]
    pub legacy_workspace_root: Option<String>,

    /// Whether the AI gateway is enabled.
    #[envconfig(from = "USE_AI_GATEWAY")]
    pub use_ai_gateway: Option<String>,

    /// Whether the bootstrapper should create the Nexus control-plane
    /// workspace.
    #[envconfig(from = "OPENFLOWS_CREATE_NEXUS_WORKSPACE")]
    pub create_nexus_workspace: Option<String>,

    /// The role this process runs as (e.g. `nexus`).
    #[envconfig(from = "ROLE")]
    pub role: Option<String>,
}

impl AgentConfig {
    /// Resolve the effective workspace root across the two accepted names.
    pub fn effective_workspace_root(&self) -> Option<String> {
        self.workspace_root
            .clone()
            .or_else(|| self.legacy_workspace_root.clone())
    }

    /// Whether the AI gateway is enabled.
    pub fn use_ai_gateway_enabled(&self) -> bool {
        matches!(self.use_ai_gateway.as_deref(), Some("true" | "1"))
    }

    /// Whether the bootstrapper should create the Nexus control-plane
    /// workspace.
    pub fn create_nexus_workspace_enabled(&self) -> bool {
        self.create_nexus_workspace.as_deref() != Some("false")
    }
}

/// Aggregate environment configuration loaded once at startup.
#[derive(Debug, Clone)]
pub struct EnvConfig {
    pub coder: CoderConfig,
    pub hooks: CoderHooksConfig,
    pub infra: InfraConfig,
    pub tenant: TenantConfig,
    pub github: GithubConfig,
    pub agent: AgentConfig,
}

/// Redact an optional secret for [`fmt::Debug`] output.
fn redact(v: &Option<String>) -> Option<&'static str> {
    v.as_deref().map(|_| "<redacted>")
}

impl EnvConfig {
    /// Validate the fields required to run the OpenFlows controller (in a
    /// nexus workspace). Returns a clear error naming the first missing value.
    ///
    /// # Errors
    /// Returns an error when a controller-required variable is not set.
    pub fn validate_controller(&self) -> anyhow::Result<()> {
        if self.coder.effective_token().is_none() {
            anyhow::bail!(
                "CODER_SESSION_TOKEN is not set. The Controller must run inside an \
                 openflows-nexus workspace."
            );
        }
        if self.tenant.tenant.is_none() {
            anyhow::bail!(
                "OPENFLOWS_TENANT is not set. The Controller must run inside an \
                 openflows-nexus workspace."
            );
        }
        Ok(())
    }

    /// Initialize all config structs from the environment, returning a clear
    /// error naming any missing/invalid variable. The caller decides whether to
    /// load a `.env` file first (e.g. via `dotenvy::dotenv()`); this function
    /// reads the already-populated process environment only, which keeps it
    /// deterministic and testable.
    ///
    /// # Errors
    /// Returns an error if any required environment variable is missing or a
    /// supplied value fails to parse.
    pub fn from_env() -> anyhow::Result<Self> {
        let coder =
            CoderConfig::init_from_env().map_err(|e| anyhow::anyhow!("Coder config: {e}"))?;
        let infra =
            InfraConfig::init_from_env().map_err(|e| anyhow::anyhow!("Infra config: {e}"))?;
        let tenant =
            TenantConfig::init_from_env().map_err(|e| anyhow::anyhow!("Tenant config: {e}"))?;
        let github =
            GithubConfig::init_from_env().map_err(|e| anyhow::anyhow!("GitHub config: {e}"))?;
        let agent =
            AgentConfig::init_from_env().map_err(|e| anyhow::anyhow!("Agent config: {e}"))?;
        let hooks =
            CoderHooksConfig::init_from_env().map_err(|e| anyhow::anyhow!("Hooks config: {e}"))?;

        Ok(Self {
            coder,
            hooks,
            infra,
            tenant,
            github,
            agent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot the given variables and restore them on drop so test-runner
    /// environment changes never leak to other tests.
    struct EnvGuard {
        snapshots: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            let snapshots = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            EnvGuard { snapshots }
        }

        fn unset_all(&self) {
            for (k, _) in &self.snapshots {
                std::env::remove_var(k);
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.snapshots {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn defaults_applied_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::capture(&[
            "CODER_URL",
            "CODER_ADMIN_EMAIL",
            "CODER_ADMIN_PASSWORD",
            "CODER_IMAGE_TAG",
            "REDIS_URL",
            "A2A_RELAY_ADDR",
            "CODER_CHAT_HOOK_SECRET",
            "CODER_CHAT_HOOK_ENABLED",
            "OPENFLOWS_HOOK_ADDR",
            "OPENFLOWS_HOOK_HOST",
            "OPENFLOWS_HOOK_LOGS",
            "CODER_CHAT_HOOK_URL",
            "OPENFLOWS_HOOK_URL",
            "OPENFLOWS_TENANT",
            "OPENFLOWS_TAR",
            "USE_AI_GATEWAY",
        ]);
        guard.unset_all();
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.coder.url, "http://localhost:7080");
        assert_eq!(cfg.coder.admin_email, "admin@openflows.dev");
        assert_eq!(cfg.coder.admin_username, "admin");
        assert_eq!(cfg.coder.admin_password, None);
        assert_eq!(cfg.coder.image_tag, "v2.37.1");
        assert_eq!(cfg.infra.effective_redis_url(), "redis://localhost:6379");
        assert_eq!(cfg.infra.a2a_relay_addr, "127.0.0.1:3000");
        assert_eq!(cfg.hooks.hook_addr, "0.0.0.0:3001");
        assert_eq!(cfg.hooks.hook_host, "openflows-nexus");
        assert!(cfg.hooks.chat_hook_url.is_none());
        assert!(!cfg.hooks.hook_logs);
        assert!(!cfg.hooks.enabled());
        assert_eq!(cfg.tenant.effective_tenant(), "default");
        assert_eq!(cfg.tenant.tar, "tar");
        assert_eq!(cfg.agent.use_ai_gateway, None);
        assert!(!cfg.agent.use_ai_gateway_enabled());
    }

    #[test]
    fn overrides_from_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[
            "CODER_URL",
            "REDIS_URL",
            "OPENFLOWS_TENANT",
            "USE_AI_GATEWAY",
        ]);
        for (k, v) in [
            ("CODER_URL", "http://coder.example.com:8080"),
            ("REDIS_URL", "redis://redis.example.com:6379"),
            ("OPENFLOWS_TENANT", "acme"),
            ("USE_AI_GATEWAY", "true"),
        ] {
            std::env::set_var(k, v);
        }
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.coder.url, "http://coder.example.com:8080");
        assert_eq!(
            cfg.infra.effective_redis_url(),
            "redis://redis.example.com:6379"
        );
        assert_eq!(cfg.tenant.effective_tenant(), "acme");
        assert!(cfg.agent.use_ai_gateway_enabled());
    }

    #[test]
    fn ai_gateway_accepts_valid_values_without_aborting() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&["USE_AI_GATEWAY"]);
        for (v, expected) in [
            ("true", true),
            ("1", true),
            ("false", false),
            ("garbage", false),
        ] {
            std::env::set_var("USE_AI_GATEWAY", v);
            let cfg = EnvConfig::from_env().unwrap();
            assert_eq!(
                cfg.agent.use_ai_gateway_enabled(),
                expected,
                "USE_AI_GATEWAY={v}"
            );
        }
    }

    #[test]
    fn hook_consumer_enablement_uses_secret_not_coder_experiment_flag() {
        let _g = ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::capture(&[
            "CODER_EXPERIMENTS",
            "CODER_CHAT_HOOK_SECRET",
            "CODER_CHAT_HOOK_ENABLED",
        ]);
        guard.unset_all();

        std::env::set_var("CODER_EXPERIMENTS", "agent-lifecycle-hooks");
        assert!(!EnvConfig::from_env().unwrap().hooks.enabled());

        std::env::set_var("CODER_CHAT_HOOK_SECRET", "0123456789abcdef0123456789abcdef");
        assert!(EnvConfig::from_env().unwrap().hooks.enabled());

        std::env::set_var("CODER_CHAT_HOOK_ENABLED", "false");
        assert!(!EnvConfig::from_env().unwrap().hooks.enabled());
    }

    #[test]
    fn hook_logs_are_opt_in() {
        let _g = ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::capture(&["OPENFLOWS_HOOK_LOGS"]);
        guard.unset_all();

        assert!(!EnvConfig::from_env().unwrap().hooks.hook_logs);

        std::env::set_var("OPENFLOWS_HOOK_LOGS", "true");
        assert!(EnvConfig::from_env().unwrap().hooks.hook_logs);
    }

    #[test]
    fn hook_public_url_prefers_coder_chat_hook_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::capture(&[
            "CODER_CHAT_HOOK_URL",
            "OPENFLOWS_HOOK_URL",
            "OPENFLOWS_HOOK_ADDR",
            "OPENFLOWS_HOOK_HOST",
        ]);
        guard.unset_all();

        // Without CODER_CHAT_HOOK_URL the audience is derived from host + addr.
        std::env::set_var("OPENFLOWS_HOOK_ADDR", "0.0.0.0:3001");
        std::env::set_var("OPENFLOWS_HOOK_HOST", "openflows-nexus");
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(
            cfg.hooks.hook_public_url().as_deref(),
            Some("http://openflows-nexus:3001/experimental/hooks/chat")
        );

        // When Coder's CODER_CHAT_HOOK_URL is set it is the authoritative
        // audience, regardless of the derived bind/host, preventing aud drift.
        std::env::set_var(
            "CODER_CHAT_HOOK_URL",
            "http://openflows-nexus:3900/experimental/hooks/chat",
        );
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(
            cfg.hooks.hook_public_url().as_deref(),
            Some("http://openflows-nexus:3900/experimental/hooks/chat")
        );

        // A custom OPENFLOWS_HOOK_URL (compose's single knob) is honored when
        // CODER_CHAT_HOOK_URL is not exported to the controller process — the
        // drift case where the controller would otherwise fall back to the
        // derived default while Coder signs with the custom URL.
        std::env::remove_var("CODER_CHAT_HOOK_URL");
        std::env::set_var(
            "OPENFLOWS_HOOK_URL",
            "http://openflows-nexus:4900/experimental/hooks/chat",
        );
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(
            cfg.hooks.hook_public_url().as_deref(),
            Some("http://openflows-nexus:4900/experimental/hooks/chat")
        );
    }

    #[test]
    fn create_nexus_workspace_is_lenient_and_defaults_enabled() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&["OPENFLOWS_CREATE_NEXUS_WORKSPACE"]);
        std::env::remove_var("OPENFLOWS_CREATE_NEXUS_WORKSPACE");
        assert!(EnvConfig::from_env()
            .unwrap()
            .agent
            .create_nexus_workspace_enabled());

        for (v, expected) in [
            ("false", false),
            ("true", true),
            ("1", true),
            ("garbage", true),
        ] {
            std::env::set_var("OPENFLOWS_CREATE_NEXUS_WORKSPACE", v);
            let cfg = EnvConfig::from_env().unwrap();
            assert_eq!(
                cfg.agent.create_nexus_workspace_enabled(),
                expected,
                "OPENFLOWS_CREATE_NEXUS_WORKSPACE={v}"
            );
        }
    }

    #[test]
    fn debug_redacts_secrets() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&["CODER_SESSION_TOKEN", "CODER_ADMIN_PASSWORD"]);
        std::env::set_var("CODER_SESSION_TOKEN", "s3cr3t-token");
        std::env::set_var("CODER_ADMIN_PASSWORD", "hunter2");
        let cfg = EnvConfig::from_env().unwrap();
        let dbg = format!("{:?}", cfg.coder);
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("s3cr3t-token"));
        assert!(!dbg.contains("hunter2"));
    }

    #[test]
    fn openflows_home_defaults_to_tilde() {
        let _g = ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::capture(&["OPENFLOWS_HOME", "HOME", "USERPROFILE"]);
        guard.unset_all();
        let cfg = EnvConfig::from_env().unwrap();
        assert!(cfg
            .tenant
            .openflows_home()
            .to_string_lossy()
            .ends_with(".openflows"));
    }
}
