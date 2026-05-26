use config::Registry;
use pair_harness::types::PairConfig;
use std::path::Path;

#[test]
fn test_pair_config_with_proxy() {
    let config = PairConfig::with_proxy(
        "pair-1",
        "T-1",
        Path::new("/tmp/project"),
        Some("redis://localhost:6379".to_string()),
        "http://proxy:4000",
        "ghp_test",
    );

    assert_eq!(config.pair_id, "pair-1");
    assert_eq!(config.proxy_url.as_deref(), Some("http://proxy:4000"));
    assert_eq!(config.redis_url.as_deref(), Some("redis://localhost:6379"));
}

#[test]
fn test_pair_config_without_proxy() {
    let config = PairConfig::new("pair-1", "T-1", Path::new("/tmp/project"), "ghp_test");

    assert!(config.proxy_url.is_none());
}

#[test]
fn test_registry_entry_model_backend() {
    let json = r#"{
      "team": [
        { "id": "forge", "cli": "claude", "active": true, "instances": 2, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "forge-key" },
        { "id": "sentinel", "cli": "claude", "active": true, "instances": 1, "model_backend": "gemini/gemini-2.5-pro", "routing_key": "sentinel-key" }
      ]
    }"#;

    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut tmp, json.as_bytes()).unwrap();

    let reg = Registry::load(tmp.path()).unwrap();
    let forge = reg.get("forge").unwrap();
    assert_eq!(
        forge.model_backend.as_deref(),
        Some("anthropic/claude-sonnet-4-5")
    );
    assert_eq!(forge.routing_key.as_deref(), Some("forge-key"));

    let sentinel = reg.get("sentinel").unwrap();
    assert_eq!(
        sentinel.model_backend.as_deref(),
        Some("gemini/gemini-2.5-pro")
    );
    assert_eq!(sentinel.routing_key.as_deref(), Some("sentinel-key"));
}

#[test]
fn test_registry_entry_backward_compatible() {
    let json = r#"{
      "team": [
        { "id": "forge", "cli": "claude", "active": true, "instances": 2 }
      ]
    }"#;

    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut tmp, json.as_bytes()).unwrap();

    let reg = Registry::load(tmp.path()).unwrap();
    let forge = reg.get("forge").unwrap();
    assert_eq!(forge.model_backend, None);
    assert_eq!(forge.routing_key, None);
}

#[test]
fn test_forge_process_builder_proxy_url_method() {
    let _builder = pair_harness::process::ForgeProcessBuilder::new(
        "pair-1",
        "T-001",
        std::path::PathBuf::from("/tmp/worktree"),
        std::path::PathBuf::from("/tmp/shared"),
    )
    .github_token("ghp_test")
    .proxy_url("http://proxy:4000");
}

#[test]
fn test_proxy_api_key_from_env() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("worktree");
    let shared = dir.path().join("shared");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    let manager = pair_harness::process::ProcessManager::with_proxy(
        "ghp_test",
        None,
        "http://proxy:4000",
        &worktree,
        &shared,
    );
    assert_eq!(manager.proxy_url(), Some("http://proxy:4000"));
    assert!(manager.proxy_api_key().is_none());
}
