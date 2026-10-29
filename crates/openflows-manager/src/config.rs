//! Centralized, typed configuration for the Manager foundations.
//!
//! All Manager settings are defined here and loaded once at startup, following
//! the same envconfig pattern as the `config` crate. This module owns the
//! deployment mode (`local` vs `hosted`) and enforces the manager requirements
//! that hosted configuration failures must not silently fall back to local
//! behavior: when hosted mode is selected, the required hosted settings must
//! be present or startup fails.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const RUNTIME_MANAGER_URL_ENV: &str = "OPENFLOWS_RUNTIME_MANAGER_URL";

/// Coder's authenticated lifecycle hook endpoint and JWT audience.
pub fn coder_hook_url(public_url: &str) -> Result<String, String> {
    let configured = std::env::var("OPENFLOWS_HOOK_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}/api/v1/hooks/coder/chat",
                public_url.trim_end_matches('/')
            )
        });
    let value = configured.trim();
    let url =
        url::Url::parse(value).map_err(|_| "OPENFLOWS_HOOK_URL must be an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/api/v1/hooks/coder/chat"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("OPENFLOWS_HOOK_URL must target /api/v1/hooks/coder/chat without credentials, query, or fragment".into());
    }
    Ok(value.to_string())
}

/// URL used by Coder workspaces to reach Manager. This can be a private
/// service address while browser, GitHub, and OAuth callbacks use PUBLIC_URL.
pub fn runtime_manager_url(public_url: &str) -> Result<String, String> {
    let configured = std::env::var(RUNTIME_MANAGER_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| public_url.to_string());
    let value = configured.trim().trim_end_matches('/');
    let url = url::Url::parse(value)
        .map_err(|_| "OPENFLOWS_RUNTIME_MANAGER_URL must be an absolute HTTP(S) origin")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("OPENFLOWS_RUNTIME_MANAGER_URL must be an HTTP(S) origin without credentials, path, query, or fragment".into());
    }
    Ok(value.to_string())
}

/// Resolve the tenant-specific hook destination used for transport and JWT audience.
pub fn runtime_hook_url(tenant: uuid::Uuid) -> Result<String, String> {
    runtime_hook_url_from_base(
        &std::env::var("OPENFLOWS_RUNTIME_RELAY_BASE_URL").unwrap_or_default(),
        tenant,
    )
}

fn runtime_hook_url_from_base(base: &str, tenant: uuid::Uuid) -> Result<String, String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("OPENFLOWS_RUNTIME_RELAY_BASE_URL is required for Coder hook relay".into());
    }
    let expanded = base.replace("{tenant}", &tenant.to_string());
    let url = url::Url::parse(&expanded)
        .map_err(|_| "OPENFLOWS_RUNTIME_RELAY_BASE_URL must be an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("OPENFLOWS_RUNTIME_RELAY_BASE_URL must be HTTP(S) without credentials, query, or fragment".into());
    }
    Ok(format!("{expanded}/experimental/hooks/chat"))
}

#[cfg(test)]
mod relay_tests {
    use super::*;

    #[test]
    fn expands_tenant_and_normalizes_base() {
        let tenant = uuid::Uuid::nil();
        assert_eq!(
            runtime_hook_url_from_base(" http://127.0.0.1:7081/tenants/{tenant}/ ", tenant)
                .unwrap(),
            format!("http://127.0.0.1:7081/tenants/{tenant}/experimental/hooks/chat")
        );
    }

    #[test]
    fn rejects_invalid_relay_urls() {
        for base in [
            "",
            "relative/path",
            "ftp://relay",
            "http://user:secret@relay",
            "http://relay?x=1",
            "http://relay#fragment",
        ] {
            assert!(runtime_hook_url_from_base(base, uuid::Uuid::nil()).is_err());
        }
    }
}

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
/// Enables the connected-local capability. In hosted mode this permits the
/// SaaS Manager to enroll local Managers; in local mode it permits this
/// Manager to act as a connected client. The default is disabled so existing
/// deployments preserve their current behavior.
pub const CONNECTED_LOCAL_ENABLED_ENV: &str = "OPENFLOWS_CONNECTED_LOCAL_ENABLED";
/// The environment variable carrying the Openflows control-plane database URL.
pub const DATABASE_URL_ENV: &str = "OPENFLOWS_DATABASE_URL";
/// The environment variable selecting the secret-provider adapter.
pub const SECRET_PROVIDER_ENV: &str = "OPENFLOWS_SECRET_PROVIDER";
pub const AWS_SECRETS_PREFIX_ENV: &str = "OPENFLOWS_AWS_SECRETS_PREFIX";
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

/// The numeric id of the central GitHub App (used to sign App JWTs).
pub const GITHUB_APP_ID_ENV: &str = "OPENFLOWS_GITHUB_APP_ID";
/// The slug of the central GitHub App (used to build the installation setup URL).
pub const GITHUB_APP_SLUG_ENV: &str = "OPENFLOWS_GITHUB_APP_SLUG";
/// Secret-provider reference for the GitHub App private key (RS256 signing).
pub const GITHUB_PRIVATE_KEY_REF_ENV: &str = "OPENFLOWS_GITHUB_PRIVATE_KEY_REF";
/// Secret-provider reference for the GitHub webhook signing secret.
pub const GITHUB_WEBHOOK_SECRET_REF_ENV: &str = "OPENFLOWS_GITHUB_WEBHOOK_SECRET_REF";
/// Maximum accepted webhook request body size in bytes (default 2 MiB).
pub const WEBHOOK_BODY_LIMIT_ENV: &str = "OPENFLOWS_GITHUB_WEBHOOK_BODY_LIMIT";

