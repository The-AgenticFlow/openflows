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
                Ok(ManagerConfig {
                    mode,
                    database_url: Some(database_url),
                    secret_provider,
                    http_addr: http_addr_from_env(),
                })
            }
            Mode::Local => Ok(ManagerConfig {
                mode,
                database_url,
                secret_provider,
                http_addr: http_addr_from_env(),
            }),
        }
    }
}

fn http_addr_from_env() -> String {
    std::env::var("OPENFLOWS_MANAGER_ADDR").unwrap_or_else(|_| "127.0.0.1:3002".to_string())
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

    #[test]
    fn defaults_to_local_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[MODE_ENV, DATABASE_URL_ENV, SECRET_PROVIDER_ENV]);
        std::env::remove_var(MODE_ENV);
        std::env::remove_var(DATABASE_URL_ENV);
        std::env::remove_var(SECRET_PROVIDER_ENV);

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Local);
        assert_eq!(cfg.database_url, None);
        assert_eq!(cfg.secret_provider, DEFAULT_SECRET_PROVIDER);
    }

    #[test]
    fn explicit_local_mode_is_preserved() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[MODE_ENV, DATABASE_URL_ENV]);
        std::env::set_var(MODE_ENV, "local");
        std::env::remove_var(DATABASE_URL_ENV);

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Local);
        assert_eq!(cfg.database_url, None);
    }

    #[test]
    fn hosted_mode_requires_database_url_no_silent_fallback() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[MODE_ENV, DATABASE_URL_ENV]);
        std::env::set_var(MODE_ENV, "hosted");
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
        let _guard = EnvGuard::capture(&[MODE_ENV, DATABASE_URL_ENV, SECRET_PROVIDER_ENV]);
        std::env::set_var(MODE_ENV, "hosted");
        std::env::set_var(DATABASE_URL_ENV, "postgres://hosted.example/control");
        std::env::set_var(SECRET_PROVIDER_ENV, "vault");

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Hosted);
        assert_eq!(
            cfg.database_url.as_deref(),
            Some("postgres://hosted.example/control")
        );
        assert_eq!(cfg.secret_provider, "vault");
    }

    #[test]
    fn hosted_mode_rejects_implicit_in_memory_provider() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[MODE_ENV, DATABASE_URL_ENV, SECRET_PROVIDER_ENV]);
        std::env::set_var(MODE_ENV, "hosted");
        std::env::set_var(DATABASE_URL_ENV, "postgres://hosted.example/control");
        std::env::remove_var(SECRET_PROVIDER_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(SECRET_PROVIDER_ENV));
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[MODE_ENV]);
        std::env::set_var(MODE_ENV, "hybrid");

        assert!(
            ManagerConfig::from_env().is_err(),
            "unknown mode must be rejected"
        );
    }
}
