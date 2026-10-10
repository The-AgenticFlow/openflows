//! Centralized, typed configuration for the Manager foundations.
//!
//! All Manager settings are defined here and loaded once at startup, following
//! the same envconfig pattern as the `config` crate. This module owns the
//! deployment mode (`local` vs `hosted`) and enforces the WP-01 requirement
//! that hosted configuration failures must not silently fall back to local
//! behavior: when hosted mode is selected, the required hosted settings must
//! be present or startup fails.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The deployment mode of the Manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Explicit local mode: existing operator workflows are preserved. The
    /// product database is optional and no hosted endpoints are required.
    Local,
    /// Hosted mode: the Manager owns the Openflows control-plane database and
    /// requires a secret provider. Missing hosted settings are a startup
    /// failure, never a silent fallback to local.
    Hosted,
}

impl Mode {
    /// Parse a mode from the `OPENFLOWS_MODE` value. An empty/absent value
    /// defaults to local. Unknown values are rejected rather than silently
    /// treated as local, so a misconfigured deployment fails loudly.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "local" => Ok(Mode::Local),
            "hosted" => Ok(Mode::Hosted),
            other => Err(format!(
                "invalid OPENFLOWS_MODE '{other}': expected 'local' or 'hosted'"
            )),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Local => write!(f, "local"),
            Mode::Hosted => write!(f, "hosted"),
        }
    }
}

/// The name of the environment variable that selects the deployment mode.
pub const MODE_ENV: &str = "OPENFLOWS_MODE";
/// The environment variable carrying the Openflows control-plane database URL.
pub const DATABASE_URL_ENV: &str = "OPENFLOWS_DATABASE_URL";
/// The environment variable selecting the secret-provider adapter.
pub const SECRET_PROVIDER_ENV: &str = "OPENFLOWS_SECRET_PROVIDER";
/// The default secret-provider adapter name for local mode and tests.
pub const DEFAULT_SECRET_PROVIDER: &str = "in-memory";

/// The public origin used for OAuth callbacks and device verification URLs.
pub const PUBLIC_URL_ENV: &str = "OPENFLOWS_PUBLIC_URL";
/// The central GitHub App client id.
pub const GITHUB_CLIENT_ID_ENV: &str = "OPENFLOWS_GITHUB_CLIENT_ID";
/// Secret-provider reference for the GitHub App client secret.
pub const GITHUB_CLIENT_SECRET_REF_ENV: &str = "OPENFLOWS_GITHUB_CLIENT_SECRET_REF";
/// GitHub API base (defaults to https://api.github.com).
pub const GITHUB_API_BASE_ENV: &str = "OPENFLOWS_GITHUB_API_BASE";
/// Secret-provider reference for the envelope master key used to encrypt
/// recoverable OAuth material.
pub const CRYPTO_MASTER_KEY_REF_ENV: &str = "OPENFLOWS_CRYPTO_MASTER_KEY_REF";
/// Whether browser session cookies are marked Secure.
pub const AUTH_COOKIE_SECURE_ENV: &str = "OPENFLOWS_AUTH_COOKIE_SECURE";
/// Explicit loopback-development exception (allows insecure cookies / HTTP).
pub const AUTH_ALLOW_LOOPBACK_ENV: &str = "OPENFLOWS_AUTH_ALLOW_LOOPBACK";

/// The length (in bytes) of the envelope master key.
pub const MASTER_KEY_BYTES: usize = 32;

use crate::auth::crypto::EnvelopeCipher;
use crate::error::ManagerError;

/// A 32-byte envelope master key, resolved from the secret provider or supplied
/// as an explicit development/test key.
#[derive(Clone)]
pub struct MasterKey(pub [u8; MASTER_KEY_BYTES]);
impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

/// The default development/test master key. Only used when no
/// `OPENFLOWS_CRYPTO_MASTER_KEY_REF` is configured and the deployment is not
/// hosted. Hosted mode requires a provider ref and fails closed otherwise.
pub fn dev_master_key() -> MasterKey {
    // Exactly 32 bytes; explicit development/test material only.
    MasterKey(*b"abcdefghijklmnopqrstuvwxyz012345")
}