/// The length (in bytes) of the envelope master key.
pub const MASTER_KEY_BYTES: usize = 32;

/// Default webhook body-size limit: 2 MiB.
pub const DEFAULT_WEBHOOK_BODY_LIMIT: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Coder adapter and template release configuration.
// ---------------------------------------------------------------------------
/// The Coder server URL the manager's platform adapter talks to.
pub const CODER_URL_ENV: &str = "CODER_URL";
/// Secret-provider reference for the platform-only Coder operator token. The
/// token is resolved inside the trusted adapter and never exposed through
/// browser responses, logs, errors, audit/outbox payloads, or template
/// parameters.
pub const CODER_OPERATOR_TOKEN_REF_ENV: &str = "OPENFLOWS_CODER_OPERATOR_TOKEN_REF";
pub const CODER_HOOK_SECRET_REF_ENV: &str = "OPENFLOWS_CODER_HOOK_SECRET_REF";
/// The pinned Coder version baseline for readiness (e.g. `v2.37.3`).
pub const CODER_VERSION_BASELINE_ENV: &str = "OPENFLOWS_CODER_VERSION_BASELINE";
/// The operator-configured provisioner backend adapter name.
pub const PROVISIONER_BACKEND_ENV: &str = "OPENFLOWS_PROVISIONER_BACKEND";
/// Default Coder version baseline.
pub const DEFAULT_CODER_VERSION_BASELINE: &str = "v2.37.3";
/// Default provisioner backend (operator input; validated, not fabricated).
pub const DEFAULT_PROVISIONER_BACKEND: &str = "docker";
pub const REDIS_ADMIN_URL_ENV: &str = "OPENFLOWS_REDIS_ADMIN_URL";
pub const RUNTIME_REDIS_URL_ENV: &str = "OPENFLOWS_RUNTIME_REDIS_URL";

// ---------------------------------------------------------------------------
// Template release source configuration.
//
// These settings select and pin the operator-configured template release
// source. The source is operator configuration, never customer-supplied:
// customer organizations can never provide arbitrary template URLs. A hosted
// source is pinned to an immutable release tag plus the pinned manifest
// SHA-256, so production releases are never resolved from mutable branch names
// or `latest` timestamps.
// ---------------------------------------------------------------------------
/// Selects the template release source: `local` (default) or `github`.
pub const RELEASE_SOURCE_ENV: &str = "OPENFLOWS_RELEASE_SOURCE";
/// The pinned GitHub template repository as `owner/repo`.
pub const TEMPLATE_REPOSITORY_ENV: &str = "OPENFLOWS_GITHUB_TEMPLATE_REPOSITORY";
/// The immutable release tag to resolve. Never `latest` or a branch name.
pub const TEMPLATE_RELEASE_TAG_ENV: &str = "OPENFLOWS_GITHUB_TEMPLATE_TAG";
/// The pinned manifest SHA-256 digest that defines the release identity.
pub const TEMPLATE_MANIFEST_SHA256_ENV: &str = "OPENFLOWS_TEMPLATE_MANIFEST_SHA256";
/// Secret-provider reference for an optional private template repository token.
pub const TEMPLATE_GITHUB_TOKEN_REF_ENV: &str = "OPENFLOWS_GITHUB_TEMPLATE_TOKEN_REF";
/// Maximum accepted bytes for a downloaded release artifact (default 256 MiB).
pub const RELEASE_MAX_DOWNLOAD_BYTES_ENV: &str = "OPENFLOWS_RELEASE_MAX_DOWNLOAD_BYTES";

// ---------------------------------------------------------------------------
// Tenant runtime limits (minimal infrastructure safeguards).
//
// These are platform-configured ceilings enforced centrally in the Manager.
// The defaults are the minimal safeguards named by the specification (max 2
// active tenants per organization, max 2 workers per tenant in addition to its
// Nexus controller, and max 1 active provisioning operation per organization).
// Only platform operators may configure higher limits (these keys are startup
// configuration, never a customer-facing endpoint).
// ---------------------------------------------------------------------------
/// Default maximum active tenants per organization.
pub const DEFAULT_TENANT_MAX_PER_ORG: i32 = 2;
/// Default maximum workers per tenant (in addition to its Nexus controller).
pub const DEFAULT_TENANT_MAX_WORKERS: i32 = 2;
/// Default maximum concurrent provisioning operations per organization.
pub const DEFAULT_TENANT_MAX_PROVISIONING_OPS: i32 = 1;
/// Env var: maximum active tenants per organization.
pub const TENANT_MAX_PER_ORG_ENV: &str = "OPENFLOWS_TENANT_MAX_PER_ORG";
/// Env var: maximum workers per tenant.
pub const TENANT_MAX_WORKERS_ENV: &str = "OPENFLOWS_TENANT_MAX_WORKERS";
/// Env var: maximum concurrent provisioning operations per organization.
pub const TENANT_MAX_PROVISIONING_OPS_ENV: &str = "OPENFLOWS_TENANT_MAX_PROVISIONING_OPS";
/// Per-request download timeout in seconds (default 30).
pub const RELEASE_TIMEOUT_SECS_ENV: &str = "OPENFLOWS_RELEASE_TIMEOUT_SECS";
/// Directory for the bounded verified-artifact cache (default
/// `.openflows/release-cache`).
pub const RELEASE_CACHE_DIR_ENV: &str = "OPENFLOWS_RELEASE_CACHE_DIR";

