//! Tenant Redis credentials: recovery of lost secrets, and which runtime
//! bundle gets the controller user and the hook key.

use super::*;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_tenant_secrets_recover_existing_unique_rows() {
    let h = harness().await;
    let tenant = create_tenant(&h, "secret-recovery", 1, "secret-recovery").await;
    let redis = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    let hook = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    h.secret_provider.delete(&redis.secret_ref).await.unwrap();
    h.secret_provider.delete(&hook.secret_ref).await.unwrap();
    // Reproduce both a lost provider and a previous attempt that already
    // revoked the row before failing to allocate its replacement.
    sqlx::query("UPDATE tenant_redis_credentials SET revoked_at = now() WHERE tenant_id = $1")
        .bind(tenant.0)
        .execute(&h.pool)
        .await
        .unwrap();
    let recovered = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    let recovered_hook = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    assert_eq!(recovered.generation, redis.generation + 1);
    assert_eq!(recovered_hook.generation, hook.generation + 1);
    assert_ne!(recovered.credential_hash, redis.credential_hash);
    assert_ne!(recovered_hook.key_hash, hook.key_hash);
    assert_eq!(recovered.namespace, redis.namespace);
    assert_eq!(recovered.username, redis.username);
    assert!(h.secret_provider.get(&recovered.secret_ref).await.is_ok());
    assert!(h
        .secret_provider
        .get(&recovered_hook.secret_ref)
        .await
        .is_ok());
    let again = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    assert_eq!(again.credential_hash, recovered.credential_hash);
    let again = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    assert_eq!(again.key_hash, recovered_hook.key_hash);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn only_the_controller_bundle_carries_controller_redis_and_hook_key() {
    let h = harness().await;
    let tenant = create_tenant(&h, "bundle-split", 1, "bundle-split").await;
    let redis = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "bundle-redis")
        .await
        .unwrap();
    h.tenant_repo
        .ensure_hook_key(h.org, tenant, "bundle-hook")
        .await
        .unwrap();
    let bootstrap = h
        .secret_provider
        .put("bundle-bootstrap", b"bootstrap")
        .await
        .unwrap();
    let bundle = |role: &'static str| {
        let h = &h;
        let bootstrap = bootstrap.clone();
        async move {
            let workspace = h
                .tenant_repo
                .insert_workspace(h.org, tenant, role, Some(0))
                .await
                .unwrap();
            let reference = h
                .tenant_repo
                .prepare_workspace_secret(
                    h.org,
                    tenant,
                    workspace,
                    &bootstrap,
                    "http://manager",
                    "redis://redis:6379",
                )
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(
                &h.secret_provider.get(&reference).await.unwrap(),
            )
            .unwrap()
        }
    };
    let controller = bundle("nexus").await;
    let worker = bundle("lore").await;
    let controller_user =
        openflows_manager::runtime::tenant::controller_redis_username(&redis.username);
    assert_eq!(controller["redis_username"], controller_user.as_str());
    assert!(controller["redis_url"]
        .as_str()
        .unwrap()
        .contains(&format!("{controller_user}:")));
    assert!(controller["hook_key"].is_string());
    assert_eq!(worker["redis_username"], redis.username.as_str());
    assert!(worker["redis_url"]
        .as_str()
        .unwrap()
        .contains(&format!("{}:", redis.username)));
    assert!(worker.get("hook_key").is_none(), "{worker}");
    assert!(worker.get("hook_url").is_none(), "{worker}");
}
