//! Integration tests for the centralized `config::env` layer (issue #185).

use config::{CoderConfig, EnvConfig, GithubConfig, InfraConfig, TenantConfig};
use envconfig::Envconfig;
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
fn coder_config_parses_defaults_and_overrides() {
    let _g = ENV_LOCK.lock().unwrap();
    let guard = EnvGuard::capture(&[
        "CODER_URL",
        "CODER_ADMIN_EMAIL",
        "CODER_ADMIN_PASSWORD",
        "CODER_IMAGE_TAG",
        "CODER_EXTERNAL_AUTH_0_CLIENT_ID",
        "CODER_EXTERNAL_AUTH_0_CLIENT_SECRET",
    ]);
    guard.unset_all();
    std::env::set_var("CODER_URL", "http://coder.internal:8080");
    std::env::set_var("CODER_EXTERNAL_AUTH_0_CLIENT_ID", "github-app");
    let cfg = CoderConfig::init_from_env().unwrap();
    assert_eq!(cfg.url, "http://coder.internal:8080");
    assert_eq!(cfg.admin_email, "admin@openflows.dev");
    assert_eq!(cfg.admin_password, None);
    assert_eq!(cfg.admin_username, "admin");
    assert_eq!(cfg.image_tag, "v2.37.1");
    assert_eq!(cfg.external_auth_client_id.as_deref(), Some("github-app"));
}

#[test]
fn tenant_openflows_home_uses_default() {
    let _g = ENV_LOCK.lock().unwrap();
    let guard = EnvGuard::capture(&["OPENFLOWS_HOME", "HOME", "USERPROFILE"]);
    guard.unset_all();
    let cfg = TenantConfig::init_from_env().unwrap();
    let home = cfg.openflows_home();
    assert!(home.to_string_lossy().ends_with(".openflows"));
}

#[test]
fn infra_defaults_applied() {
    let _g = ENV_LOCK.lock().unwrap();
    let guard = EnvGuard::capture(&["REDIS_URL", "A2A_RELAY_ADDR"]);
    guard.unset_all();
    let cfg = InfraConfig::init_from_env().unwrap();
    assert_eq!(cfg.redis_url, None);
    assert_eq!(cfg.effective_redis_url(), "redis://localhost:6379");
    assert_eq!(cfg.a2a_relay_addr, "127.0.0.1:3000");
}

#[test]
fn github_config_api_base_default_and_override() {
    let _g = ENV_LOCK.lock().unwrap();
    let guard = EnvGuard::capture(&["GITHUB_API_BASE"]);
    guard.unset_all();
    let cfg = GithubConfig::init_from_env().unwrap();
    assert_eq!(cfg.api_base, "https://api.github.com");

    std::env::set_var("GITHUB_API_BASE", "https://ghe.example.com/api/v3");
    let cfg = GithubConfig::init_from_env().unwrap();
    assert_eq!(cfg.api_base, "https://ghe.example.com/api/v3");
}

#[test]
fn env_config_from_env_and_controller_validation() {
    let _g = ENV_LOCK.lock().unwrap();
    let guard = EnvGuard::capture(&[
        "CODER_SESSION_TOKEN",
        "OPENFLOWS_TENANT",
        "GITHUB_REPOSITORY",
    ]);
    guard.unset_all();
    std::env::set_var("CODER_SESSION_TOKEN", "fake-token");
    std::env::set_var("OPENFLOWS_TENANT", "acme");
    std::env::set_var("GITHUB_REPOSITORY", "acme/repo");
    let env = EnvConfig::from_env().unwrap();
    assert!(env.validate_controller().is_ok());
    assert_eq!(env.tenant.effective_tenant(), "acme");

    // Without OPENFLOWS_TENANT the controller must fail even though a fallback
    // "default" namespace exists for unrelated processes.
    std::env::remove_var("OPENFLOWS_TENANT");
    let env = EnvConfig::from_env().unwrap();
    assert!(env.validate_controller().is_err());
}