/// Default maximum downloaded artifact size: 256 MiB.
pub const DEFAULT_RELEASE_MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;
/// Default per-request download timeout.
pub const DEFAULT_RELEASE_TIMEOUT_SECS: u64 = 30;
/// Default verified-artifact cache directory.
pub const DEFAULT_RELEASE_CACHE_DIR: &str = ".openflows/release-cache";
/// Maximum number of releases kept in the verified-artifact cache.
pub const DEFAULT_RELEASE_CACHE_MAX_ENTRIES: usize = 8;

/// The kind of template release source selected by the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseSourceKind {
    /// Local filesystem source: the checked-in `crates/coder-client/templates`
    /// directory (or an operator-configured directory). Preserved for
    /// development and tests; requires no network.
    Local,
    /// Hosted source backed by immutable GitHub Releases.
    Github,
}

impl ReleaseSourceKind {
    /// Parse the `OPENFLOWS_RELEASE_SOURCE` value. An empty/absent value
    /// defaults to `local`. Unknown values are rejected so a misconfigured
    /// deployment fails loudly instead of silently using a local source.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "local" => Ok(ReleaseSourceKind::Local),
            "github" => Ok(ReleaseSourceKind::Github),
            other => Err(format!(
                "invalid {RELEASE_SOURCE_ENV} '{other}': expected 'local' or 'github'"
            )),
        }
    }
}

/// Centralized, typed configuration for the template release source. The
/// source is operator configuration (never customer-supplied), and all
/// environment lookups happen here at startup rather than being scattered
/// through handlers.
#[derive(Debug, Clone)]
pub enum ReleaseSourceConfig {
    /// Local filesystem release source (development and tests). Uses the
    /// existing operator-configured local directory and optional pinned
    /// manifest digest.
    Local,
    /// Hosted release source pinned to an immutable GitHub release tag and a
    /// pinned manifest SHA-256 digest.
    Github(HostedReleaseConfig),
}

impl ReleaseSourceConfig {
    /// Load the release source configuration from the environment. In hosted
    /// mode the required pinned settings must be present or startup fails.
    pub fn from_env() -> Result<Self, String> {
        match ReleaseSourceKind::parse(std::env::var(RELEASE_SOURCE_ENV).ok().as_deref())? {
            ReleaseSourceKind::Local => Ok(ReleaseSourceConfig::Local),
            ReleaseSourceKind::Github => {
                let hosted = HostedReleaseConfig::from_env()?;
                Ok(ReleaseSourceConfig::Github(hosted))
            }
        }
    }
}

/// Configuration for a hosted GitHub release source.
#[derive(Debug, Clone)]
pub struct HostedReleaseConfig {
    /// The pinned GitHub template repository as `owner/repo`.
    pub repository: String,
    /// The immutable release tag to resolve. Never `latest` or a branch name.
    pub tag: String,
    /// The pinned manifest SHA-256 digest; the release identity.
    pub manifest_sha256: String,
    /// Optional secret-provider reference for private GitHub release assets.
    pub github_token_ref: Option<String>,
    /// The GitHub API base (defaults to https://api.github.com).
    pub api_base: String,
    /// Maximum accepted bytes for a downloaded artifact.
    pub max_download_bytes: usize,
    /// Per-request download timeout.
    pub timeout_secs: u64,
    /// Directory for the bounded verified-artifact cache.
    pub cache_dir: std::path::PathBuf,
}

impl HostedReleaseConfig {
    fn from_env() -> Result<Self, String> {
        let repository = std::env::var(TEMPLATE_REPOSITORY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("hosted release source requires {TEMPLATE_REPOSITORY_ENV}"))?;
        if !is_repository_name(&repository) {
            return Err(format!(
                "{TEMPLATE_REPOSITORY_ENV} must be in the form 'owner/repo'"
            ));
        }

        let tag = std::env::var(TEMPLATE_RELEASE_TAG_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("hosted release source requires {TEMPLATE_RELEASE_TAG_ENV}"))?;
        // A release must be pinned to an immutable tag. Never resolve from the
        // mutable `latest` endpoint, a branch name, or a ref path.
        const MUTABLE_NAMES: &[&str] = &[
            "latest",
            "main",
            "master",
            "develop",
            "dev",
            "trunk",
            "head",
            "production",
            "staging",
        ];
        let normalized = tag.to_ascii_lowercase();
        if MUTABLE_NAMES.contains(&normalized.as_str())
            || tag.contains('/')
            || tag.contains('*')
            || tag.contains('?')
        {
            return Err(format!(
                "{TEMPLATE_RELEASE_TAG_ENV} must be an immutable release tag, not a mutable branch, ref path, or 'latest'"
            ));
        }

        let manifest_sha256 = std::env::var(TEMPLATE_MANIFEST_SHA256_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                format!("hosted release source requires {TEMPLATE_MANIFEST_SHA256_ENV}")
            })?;
        let manifest_sha256 = manifest_sha256.to_ascii_lowercase();
        if manifest_sha256.len() != 64 || !manifest_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "{TEMPLATE_MANIFEST_SHA256_ENV} must be a 64-character SHA-256 hex digest"
            ));
        }

        let github_token_ref = std::env::var(TEMPLATE_GITHUB_TOKEN_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());

        let api_base = std::env::var(GITHUB_API_BASE_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "https://api.github.com".to_string());

        let max_download_bytes = match std::env::var(RELEASE_MAX_DOWNLOAD_BYTES_ENV).ok() {
            None => DEFAULT_RELEASE_MAX_DOWNLOAD_BYTES,
            Some(v) => match v.trim().parse::<usize>() {
                Ok(n) if n >= 1024 => n,
                _ => {
                    return Err(format!(
                        "{RELEASE_MAX_DOWNLOAD_BYTES_ENV} must be a positive byte count"
                    ))
                }
            },
        };

        let timeout_secs = match std::env::var(RELEASE_TIMEOUT_SECS_ENV).ok() {
            None => DEFAULT_RELEASE_TIMEOUT_SECS,
            Some(v) => match v.trim().parse::<u64>() {
                Ok(n) if n >= 1 => n,
                _ => {
                    return Err(format!(
                        "{RELEASE_TIMEOUT_SECS_ENV} must be a positive number of seconds"
                    ))
                }
            },
        };

        let cache_dir = std::env::var(RELEASE_CACHE_DIR_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_RELEASE_CACHE_DIR));

        Ok(HostedReleaseConfig {
            repository,
            tag,
            manifest_sha256,
            github_token_ref,
            api_base,
            max_download_bytes,
            timeout_secs,
            cache_dir,
        })
    }
}

