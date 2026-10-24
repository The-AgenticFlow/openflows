//! Explicit live Docker/Coder smoke test. Uses the configured local Manager
//! database and leaves its managed provisioner running for onboarding.
use openflows_manager::{
    auth::crypto::EnvelopeCipher, db::Db, provisioning::docker::DockerProvisioner,
};

#[tokio::test]
#[ignore = "creates a managed provisioner in the explicitly selected local organization"]
async fn managed_provisioner_live_reuses_key_and_reports_online() {
    std::env::set_current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).unwrap();
    dotenvy::from_filename(".env.prod").ok();
    let product_org = std::env::var("OPENFLOWS_TEST_PROVISIONER_ORG")
        .expect("explicit OpenFlows org UUID required");
    let db = Db::connect(&std::env::var("OPENFLOWS_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let org: String =
        sqlx::query_scalar("SELECT coder_organization_id FROM organizations WHERE id=$1")
            .bind(uuid::Uuid::parse_str(&product_org).unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let secrets: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("OPENFLOWS_DEV_SECRETS_FILE").unwrap()).unwrap(),
    )
    .unwrap();
    let token_ref = std::env::var("OPENFLOWS_CODER_OPERATOR_TOKEN_REF").unwrap();
    let master_ref = std::env::var("OPENFLOWS_CRYPTO_MASTER_KEY_REF").unwrap();
    let master: [u8; 32] = secrets[master_ref]
        .as_str()
        .unwrap()
        .as_bytes()
        .try_into()
        .unwrap();
    let url = std::env::var("CODER_URL").unwrap();
    assert!(
        url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"),
        "live test is restricted to local Coder"
    );
    let client = coder_client::CoderClient::new(&url, secrets[token_ref].as_str().unwrap());
    let backend = DockerProvisioner::from_env(
        db.pool().clone(),
        EnvelopeCipher::derive(&master, "coder-provisioner", 1),
        &url,
        "v2.37.3",
    )
    .unwrap();
    let org = coder_client::CoderOrganizationId::new(org);
    let key = backend.ensure(&client, &org).await.unwrap();
    assert_eq!(key, backend.ensure(&client, &org).await.unwrap());
    let docker = std::env::var("OPENFLOWS_DOCKER_BINARY").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/Applications/Docker.app/Contents/Resources/bin/docker".into()
        } else {
            "docker".into()
        }
    });
    let inspected = tokio::process::Command::new(&docker)
        .args(["inspect", &key])
        .output()
        .await
        .unwrap();
    assert!(inspected.status.success());
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let original_id = inspected[0]["Id"].clone();
    assert!(inspected[0]["Config"]["Env"]
        .as_array()
        .unwrap()
        .iter()
        .all(|value| !value
            .as_str()
            .unwrap()
            .starts_with("CODER_PROVISIONER_DAEMON_KEY=")));
    // Simulate a stopped container and a new Manager instance. Recovery must
    // retain both container identity and credential identity.
    assert!(tokio::process::Command::new(&docker)
        .args(["stop", &key])
        .output()
        .await
        .unwrap()
        .status
        .success());
    let restarted_backend = DockerProvisioner::from_env(
        db.pool().clone(),
        EnvelopeCipher::derive(&master, "coder-provisioner", 1),
        &url,
        "v2.37.3",
    )
    .unwrap();
    assert_eq!(key, restarted_backend.ensure(&client, &org).await.unwrap());
    let inspected = tokio::process::Command::new(&docker)
        .args(["inspect", &key])
        .output()
        .await
        .unwrap();
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(original_id, inspected[0]["Id"]);
    assert_eq!(inspected[0]["State"]["Running"], true);
    // Simulate infrastructure loss: recreate the container using the durable
    // credential, without creating another upstream key.
    assert!(tokio::process::Command::new(&docker)
        .args(["rm", "-f", &key])
        .output()
        .await
        .unwrap()
        .status
        .success());
    assert_eq!(key, restarted_backend.ensure(&client, &org).await.unwrap());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let daemons = client.list_provisioner_daemons(&org).await.unwrap();
        if daemons.iter().any(|d| {
            d.key_name.as_deref() == Some(&key)
                && matches!(d.status.as_deref(), Some("idle" | "busy"))
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "managed daemon did not report online"
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    println!("Managed provisioner is online; retries, stopped-container recovery and deleted-container recovery reused its key.");
}