/// Typed, centralized GitHub user-authorization configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// The central GitHub App client id (OAuth).
    pub client_id: String,
    /// The public origin used for callbacks and verification URLs.
    pub public_url: String,
    /// GitHub API base (defaults to https://api.github.com).
    pub github_api_base: String,
    /// Whether browser session cookies are `Secure`. Loopback development may
    /// set this false only via the explicit exception.
    pub cookie_secure: bool,
    /// Explicit loopback-development exception. When false, cookies are always
    /// Secure.
    pub allow_loopback: bool,
    /// The 32-byte envelope master key for recoverable OAuth material. `None`
    /// until resolved from the secret provider at startup (hosted mode) or from
    /// the dev key (local/tests).
    pub master_key: Option<MasterKey>,
}

impl AuthConfig {
    pub fn validate_origin(&self) -> Result<(), String> {
        let url = url::Url::parse(&self.public_url).map_err(|_| "invalid OPENFLOWS_PUBLIC_URL")?;
        let loopback = url
            .host_str()
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]");
        if url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.host_str().is_none()
        {
            return Err("OPENFLOWS_PUBLIC_URL must be an origin without credentials, path, query, or fragment".into());
        }
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback && self.allow_loopback) {
            return Err(
                "public origin requires HTTPS; HTTP requires explicit loopback development".into(),
            );
        }
        if !self.cookie_secure && !(loopback && self.allow_loopback) {
            return Err(
                "insecure cookies are permitted only for explicit loopback development".into(),
            );
        }
        Ok(())
    }

    /// Build a purpose-scoped [`EnvelopeCipher`] from the master key. Returns a
    /// configuration error when the key has not been resolved (missing hosted
    /// authentication configuration must fail closed, never silently degrade).
    pub fn envelope_cipher(&self, purpose: &str) -> Result<EnvelopeCipher, ManagerError> {
        let key = self
            .master_key
            .as_ref()
            .ok_or_else(|| ManagerError::Config("envelope master key not resolved".into()))?;
        Ok(EnvelopeCipher::derive(&key.0, purpose, 1))
    }

    /// Build a purpose-scoped cipher for a specific recorded key version.
    pub fn envelope_cipher_for_version(
        &self,
        purpose: &str,
        version: i32,
    ) -> Result<EnvelopeCipher, ManagerError> {
        if version != 1 {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "unsupported key version for OAuth material",
            ));
        }
        let key = self
            .master_key
            .as_ref()
            .ok_or_else(|| ManagerError::Config("envelope master key not resolved".into()))?;
        Ok(EnvelopeCipher::derive(&key.0, purpose, version as u32))
    }
}