/// Validate a `owner/repo` GitHub repository reference.
fn is_repository_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo_name = parts.next().unwrap_or_default();
    parts.next().is_none()
        && !owner.is_empty()
        && !repo_name.is_empty()
        && owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && repo_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

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

/// The parsed value of the webhook body-size limit, validated against a sane
/// minimum so a misconfiguration cannot silently disable the limit.
fn webhook_body_limit_from_env() -> usize {
    let raw = std::env::var(WEBHOOK_BODY_LIMIT_ENV).ok();
    match raw {
        None => DEFAULT_WEBHOOK_BODY_LIMIT,
        Some(v) => match v.trim().parse::<usize>() {
            Ok(n) if n >= 1024 => n,
            _ => DEFAULT_WEBHOOK_BODY_LIMIT,
        },
    }
}

/// Centralized GitHub App configuration for the installation connection
/// lifecycle: App JWT signing, installation metadata lookups, and
/// webhook signature verification. These reference secrets through the secret
/// provider; the private key and webhook secret are never plaintext config.
#[derive(Debug, Clone)]
pub struct GithubAppConfig {
    /// The central GitHub App numeric id, used to sign App JWTs.
    pub app_id: Option<i64>,
    /// The central GitHub App slug, used to build the installation setup URL.
    pub app_slug: Option<String>,
    /// Secret-provider reference for the GitHub App private key (RS256).
    pub private_key_ref: Option<String>,
    /// Secret-provider reference for the GitHub webhook signing secret.
    pub webhook_secret_ref: Option<String>,
    /// Maximum accepted webhook request body size in bytes.
    pub webhook_body_limit: usize,
}

/// Load the GitHub App configuration from the environment. Local mode defaults
/// are unset (the connection lifecycle fails closed until configured).
pub fn github_app_from_env() -> GithubAppConfig {
    GithubAppConfig {
        app_id: std::env::var(GITHUB_APP_ID_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| v.trim().parse::<i64>().ok()),
        app_slug: std::env::var(GITHUB_APP_SLUG_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty()),
        private_key_ref: std::env::var(GITHUB_PRIVATE_KEY_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty()),
        webhook_secret_ref: std::env::var(GITHUB_WEBHOOK_SECRET_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty()),
        webhook_body_limit: webhook_body_limit_from_env(),
    }
}

/// Tenant runtime limits (minimal infrastructure safeguards).
///
/// These ceilings are enforced centrally by the Manager. They are operator
/// startup configuration only: there is no customer-facing endpoint to raise
/// them, so only a platform operator (with write access to the Manager
/// environment) can configure higher limits. Defaults are the minimal
/// safeguards named by the specification.
#[derive(Debug, Clone)]
pub struct TenantLimits {
    /// Maximum active tenants per organization.
    pub max_tenants_per_org: i32,
    /// Maximum workers per tenant, in addition to its Nexus controller.
    pub max_workers_per_tenant: i32,
    /// Maximum concurrent provisioning operations per organization. When the
    /// organization's execution slot is occupied, further provisioning work is
    /// queued rather than rejected as a resource-limit violation.
    pub max_concurrent_provisioning_ops: i32,
}

impl TenantLimits {
    fn from_env() -> Result<Self, String> {
        Ok(TenantLimits {
            max_tenants_per_org: parse_positive(
                TENANT_MAX_PER_ORG_ENV,
                DEFAULT_TENANT_MAX_PER_ORG,
            )?,
            max_workers_per_tenant: parse_positive(
                TENANT_MAX_WORKERS_ENV,
                DEFAULT_TENANT_MAX_WORKERS,
            )?,
            max_concurrent_provisioning_ops: parse_positive(
                TENANT_MAX_PROVISIONING_OPS_ENV,
                DEFAULT_TENANT_MAX_PROVISIONING_OPS,
            )?,
        })
    }
}

impl Default for TenantLimits {
    fn default() -> Self {
        TenantLimits {
            max_tenants_per_org: DEFAULT_TENANT_MAX_PER_ORG,
            max_workers_per_tenant: DEFAULT_TENANT_MAX_WORKERS,
            max_concurrent_provisioning_ops: DEFAULT_TENANT_MAX_PROVISIONING_OPS,
        }
    }
}

/// Parse a positive-integer environment setting, defaulting when absent.
fn parse_positive(env: &str, default: i32) -> Result<i32, String> {
    match std::env::var(env).ok() {
        None => Ok(default),
        Some(v) => match v.trim().parse::<i32>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => Err(format!("{env} must be a positive integer")),
        },
    }
}

/// Parse a strict boolean environment setting. Empty values are treated as
/// absent and use the supplied default; ambiguous values fail closed.
fn parse_bool(env: &str, default: bool) -> Result<bool, String> {
    match std::env::var(env).ok().as_deref().map(str::trim) {
        None | Some("") => Ok(default),
        Some("1") | Some("true") | Some("yes") | Some("on") => Ok(true),
        Some("0") | Some("false") | Some("no") | Some("off") => Ok(false),
        Some(_) => Err(format!("invalid {env}; expected true/false (or 1/0)")),
    }
}

