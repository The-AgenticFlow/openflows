mod common;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn temporary_failures_keep_retrying_and_permanent_failures_stop() {
    let db = common::TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    let first = outbox::insert_in_tx(&mut tx, None, "retry.test", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let claim = outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
        .await
        .unwrap()
        .remove(0);
    let mut tx = db.pool().begin().await.unwrap();
    let second = outbox::insert_in_tx(&mut tx, None, "retry.test", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(outbox::fail_delivery(db.pool(), &claim, false)
        .await
        .unwrap());
    assert!(!outbox::fail_delivery(db.pool(), &claim, true)
        .await
        .unwrap());
    let next = outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(next.id, second);
    assert!(outbox::mark_delivered(db.pool(), &next).await.unwrap());
    sqlx::query("UPDATE outbox_events SET retry_at = now() - interval '1 second', attempts = 7 WHERE id = $1")
        .bind(first.0).execute(db.pool()).await.unwrap();
    let last = outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(last.attempts, 8);
    assert!(outbox::fail_delivery(db.pool(), &last, false)
        .await
        .unwrap());
    assert!(
        outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
            .await
            .unwrap()
            .is_empty()
    );
    let state: (bool, bool) = sqlx::query_as(
        "SELECT failed_at IS NOT NULL, delivered_at IS NULL FROM outbox_events WHERE id = $1",
    )
    .bind(first.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state, (false, true));
    let delay: bool = sqlx::query_scalar("SELECT retry_at > clock_timestamp() AND retry_at <= clock_timestamp() + interval '256 seconds' FROM outbox_events WHERE id = $1")
        .bind(first.0).fetch_one(db.pool()).await.unwrap();
    assert!(delay);
    // Simulate an extended outage, then recovery, without sleeping through
    // real backoff intervals. Attempt counts above eight must remain eligible.
    sqlx::query("UPDATE outbox_events SET retry_at = now() - interval '1 second', attempts = 100 WHERE id = $1")
        .bind(first.0).execute(db.pool()).await.unwrap();
    let recovered = outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(recovered.id, first);
    assert_eq!(recovered.attempts, 101);
    assert!(outbox::complete_operation_delivery(db.pool(), &recovered)
        .await
        .unwrap());

    let mut tx = db.pool().begin().await.unwrap();
    let permanent = outbox::insert_in_tx(&mut tx, None, "retry.test", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let claim = outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(claim.id, permanent);
    assert!(outbox::fail_delivery(db.pool(), &claim, true)
        .await
        .unwrap());
    assert!(
        outbox::claim_filtered(db.pool(), "worker", 1, Some(&["retry.test"]))
            .await
            .unwrap()
            .is_empty()
    );
    let terminal: bool = sqlx::query_scalar("SELECT failed_at IS NOT NULL AND retry_at IS NULL AND delivered_at IS NULL FROM outbox_events WHERE id = $1")
        .bind(permanent.0).fetch_one(db.pool()).await.unwrap();
    assert!(terminal);
}

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use openflows_manager::{
    outbox,
    server::{create_router, AppState, ManagerServices},
};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn repository_discovery_exchanges_app_jwt_for_installation_token() {
    use axum::{
        http::HeaderMap,
        routing::{get, post},
        Json, Router,
    };
    use openflows_manager::connections::{
        app_jwt::FixtureAppSigner, AppJwtClaims, AppSigner, GithubAppApi, RealGithubAppApi,
    };
    let router = Router::new()
        .route("/app/installations/42/access_tokens", post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(headers["authorization"], "Bearer fixture.jwt.token");
            assert_eq!(body["permissions"]["metadata"], "read");
            Json(serde_json::json!({"token": "installation-token", "expires_at": "2099-01-01T00:00:00Z"}))
        }))
        .route("/installation/repositories", get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer installation-token");
            Json(serde_json::json!({"repositories": [{"id": 7, "owner": {"login": "owner"}, "name": "repo", "full_name": "owner/repo", "private": true}]}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let jwt = FixtureAppSigner::new(AppJwtClaims {
        iss: 1,
        iat: 1,
        exp: 2,
    })
    .sign()
    .await
    .unwrap();
    let result = RealGithubAppApi::new(base)
        .installation_repositories(&jwt, 42, 1, 100)
        .await;
    server.abort();
    assert_eq!(result.unwrap()[0].id, 7);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn attempt_ciphertext_roundtrips_through_migrated_schema() {
    use openflows_manager::{
        connections::{ConnectionRepository, FlowType},
        id::OrganizationId,
    };
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let s = ManagerServices::from_db(openflows_manager::db::Db::from_pool(db.pool().clone()));
    let (user, _) = s
        .users
        .find_or_create_user(db.pool(), "github", "100", "tester", "Tester")
        .await
        .unwrap();
    let org: OrganizationId = s
        .orgs_service
        .create_org(user, "roundtrip", "Roundtrip", "key", "request")
        .await
        .unwrap()
        .resource_id;
    let id = uuid::Uuid::new_v4();
    let ciphertext = vec![0, 255, 42, 128];
    s.connections
        .create_attempt(
            id,
            org,
            user,
            FlowType::Both,
            Some("oauth"),
            Some("setup"),
            Some(&ciphertext),
            10,
        )
        .await
        .unwrap();
    let row = s.connections.attempt_by_id(id).await.unwrap().unwrap();
    assert_eq!(row.oauth_code_verifier_ref.as_ref(), Some(&ciphertext));
    let mut tx = db.pool().begin().await.unwrap();
    assert!(ConnectionRepository::consume_oauth_proof_in_tx(
        &mut tx,
        id,
        100,
        None,
        Some(&ciphertext)
    )
    .await
    .unwrap());
    tx.commit().await.unwrap();
    assert_eq!(
        s.connections
            .attempt_credentials(id)
            .await
            .unwrap()
            .unwrap()
            .0,
        Some(ciphertext)
    );
    let mut tx = db.pool().begin().await.unwrap();
    assert!(ConnectionRepository::consume_attempt_in_tx(&mut tx, id)
        .await
        .unwrap());
    tx.commit().await.unwrap();
    assert!(s
        .connections
        .attempt_credentials(id)
        .await
        .unwrap()
        .unwrap()
        .0
        .is_none());

    // Exercise the public disconnect path, then let the actual worker finish
    // the operation while unrelated organization events remain pending.
    let connection = openflows_manager::id::ConnectionId::new();
    let mut tx = db.pool().begin().await.unwrap();
    ConnectionRepository::insert_connection_in_tx(
        &mut tx,
        connection,
        org,
        1,
        10,
        100,
        "User",
        Some("tester"),
        user,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let operation = s
        .connections_service
        .disconnect(user, org, connection, "disconnect", "request")
        .await
        .unwrap();
    s.connection_worker.run_once().await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM operations WHERE id = $1")
        .bind(operation.0)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(state, "succeeded");
    assert!(!s.connections.reactivate_connection(1, 10, 1).await.unwrap());
    // Simulate a later explicit reconnect; replaying the previous DELETE must
    // return its old operation without disconnecting the new connection.
    sqlx::query("UPDATE github_connections SET status = 'active', access_revoked_reason = NULL WHERE id = $1")
        .bind(connection.0).execute(db.pool()).await.unwrap();
    assert_eq!(
        s.connections_service
            .disconnect(user, org, connection, "disconnect", "request")
            .await
            .unwrap(),
        operation
    );
    assert_eq!(
        s.connections
            .get_connection_scoped(org, connection)
            .await
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn connection_worker_does_not_consume_other_workers_events() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    let unrelated = outbox::insert_in_tx(&mut tx, None, "organization.provision", None)
        .await
        .unwrap();
    let owned = outbox::insert_in_tx(&mut tx, None, "github.reconcile", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let claims = outbox::claim_filtered(db.pool(), "test", 10, Some(&["github.reconcile"]))
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id, owned);
    assert!(!outbox::is_leased_to(db.pool(), unrelated, "test")
        .await
        .unwrap());
    assert!(outbox::complete_operation_delivery(db.pool(), &claims[0])
        .await
        .unwrap());
    assert!(!outbox::complete_operation_delivery(db.pool(), &claims[0])
        .await
        .unwrap());
    assert_eq!(
        outbox::claim(db.pool(), "other", 10).await.unwrap()[0].id,
        unrelated
    );
}

#[tokio::test]
async fn webhook_limits_streamed_body_before_signature_or_database_work() {
    // A lazy pool makes any accidental database access fail, not silently pass.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let mut services = ManagerServices::from_db(openflows_manager::db::Db::from_pool(pool));
    services.webhook_secret = Some(b"test-secret".to_vec());
    services.webhooks.body_limit = 1024;
    let state = AppState::with_services(
        pocketflow_core::SharedStore::new_in_memory_with_tenant("test"),
        Arc::new(OkProbe),
        services,
        openflows_manager::config::local_test_config(),
    );
    let router = create_router(state);
    let chunks = futures::stream::iter(vec![
        Ok::<_, std::io::Error>(vec![b'x'; 600]),
        Ok(vec![b'y'; 600]),
    ]);
    let response = router
        .oneshot(
            Request::post("/api/v1/webhooks/github")
                .body(Body::from_stream(chunks))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn signed_jwt_debug_does_not_disclose_credentials() {
    use openflows_manager::connections::{app_jwt::FixtureAppSigner, AppJwtClaims, AppSigner};
    let jwt = FixtureAppSigner::new(AppJwtClaims {
        iss: 1,
        iat: 1,
        exp: 2,
    })
    .sign()
    .await
    .unwrap();
    assert!(!format!("{jwt:?}").contains(jwt.raw()));
}