fn loopback_exception_enabled() -> bool {
    std::env::var(AUTH_ALLOW_LOOPBACK_ENV)
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

/// Build auth settings from the environment. In hosted mode the required
/// settings must be present; in local mode they default for development.
/// `master_key_ref` selects a provider-backed key; when absent the dev key is
/// used (local/tests only — hosted mode validates a ref separately).
fn auth_from_env(require_auth: bool, master_key_ref: Option<String>) -> Result<AuthConfig, String> {
    let cookie_secure = match std::env::var(AUTH_COOKIE_SECURE_ENV).ok().as_deref() {
        None => !loopback_exception_enabled(),
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        Some(_) => return Err(format!("{AUTH_COOKIE_SECURE_ENV} must be true or false")),
    };
    let client_id = std::env::var(GITHUB_CLIENT_ID_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let public_url = std::env::var(PUBLIC_URL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());

    // If a provider ref is configured, the key is resolved asynchronously at
    // startup; otherwise use the explicit dev key (local/tests only).
    let master_key = if master_key_ref.is_some() {
        None
    } else {
        Some(dev_master_key())
    };

    if require_auth {
        let client_id =
            client_id.ok_or_else(|| format!("hosted mode requires {GITHUB_CLIENT_ID_ENV}"))?;
        let public_url =
            public_url.ok_or_else(|| format!("hosted mode requires {PUBLIC_URL_ENV}"))?;
        return Ok(AuthConfig {
            client_id,
            public_url: public_url.trim_end_matches('/').to_string(),
            github_api_base: std::env::var(GITHUB_API_BASE_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "https://api.github.com".to_string()),
            cookie_secure,
            allow_loopback: loopback_exception_enabled(),
            master_key,
        });
    }

    Ok(AuthConfig {
        client_id: client_id.unwrap_or_default(),
        public_url: public_url
            .unwrap_or_else(|| "http://127.0.0.1:3002".to_string())
            .trim_end_matches('/')
            .to_string(),
        github_api_base: std::env::var(GITHUB_API_BASE_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "https://api.github.com".to_string()),
        cookie_secure,
        allow_loopback: loopback_exception_enabled(),
        master_key,
    })
}

/// The foundational Manager configuration.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub mode: Mode,
    /// The Openflows control-plane PostgreSQL URL. Required in hosted mode;
    /// optional in local mode.
    pub database_url: Option<String>,
    /// The selected secret-provider adapter. Required in hosted mode; defaults
    /// to the in-memory provider in local mode.
    pub secret_provider: String,
    /// Bind address for the HTTP server.
    pub http_addr: String,
    /// Centralized GitHub user-authorization settings.
    pub auth: AuthConfig,
    /// Secret-provider reference for the GitHub App client secret (OAuth
    /// exchange). Required in hosted mode.
    pub github_client_secret_ref: Option<String>,
    /// Secret-provider reference for the envelope master key. Required in
    /// hosted mode; local/tests use the dev key when absent.
    pub crypto_master_key_ref: Option<String>,
}

impl ManagerConfig {
    /// Load and validate configuration from the process environment.
    ///
    /// The explicit mode is authoritative: hosted mode requires a database URL
    /// and a secret provider, and a missing value is a startup error — the
    /// Manager must not silently fall back to local behavior.
    pub fn from_env() -> Result<Self, String> {
        let mode = Mode::parse(std::env::var(MODE_ENV).ok().as_deref())?;

        let database_url = std::env::var(DATABASE_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let configured_secret_provider = std::env::var(SECRET_PROVIDER_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let secret_provider = configured_secret_provider
            .clone()
            .unwrap_or_else(|| DEFAULT_SECRET_PROVIDER.to_string());

        let github_client_secret_ref = std::env::var(GITHUB_CLIENT_SECRET_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let crypto_master_key_ref = std::env::var(CRYPTO_MASTER_KEY_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());

        let auth = auth_from_env(mode == Mode::Hosted, crypto_master_key_ref.clone())?;

        if mode == Mode::Hosted || !auth.client_id.is_empty() {
            auth.validate_origin()?;
        }
        match mode {
            Mode::Hosted => {
                let database_url = database_url.ok_or_else(|| {
                    format!(
                        "hosted mode requires {DATABASE_URL_ENV}; refusing to fall back to local mode"
                    )
                })?;
                if configured_secret_provider.is_none()
                    || secret_provider == DEFAULT_SECRET_PROVIDER
                {
                    return Err(format!(
                        "hosted mode requires a durable {SECRET_PROVIDER_ENV}; in-memory is local/test only"
                    ));
                }
                // Missing hosted authentication configuration must fail closed.
                let github_client_secret_ref = github_client_secret_ref.ok_or_else(|| {
                    format!("hosted mode requires {GITHUB_CLIENT_SECRET_REF_ENV}")
                })?;
                let crypto_master_key_ref = crypto_master_key_ref
                    .ok_or_else(|| format!("hosted mode requires {CRYPTO_MASTER_KEY_REF_ENV}"))?;
                Ok(ManagerConfig {
                    mode,
                    database_url: Some(database_url),
                    secret_provider,
                    http_addr: http_addr_from_env(),
                    auth,
                    github_client_secret_ref: Some(github_client_secret_ref),
                    crypto_master_key_ref: Some(crypto_master_key_ref),
                })
            }
            Mode::Local => Ok(ManagerConfig {
                mode,
                database_url,
                secret_provider,
                http_addr: http_addr_from_env(),
                auth,
                github_client_secret_ref,
                crypto_master_key_ref,
            }),
        }
    }
}

fn http_addr_from_env() -> String {
    std::env::var("OPENFLOWS_MANAGER_ADDR").unwrap_or_else(|_| "127.0.0.1:3002".to_string())
}

/// Build a local/test [`ManagerConfig`] without touching the environment. Used
/// by `AppState` constructors and unit tests that assemble a Manager directly.
pub fn local_test_config() -> ManagerConfig {
    ManagerConfig {
        mode: Mode::Local,
        database_url: None,
        secret_provider: DEFAULT_SECRET_PROVIDER.to_string(),
        http_addr: "127.0.0.1:3002".to_string(),
        auth: AuthConfig {
            client_id: String::new(),
            public_url: "http://127.0.0.1:3002".to_string(),
            github_api_base: "https://api.github.com".to_string(),
            cookie_secure: false,
            allow_loopback: true,
            master_key: Some(dev_master_key()),
        },
        github_client_secret_ref: None,
        crypto_master_key_ref: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        keys: Vec<&'static str>,
        snapshots: Vec<Option<String>>,
    }

    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            let snapshots = keys.iter().map(|k| std::env::var(k).ok()).collect();
            EnvGuard {
                keys: keys.to_vec(),
                snapshots,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.keys.iter().zip(self.snapshots.iter()) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// The full set of env vars the tests touch.
    const ALL_KEYS: &[&str] = &[
        MODE_ENV,
        DATABASE_URL_ENV,
        SECRET_PROVIDER_ENV,
        GITHUB_CLIENT_ID_ENV,
        PUBLIC_URL_ENV,
        GITHUB_CLIENT_SECRET_REF_ENV,
        CRYPTO_MASTER_KEY_REF_ENV,
    ];

    /// Set the env required for a hosted-mode config that should validate.
    fn set_hosted_env() {
        std::env::set_var(MODE_ENV, "hosted");
        std::env::set_var(DATABASE_URL_ENV, "postgres://hosted.example/control");
        std::env::set_var(SECRET_PROVIDER_ENV, "vault");
        std::env::set_var(GITHUB_CLIENT_ID_ENV, "client-id");
        std::env::set_var(PUBLIC_URL_ENV, "https://openflows.example.com");
        std::env::set_var(GITHUB_CLIENT_SECRET_REF_ENV, "ref:github-client-secret");
        std::env::set_var(CRYPTO_MASTER_KEY_REF_ENV, "ref:crypto-master-key");
    }

    #[test]
    fn defaults_to_local_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::remove_var(MODE_ENV);
        std::env::remove_var(DATABASE_URL_ENV);
        std::env::remove_var(SECRET_PROVIDER_ENV);

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Local);
        assert_eq!(cfg.database_url, None);
        assert_eq!(cfg.secret_provider, DEFAULT_SECRET_PROVIDER);
        // Local mode resolves the dev master key (no provider ref configured).
        assert!(cfg.auth.master_key.is_some());
    }

    #[test]
    fn explicit_local_mode_is_preserved() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(MODE_ENV, "local");
        std::env::remove_var(DATABASE_URL_ENV);

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Local);
        assert_eq!(cfg.database_url, None);
    }

    #[test]
    fn hosted_mode_requires_database_url_no_silent_fallback() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(DATABASE_URL_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(
            err.contains(DATABASE_URL_ENV),
            "hosted mode without a database URL must fail, not fall back: {err}"
        );
    }

    #[test]
    fn hosted_mode_accepts_database_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Hosted);
        assert_eq!(
            cfg.database_url.as_deref(),
            Some("postgres://hosted.example/control")
        );
        assert_eq!(cfg.secret_provider, "vault");
        // Hosted mode requires the auth secret refs.
        assert!(cfg.github_client_secret_ref.is_some());
        assert!(cfg.crypto_master_key_ref.is_some());
        // Master key is resolved later from the provider (None here).
        assert!(cfg.auth.master_key.is_none());
    }

    #[test]
    fn hosted_mode_rejects_implicit_in_memory_provider() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(SECRET_PROVIDER_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(SECRET_PROVIDER_ENV));
    }

    #[test]
    fn hosted_mode_requires_github_client_id() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(GITHUB_CLIENT_ID_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(GITHUB_CLIENT_ID_ENV));
    }

    #[test]
    fn hosted_mode_requires_public_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(PUBLIC_URL_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(PUBLIC_URL_ENV));
    }

    #[test]
    fn hosted_mode_requires_secret_refs() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(CRYPTO_MASTER_KEY_REF_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(CRYPTO_MASTER_KEY_REF_ENV));
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(MODE_ENV, "hybrid");

        assert!(
            ManagerConfig::from_env().is_err(),
            "unknown mode must be rejected"
        );
    }
}