/// The foundational Manager configuration.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub mode: Mode,
    /// Whether connected-local enrollment/client behavior is enabled. This is
    /// deliberately independent from `mode`: hosted and local deployments
    /// use the same binary but have different roles when the capability is on.
    pub connected_local_enabled: bool,
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
    /// Centralized GitHub App installation-connection settings.
    pub github_app: GithubAppConfig,
    /// Secret-provider reference for the GitHub App client secret (OAuth
    /// exchange). Required in hosted mode.
    pub github_client_secret_ref: Option<String>,
    /// Secret-provider reference for the envelope master key. Required in
    /// hosted mode; local/tests use the dev key when absent.
    pub crypto_master_key_ref: Option<String>,
    /// Coder server URL the platform adapter uses.
    pub coder_url: Option<String>,
    /// Secret-provider reference for the platform-only Coder operator token.
    pub coder_operator_token_ref: Option<String>,
    pub coder_hook_secret_ref: Option<String>,
    /// The pinned Coder version baseline for readiness.
    pub coder_version_baseline: String,
    /// The operator-configured provisioner backend adapter.
    pub provisioner_backend: String,
    /// Privileged Redis endpoint used only by the Manager to create/revoke ACL
    /// users and clear immutable tenant namespaces.
    pub redis_admin_url: Option<String>,
    /// Non-privileged Redis origin placed into per-workspace secret material;
    /// tenant username/password are added by the injection backend.
    pub runtime_redis_url: Option<String>,
    /// The operator-configured template release source.
    pub release_source: ReleaseSourceConfig,
    /// Tenant runtime limits (minimal infrastructure safeguards).
    pub tenant_limits: TenantLimits,
}

impl ManagerConfig {
    /// Load and validate configuration from the process environment.
    ///
    /// The explicit mode is authoritative: hosted mode requires a database URL
    /// and a secret provider, and a missing value is a startup error — the
    /// Manager must not silently fall back to local behavior.
    pub fn from_env() -> Result<Self, String> {
        let mode = Mode::parse(std::env::var(MODE_ENV).ok().as_deref())?;
        let connected_local_enabled = parse_bool(CONNECTED_LOCAL_ENABLED_ENV, false)?;

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

        // Coder adapter configuration.
        let coder_url = std::env::var(CODER_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let coder_operator_token_ref = std::env::var(CODER_OPERATOR_TOKEN_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let coder_hook_secret_ref = std::env::var(CODER_HOOK_SECRET_REF_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let coder_version_baseline = std::env::var(CODER_VERSION_BASELINE_ENV)
            .unwrap_or_else(|_| DEFAULT_CODER_VERSION_BASELINE.to_string());
        let provisioner_backend = std::env::var(PROVISIONER_BACKEND_ENV)
            .unwrap_or_else(|_| DEFAULT_PROVISIONER_BACKEND.to_string());
        let redis_admin_url = std::env::var(REDIS_ADMIN_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let runtime_redis_url = std::env::var(RUNTIME_REDIS_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());

        let release_source = ReleaseSourceConfig::from_env()?;

        let auth = auth_from_env(mode == Mode::Hosted, crypto_master_key_ref.clone())?;

        let tenant_limits = TenantLimits::from_env()?;

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
                if secret_provider != crate::secrets::AWS_SECRETS_MANAGER_PROVIDER {
                    return Err(format!(
                        "hosted mode requires {SECRET_PROVIDER_ENV}=aws-secrets-manager; filesystem and in-memory are local/test only"
                    ));
                }
                if std::env::var(AWS_SECRETS_PREFIX_ENV)
                    .ok()
                    .is_none_or(|prefix| prefix.trim().is_empty())
                {
                    return Err(format!("hosted mode requires {AWS_SECRETS_PREFIX_ENV}"));
                }
                // Missing hosted authentication configuration must fail closed.
                let github_client_secret_ref = github_client_secret_ref.ok_or_else(|| {
                    format!("hosted mode requires {GITHUB_CLIENT_SECRET_REF_ENV}")
                })?;
                let crypto_master_key_ref = crypto_master_key_ref
                    .ok_or_else(|| format!("hosted mode requires {CRYPTO_MASTER_KEY_REF_ENV}"))?;
                let github_app = github_app_from_env();
                if github_app.app_slug.is_none() {
                    return Err(format!("hosted mode requires {GITHUB_APP_SLUG_ENV}"));
                }
                github_app
                    .app_id
                    .ok_or_else(|| format!("hosted mode requires {GITHUB_APP_ID_ENV}"))?;
                if github_app.private_key_ref.is_none() {
                    return Err(format!("hosted mode requires {GITHUB_PRIVATE_KEY_REF_ENV}"));
                }
                if github_app.webhook_secret_ref.is_none() {
                    return Err(format!(
                        "hosted mode requires {GITHUB_WEBHOOK_SECRET_REF_ENV}"
                    ));
                }
                let coder_url =
                    coder_url.ok_or_else(|| format!("hosted mode requires {CODER_URL_ENV}"))?;
                let coder_operator_token_ref = coder_operator_token_ref.ok_or_else(|| {
                    format!("hosted mode requires {CODER_OPERATOR_TOKEN_REF_ENV}")
                })?;
                let coder_hook_secret_ref = coder_hook_secret_ref
                    .ok_or_else(|| format!("hosted mode requires {CODER_HOOK_SECRET_REF_ENV}"))?;
                let redis_admin_url = redis_admin_url
                    .ok_or_else(|| format!("hosted mode requires {REDIS_ADMIN_URL_ENV}"))?;
                let runtime_redis_url = runtime_redis_url
                    .ok_or_else(|| format!("hosted mode requires {RUNTIME_REDIS_URL_ENV}"))?;
                if provisioner_backend == DEFAULT_PROVISIONER_BACKEND {
                    return Err(format!(
                        "hosted mode refuses shared Docker provisioning: set {PROVISIONER_BACKEND_ENV} to a backend with validated per-tenant network and secret-mount isolation"
                    ));
                }
                Ok(ManagerConfig {
                    mode,
                    connected_local_enabled,
                    database_url: Some(database_url),
                    secret_provider,
                    http_addr: http_addr_from_env(),
                    auth,
                    github_app,
                    github_client_secret_ref: Some(github_client_secret_ref),
                    crypto_master_key_ref: Some(crypto_master_key_ref),
                    coder_url: Some(coder_url),
                    coder_operator_token_ref: Some(coder_operator_token_ref),
                    coder_hook_secret_ref: Some(coder_hook_secret_ref),
                    coder_version_baseline,
                    provisioner_backend,
                    redis_admin_url: Some(redis_admin_url),
                    runtime_redis_url: Some(runtime_redis_url),
                    release_source,
                    tenant_limits,
                })
            }
            Mode::Local => Ok(ManagerConfig {
                mode,
                connected_local_enabled,
                database_url,
                secret_provider,
                http_addr: http_addr_from_env(),
                auth,
                github_app: github_app_from_env(),
                github_client_secret_ref,
                crypto_master_key_ref,
                coder_url,
                coder_operator_token_ref,
                coder_hook_secret_ref,
                coder_version_baseline,
                provisioner_backend,
                redis_admin_url,
                runtime_redis_url,
                release_source,
                tenant_limits,
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
        connected_local_enabled: false,
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
        coder_url: None,
        coder_operator_token_ref: None,
        coder_hook_secret_ref: None,
        coder_version_baseline: DEFAULT_CODER_VERSION_BASELINE.to_string(),
        provisioner_backend: DEFAULT_PROVISIONER_BACKEND.to_string(),
        redis_admin_url: None,
        runtime_redis_url: None,
        github_app: GithubAppConfig {
            app_id: None,
            app_slug: None,
            private_key_ref: None,
            webhook_secret_ref: None,
            webhook_body_limit: DEFAULT_WEBHOOK_BODY_LIMIT,
        },
        release_source: ReleaseSourceConfig::Local,
        tenant_limits: TenantLimits {
            max_tenants_per_org: DEFAULT_TENANT_MAX_PER_ORG,
            max_workers_per_tenant: DEFAULT_TENANT_MAX_WORKERS,
            max_concurrent_provisioning_ops: DEFAULT_TENANT_MAX_PROVISIONING_OPS,
        },
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
        CONNECTED_LOCAL_ENABLED_ENV,
        DATABASE_URL_ENV,
        SECRET_PROVIDER_ENV,
        AWS_SECRETS_PREFIX_ENV,
        GITHUB_CLIENT_ID_ENV,
        PUBLIC_URL_ENV,
        RUNTIME_MANAGER_URL_ENV,
        "OPENFLOWS_HOOK_URL",
        GITHUB_CLIENT_SECRET_REF_ENV,
        CRYPTO_MASTER_KEY_REF_ENV,
        GITHUB_APP_ID_ENV,
        GITHUB_APP_SLUG_ENV,
        GITHUB_PRIVATE_KEY_REF_ENV,
        GITHUB_WEBHOOK_SECRET_REF_ENV,
        WEBHOOK_BODY_LIMIT_ENV,
        CODER_URL_ENV,
        CODER_OPERATOR_TOKEN_REF_ENV,
        CODER_HOOK_SECRET_REF_ENV,
        CODER_VERSION_BASELINE_ENV,
        PROVISIONER_BACKEND_ENV,
        REDIS_ADMIN_URL_ENV,
        RUNTIME_REDIS_URL_ENV,
        RELEASE_SOURCE_ENV,
        TEMPLATE_REPOSITORY_ENV,
        TEMPLATE_RELEASE_TAG_ENV,
        TEMPLATE_MANIFEST_SHA256_ENV,
        TEMPLATE_GITHUB_TOKEN_REF_ENV,
        GITHUB_API_BASE_ENV,
        RELEASE_MAX_DOWNLOAD_BYTES_ENV,
        RELEASE_TIMEOUT_SECS_ENV,
        RELEASE_CACHE_DIR_ENV,
        TENANT_MAX_PER_ORG_ENV,
        TENANT_MAX_WORKERS_ENV,
        TENANT_MAX_PROVISIONING_OPS_ENV,
    ];

    #[test]
    fn runtime_manager_origin_can_use_private_container_route() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RUNTIME_MANAGER_URL_ENV, "http://host.docker.internal:3002");
        assert_eq!(
            runtime_manager_url("https://public.example").unwrap(),
            "http://host.docker.internal:3002"
        );
        std::env::set_var(
            RUNTIME_MANAGER_URL_ENV,
            "http://user:pass@host:3002/private",
        );
        assert!(runtime_manager_url("https://public.example").is_err());
    }

    #[test]
    fn coder_hook_audience_can_use_private_container_route() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(
            "OPENFLOWS_HOOK_URL",
            "http://host.docker.internal:3002/api/v1/hooks/coder/chat",
        );
        assert_eq!(
            coder_hook_url("https://public.example").unwrap(),
            "http://host.docker.internal:3002/api/v1/hooks/coder/chat"
        );
        std::env::set_var(
            "OPENFLOWS_HOOK_URL",
            "http://host.docker.internal:3002/wrong",
        );
        assert!(coder_hook_url("https://public.example").is_err());
    }

    /// Set the env required for a hosted-mode config that should validate.
    fn set_hosted_env() {
        std::env::set_var(MODE_ENV, "hosted");
        std::env::set_var(DATABASE_URL_ENV, "postgres://hosted.example/control");
        std::env::set_var(SECRET_PROVIDER_ENV, "aws-secrets-manager");
        std::env::set_var(AWS_SECRETS_PREFIX_ENV, "openflows/staging");
        std::env::set_var(GITHUB_CLIENT_ID_ENV, "client-id");
        std::env::set_var(PUBLIC_URL_ENV, "https://openflows.example.com");
        std::env::set_var(CODER_HOOK_SECRET_REF_ENV, "ref:coder-hook-secret");
        std::env::set_var(REDIS_ADMIN_URL_ENV, "redis://admin:secret@redis:6379");
        std::env::set_var(RUNTIME_REDIS_URL_ENV, "redis://redis:6379");
        std::env::set_var(GITHUB_CLIENT_SECRET_REF_ENV, "ref:github-client-secret");
        std::env::set_var(CRYPTO_MASTER_KEY_REF_ENV, "ref:crypto-master-key");
        std::env::set_var(GITHUB_APP_ID_ENV, "123456");
        std::env::set_var(GITHUB_APP_SLUG_ENV, "openflows-app");
        std::env::set_var(GITHUB_PRIVATE_KEY_REF_ENV, "ref:github-private-key");
        std::env::set_var(GITHUB_WEBHOOK_SECRET_REF_ENV, "ref:github-webhook-secret");
        std::env::set_var(CODER_URL_ENV, "https://coder.example.com");
        std::env::set_var(CODER_OPERATOR_TOKEN_REF_ENV, "ref:coder-operator-token");
        std::env::set_var(PROVISIONER_BACKEND_ENV, "kubernetes");
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
        assert!(!cfg.connected_local_enabled);
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
        assert!(!cfg.connected_local_enabled);
        assert_eq!(cfg.database_url, None);
    }

    #[test]
    fn connected_local_capability_is_opt_in_and_parsed_strictly() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);

        std::env::set_var(MODE_ENV, "local");
        std::env::set_var(CONNECTED_LOCAL_ENABLED_ENV, "true");
        let cfg = ManagerConfig::from_env().unwrap();
        assert!(cfg.connected_local_enabled);

        std::env::set_var(CONNECTED_LOCAL_ENABLED_ENV, "maybe");
        let error = ManagerConfig::from_env().unwrap_err();
        assert!(error.contains(CONNECTED_LOCAL_ENABLED_ENV));
    }

    #[test]
    fn connected_local_capability_is_available_in_hosted_mode_without_changing_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::set_var(CONNECTED_LOCAL_ENABLED_ENV, "on");

        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.mode, Mode::Hosted);
        assert!(cfg.connected_local_enabled);
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
        assert_eq!(cfg.secret_provider, "aws-secrets-manager");
        // Hosted mode requires the auth secret refs.
        assert!(cfg.github_client_secret_ref.is_some());
        assert!(cfg.crypto_master_key_ref.is_some());
        // Master key is resolved later from the provider (None here).
        assert!(cfg.auth.master_key.is_none());
        assert_eq!(cfg.provisioner_backend, "kubernetes");
    }

    #[test]
    fn hosted_mode_rejects_unisolated_shared_docker_backend() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::set_var(PROVISIONER_BACKEND_ENV, DEFAULT_PROVISIONER_BACKEND);

        let error = ManagerConfig::from_env().unwrap_err();
        assert!(error.contains("refuses shared Docker provisioning"));
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
    fn hosted_mode_rejects_filesystem_secrets() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::set_var(SECRET_PROVIDER_ENV, "filesystem");

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains("aws-secrets-manager"));
    }

    #[test]
    fn hosted_mode_requires_aws_namespace() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        set_hosted_env();
        std::env::remove_var(AWS_SECRETS_PREFIX_ENV);

        let err = ManagerConfig::from_env().unwrap_err();
        assert!(err.contains(AWS_SECRETS_PREFIX_ENV));
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

    // -----------------------------------------------------------------------
    // Release source selection and validation.
    // -----------------------------------------------------------------------

    fn set_github_release_env() {
        std::env::set_var(RELEASE_SOURCE_ENV, "github");
        std::env::set_var(
            TEMPLATE_REPOSITORY_ENV,
            "The-AgenticFlow/openflows-templates",
        );
        std::env::set_var(TEMPLATE_RELEASE_TAG_ENV, "2026.10.0");
        std::env::set_var(TEMPLATE_MANIFEST_SHA256_ENV, "a".repeat(64));
    }

    #[test]
    fn release_source_defaults_to_local() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::remove_var(RELEASE_SOURCE_ENV);
        std::env::remove_var(TEMPLATE_REPOSITORY_ENV);

        let cfg = ReleaseSourceConfig::from_env().unwrap();
        assert!(matches!(cfg, ReleaseSourceConfig::Local));
    }

    #[test]
    fn unknown_release_source_is_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RELEASE_SOURCE_ENV, "s3");

        assert!(ReleaseSourceConfig::from_env().is_err());
    }

    #[test]
    fn github_source_requires_repository_tag_and_digest() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RELEASE_SOURCE_ENV, "github");
        std::env::remove_var(TEMPLATE_REPOSITORY_ENV);
        std::env::remove_var(TEMPLATE_RELEASE_TAG_ENV);
        std::env::remove_var(TEMPLATE_MANIFEST_SHA256_ENV);

        assert!(ReleaseSourceConfig::from_env().is_err());

        std::env::set_var(
            TEMPLATE_REPOSITORY_ENV,
            "The-AgenticFlow/openflows-templates",
        );
        assert!(ReleaseSourceConfig::from_env().is_err());

        std::env::set_var(TEMPLATE_RELEASE_TAG_ENV, "2026.10.0");
        assert!(ReleaseSourceConfig::from_env().is_err());

        std::env::set_var(TEMPLATE_MANIFEST_SHA256_ENV, "a".repeat(64));
        let cfg = ReleaseSourceConfig::from_env().unwrap();
        match cfg {
            ReleaseSourceConfig::Github(h) => {
                assert_eq!(h.repository, "The-AgenticFlow/openflows-templates");
                assert_eq!(h.tag, "2026.10.0");
                assert_eq!(h.manifest_sha256.len(), 64);
            }
            other => panic!("expected github source, got {other:?}"),
        }

        std::env::set_var(TEMPLATE_MANIFEST_SHA256_ENV, "A".repeat(64));
        match ReleaseSourceConfig::from_env().unwrap() {
            ReleaseSourceConfig::Github(h) => assert_eq!(h.manifest_sha256, "a".repeat(64)),
            other => panic!("expected github source, got {other:?}"),
        }
    }

    #[test]
    fn github_source_rejects_mutable_branch_or_latest_tag() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RELEASE_SOURCE_ENV, "github");
        set_github_release_env();

        for bad in ["latest", "main", "feature/x", "dev/feature?x"] {
            std::env::set_var(TEMPLATE_RELEASE_TAG_ENV, bad);
            assert!(
                ReleaseSourceConfig::from_env().is_err(),
                "tag '{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn github_source_rejects_malformed_repository_and_digest() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RELEASE_SOURCE_ENV, "github");
        set_github_release_env();

        std::env::set_var(TEMPLATE_REPOSITORY_ENV, "no-slash");
        assert!(ReleaseSourceConfig::from_env().is_err());
        std::env::set_var(TEMPLATE_REPOSITORY_ENV, "a/b/c");
        assert!(ReleaseSourceConfig::from_env().is_err());

        set_github_release_env();
        std::env::set_var(TEMPLATE_MANIFEST_SHA256_ENV, "not-a-digest");
        assert!(ReleaseSourceConfig::from_env().is_err());
    }

    #[test]
    fn github_source_validates_size_and_timeout() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::set_var(RELEASE_SOURCE_ENV, "github");
        set_github_release_env();

        std::env::set_var(RELEASE_MAX_DOWNLOAD_BYTES_ENV, "0");
        assert!(ReleaseSourceConfig::from_env().is_err());
        set_github_release_env();
        std::env::set_var(RELEASE_MAX_DOWNLOAD_BYTES_ENV, "1048576");
        std::env::set_var(RELEASE_TIMEOUT_SECS_ENV, "0");
        assert!(ReleaseSourceConfig::from_env().is_err());
        set_github_release_env();
        std::env::set_var(RELEASE_TIMEOUT_SECS_ENV, "15");
        std::env::set_var(RELEASE_MAX_DOWNLOAD_BYTES_ENV, "1048576");
        match ReleaseSourceConfig::from_env().unwrap() {
            ReleaseSourceConfig::Github(h) => {
                assert_eq!(h.max_download_bytes, 1048576);
                assert_eq!(h.timeout_secs, 15);
            }
            other => panic!("expected github source, got {other:?}"),
        }
    }

    #[test]
    fn local_mode_does_not_require_github_config() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::remove_var(RELEASE_SOURCE_ENV);
        std::env::remove_var(TEMPLATE_REPOSITORY_ENV);
        std::env::remove_var(TEMPLATE_RELEASE_TAG_ENV);
        std::env::remove_var(TEMPLATE_MANIFEST_SHA256_ENV);

        let cfg = ManagerConfig::from_env().unwrap();
        assert!(matches!(cfg.release_source, ReleaseSourceConfig::Local));
    }

    #[test]
    fn tenant_limits_default_to_minimal_safeguards_and_validate() {
        let _g = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(ALL_KEYS);
        std::env::remove_var(TENANT_MAX_PER_ORG_ENV);
        std::env::remove_var(TENANT_MAX_WORKERS_ENV);
        std::env::remove_var(TENANT_MAX_PROVISIONING_OPS_ENV);
        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.tenant_limits.max_tenants_per_org, 2);
        assert_eq!(cfg.tenant_limits.max_workers_per_tenant, 2);
        assert_eq!(cfg.tenant_limits.max_concurrent_provisioning_ops, 1);

        std::env::set_var(TENANT_MAX_PER_ORG_ENV, "5");
        std::env::set_var(TENANT_MAX_WORKERS_ENV, "3");
        std::env::set_var(TENANT_MAX_PROVISIONING_OPS_ENV, "2");
        let cfg = ManagerConfig::from_env().unwrap();
        assert_eq!(cfg.tenant_limits.max_tenants_per_org, 5);
        assert_eq!(cfg.tenant_limits.max_workers_per_tenant, 3);
        assert_eq!(cfg.tenant_limits.max_concurrent_provisioning_ops, 2);

        std::env::set_var(TENANT_MAX_PER_ORG_ENV, "0");
        assert!(ManagerConfig::from_env().is_err());
        std::env::set_var(TENANT_MAX_PER_ORG_ENV, "abc");
        assert!(ManagerConfig::from_env().is_err());
    }
}
