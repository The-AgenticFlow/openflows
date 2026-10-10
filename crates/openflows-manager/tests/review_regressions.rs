//! Regression tests for the WP-02 senior review. All database state is isolated.
mod common;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use common::*;
use openflows_manager::{
    auth::device::Poll,
    dto::{MembershipRole as Role, MembershipStatus as Status},
    id::{OrganizationId, UserId},
    rate_limit::LimitScope,
    server::ManagerServices,
};
use std::sync::Arc;
use tower::ServiceExt;

async fn context() -> (TestDb, ManagerServices, axum::Router) {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let (services, router) = build_context(db.pool().clone(), github, "http://127.0.0.1:3002");
    (db, services, router)
}

async fn user(s: &ManagerServices, id: i64) -> UserId {
    s.users
        .find_or_create_user(s.db.pool(), "github", &id.to_string(), "user", "User")
        .await
        .unwrap()
        .0
}

async fn org(s: &ManagerServices, owner: UserId) -> OrganizationId {
    s.orgs_service
        .create_org(owner, "regression", "Regression", "create", "request")
        .await
        .unwrap()
        .resource_id
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn wire_credentials_validate_and_suspended_users_cannot_refresh_or_login() {
    let (_db, s, _) = context().await;
    let u = user(&s, 1).await;
    let creds = s.sessions.create_cli_session(u).await.unwrap();
    assert!(s
        .session_manager
        .validate_cli(&creds.access_token)
        .await
        .unwrap()
        .is_some());
    sqlx::query("UPDATE users SET status='suspended' WHERE id=$1")
        .bind(u.0)
        .execute(s.db.pool())
        .await
        .unwrap();
    assert!(s
        .session_manager
        .validate_cli(&creds.access_token)
        .await
        .unwrap()
        .is_none());
    assert!(s
        .session_manager
        .refresh_cli(&creds.refresh_token)
        .await
        .unwrap()
        .is_none());
    assert!(s.sessions.create_browser_session(u).await.is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_refresh_reuse_revokes_the_winning_credentials() {
    let (_db, s, _) = context().await;
    let u = user(&s, 1).await;
    let creds = s.sessions.create_cli_session(u).await.unwrap();
    let (a, b) = tokio::join!(
        s.session_manager.refresh_cli(&creds.refresh_token),
        s.session_manager.refresh_cli(&creds.refresh_token)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.is_some(), b.is_some());
    let winner = a.or(b).unwrap();
    assert_eq!(winner.family_id, creds.family_id);
    assert!(s
        .session_manager
        .validate_cli(&winner.access_token)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn device_expiry_poll_interval_and_concurrent_delivery() {
    let (_db, s, _) = context().await;
    let u = user(&s, 1).await;
    let device = s.device.start().await.unwrap();
    assert!(matches!(
        s.device.poll(&device.device_secret).await.unwrap(),
        Poll::Pending
    ));
    assert!(s.device.poll(&device.device_secret).await.is_err());
    s.device.approve(&device.user_code, u).await.unwrap();
    sqlx::query("UPDATE cli_login_requests SET expires_at=now()-interval '1 second'")
        .execute(s.db.pool())
        .await
        .unwrap();
    assert!(matches!(
        s.device.poll(&device.device_secret).await.unwrap(),
        Poll::Expired
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(s.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let device = s.device.start().await.unwrap();
    s.device.approve(&device.user_code, u).await.unwrap();
    let (a, b) = tokio::join!(
        s.device.poll(&device.device_secret),
        s.device.poll(&device.device_secret)
    );
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Poll::Credentials(_)))
            .count(),
        1
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(s.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rate_limit_counts_shared_windows_without_storing_raw_credentials() {
    let (_db, s, _) = context().await;
    for _ in 0..3 {
        assert!(s
            .rate_limiter
            .allow("sensitive-key", LimitScope::Verify, 3)
            .await
            .unwrap());
    }
    assert!(!s
        .rate_limiter
        .allow("sensitive-key", LimitScope::Verify, 3)
        .await
        .unwrap());
    assert!(s
        .rate_limiter
        .allow("sensitive-key", LimitScope::Poll, 3)
        .await
        .unwrap());
    let raw: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rate_limit_ledger WHERE bucket_key='sensitive-key'",
    )
    .fetch_one(s.db.pool())
    .await
    .unwrap();
    assert_eq!(raw, 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_first_login_maps_to_one_identity() {
    let (_db, s, _) = context().await;
    let (a, b) = tokio::join!(
        s.users
            .find_or_create_user(s.db.pool(), "github", "777", "alice", "Alice"),
        s.users
            .find_or_create_user(s.db.pool(), "github", "777", "alice", "Alice")
    );
    assert_eq!(a.unwrap().0, b.unwrap().0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(s.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn owner_can_be_developer_and_concurrent_demotions_leave_an_admin() {
    let (_db, s, _) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let oid = org(&s, alice).await;
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role,status) VALUES($1,$2,'admin','active')")
        .bind(oid.0).bind(bob.0).execute(s.db.pool()).await.unwrap();
    let (a, b) = tokio::join!(
        s.orgs_service
            .update_member(alice, oid, alice, Some(Role::Developer), None, "a"),
        s.orgs_service
            .update_member(bob, oid, bob, Some(Role::Developer), None, "b")
    );
    assert_eq!([a, b].iter().filter(|r| r.is_ok()).count(), 1);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM memberships WHERE organization_id=$1 AND role='admin' AND status='active'")
        .bind(oid.0).fetch_one(s.db.pool()).await.unwrap();
    assert_eq!(count, 1);
    // Transfer never changes membership roles.
    let before: String =
        sqlx::query_scalar("SELECT role FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(oid.0)
            .bind(bob.0)
            .fetch_one(s.db.pool())
            .await
            .unwrap();
    s.orgs_service
        .transfer_ownership(alice, oid, bob, true, "transfer")
        .await
        .unwrap();
    let after: String =
        sqlx::query_scalar("SELECT role FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(oid.0)
            .bind(bob.0)
            .fetch_one(s.db.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    assert!(s
        .orgs_service
        .transfer_ownership(alice, oid, alice, true, "stale")
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn audit_failure_rolls_back_member_mutation() {
    let (_db, s, _) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let oid = org(&s, alice).await;
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role,status) VALUES($1,$2,'developer','active')")
        .bind(oid.0).bind(bob.0).execute(s.db.pool()).await.unwrap();
    sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT reject_review_test CHECK(action <> 'member.update')")
        .execute(s.db.pool()).await.unwrap();
    assert!(s
        .orgs_service
        .update_member(alice, oid, bob, Some(Role::Viewer), None, "audit-fail")
        .await
        .is_err());
    let role: String =
        sqlx::query_scalar("SELECT role FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(oid.0)
            .bind(bob.0)
            .fetch_one(s.db.pool())
            .await
            .unwrap();
    assert_eq!(role, "developer");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn org_lifecycle_and_invites_cannot_reactivate_suspended_members() {
    let (_db, s, _) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let oid = org(&s, alice).await;
    let (_, token) = s
        .orgs_service
        .create_invitation(alice, oid, "bob", Role::Viewer, s.github.as_ref(), "invite")
        .await
        .unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role,status) VALUES($1,$2,'admin','suspended')")
        .bind(oid.0).bind(bob.0).execute(s.db.pool()).await.unwrap();
    assert!(s
        .orgs_service
        .accept_invitation(bob, 2, &token, "accept")
        .await
        .is_err());
    sqlx::query("UPDATE organizations SET status='suspended' WHERE id=$1")
        .bind(oid.0)
        .execute(s.db.pool())
        .await
        .unwrap();
    assert!(s
        .orgs_service
        .update_org(alice, oid, Some("new"), "suspended")
        .await
        .is_err());
    assert!(s
        .orgs_service
        .accept_invitation(bob, 2, &token, "accept")
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn header_idempotency_is_required_and_replays_original_operation() {
    let (_db, s, router) = context().await;
    let u = user(&s, 1).await;
    let creds = s.sessions.create_cli_session(u).await.unwrap();
    let make = |key: Option<&str>, name: &str| {
        let mut r = Request::builder()
            .method("POST")
            .uri("/api/v1/organizations")
            .header("authorization", format!("Bearer {}", creds.access_token))
            .header("content-type", "application/json");
        if let Some(k) = key {
            r = r.header("idempotency-key", k);
        }
        r.body(Body::from(
            serde_json::json!({"slug":"http-org","display_name":name}).to_string(),
        ))
        .unwrap()
    };
    assert_eq!(
        router
            .clone()
            .oneshot(make(None, "One"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let a = router
        .clone()
        .oneshot(make(Some("same"), "One"))
        .await
        .unwrap();
    let b = router
        .clone()
        .oneshot(make(Some("same"), "One"))
        .await
        .unwrap();
    assert_eq!(a.status(), StatusCode::ACCEPTED);
    assert_eq!(b.status(), StatusCode::ACCEPTED);
    assert_eq!(
        to_bytes(a.into_body(), 65536).await.unwrap(),
        to_bytes(b.into_body(), 65536).await.unwrap()
    );
    assert_eq!(
        router
            .oneshot(make(Some("same"), "Other"))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn invitation_page_submits_token_and_form_csrf_without_custom_headers() {
    let (_db, s, router) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let oid = org(&s, alice).await;
    let (_, token) = s
        .orgs_service
        .create_invitation(alice, oid, "bob", Role::Viewer, s.github.as_ref(), "invite")
        .await
        .unwrap();
    let session = s.sessions.create_browser_session(bob).await.unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/invitations/accept?token={token}"))
                .header("cookie", format!("of_session={}", session.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let html = String::from_utf8(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains(&format!("name=\"token\" value=\"{token}\"")));
    let csrf = openflows_manager::auth::csrf::for_session(&session.access_token);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/invitations/accept")
                .header(
                    "cookie",
                    format!("of_session={}; of_csrf={csrf}", session.access_token),
                )
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("token={token}&_csrf={csrf}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let member = s.orgs_service.list_orgs(bob).await.unwrap();
    assert_eq!(member[0].status, Status::Active);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn invitation_retries_disclose_url_once_and_delete_retries_converge() {
    let (_db, s, _) = context().await;
    let alice = user(&s, 1).await;
    let oid = org(&s, alice).await;
    let a = s
        .orgs_service
        .create_invitation_idempotent(
            alice,
            oid,
            "bob",
            Role::Viewer,
            s.github.as_ref(),
            "invite-key",
            "r",
        )
        .await
        .unwrap();
    let unavailable = FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    });
    unavailable.users_by_login.lock().unwrap().clear();
    let b = s
        .orgs_service
        .create_invitation_idempotent(
            alice,
            oid,
            "bob",
            Role::Viewer,
            &unavailable,
            "invite-key",
            "r",
        )
        .await
        .unwrap();
    assert_eq!(a.0, b.0);
    assert!(a.1.is_some());
    assert!(b.1.is_none());
    assert!(s
        .orgs_service
        .create_invitation_idempotent(
            alice,
            oid,
            "bob",
            Role::Admin,
            s.github.as_ref(),
            "invite-key",
            "r"
        )
        .await
        .is_err());
    sqlx::query(
        "UPDATE memberships SET status='suspended' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(oid.0)
    .bind(alice.0)
    .execute(s.db.pool())
    .await
    .unwrap();
    assert!(s
        .orgs_service
        .create_invitation_idempotent(
            alice,
            oid,
            "bob",
            Role::Viewer,
            &unavailable,
            "invite-key",
            "r",
        )
        .await
        .is_err());
    sqlx::query("UPDATE memberships SET status='active' WHERE organization_id=$1 AND user_id=$2")
        .bind(oid.0)
        .bind(alice.0)
        .execute(s.db.pool())
        .await
        .unwrap();
    let a = s
        .orgs_service
        .request_deletion_idempotent(alice, oid, true, "delete-key", "r")
        .await
        .unwrap();
    let b = s
        .orgs_service
        .request_deletion_idempotent(alice, oid, true, "delete-key", "r")
        .await
        .unwrap();
    assert_eq!(a, b);
    assert!(s
        .orgs_service
        .update_org(alice, oid, Some("No"), "r")
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn oauth_return_path_is_bound_encrypted_and_callback_sets_both_cookies() {
    let (_db, s, router) = context().await;
    let target = "/invitations/accept?token=private-invitation-marker";
    let (_, state) = s.login.start_with_next(target).await.unwrap();
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT encrypted_pkce_verifier FROM auth_transactions WHERE state_hash=$1",
    )
    .bind(openflows_manager::auth::crypto::hash_token(&state))
    .fetch_one(s.db.pool())
    .await
    .unwrap();
    assert!(!ciphertext
        .windows(25)
        .any(|w| w == b"private-invitation-marker"));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/github/callback?state={state}&code=test&next=%2Fapi%2Fv1%2Fme"
                ))
                .header("cookie", format!("of_login_tx={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()["location"], target);
    let cookies: Vec<_> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert!(cookies.iter().any(|c| c.starts_with("of_session=")));
    assert!(cookies.iter().any(|c| c.starts_with("of_login_tx=;")));
    let replay = router
        .oneshot(
            Request::builder()
                .uri(format!("/auth/github/callback?state={state}&code=test"))
                .header("cookie", format!("of_login_tx={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn device_delivery_and_refresh_preserve_authentication_time_and_family_limit() {
    let (_db, s, _) = context().await;
    let u = user(&s, 1).await;
    let original = chrono::Utc::now() - chrono::Duration::minutes(11);
    let d = s.device.start().await.unwrap();
    s.device
        .approve_authenticated(&d.user_code, u, original)
        .await
        .unwrap();
    let Poll::Credentials(creds) = s.device.poll(&d.device_secret).await.unwrap() else {
        panic!("credentials")
    };
    assert_eq!(
        s.session_manager
            .has_recent_auth(creds.session_id)
            .await
            .unwrap(),
        Some(false)
    );
    sqlx::query(
        "UPDATE sessions SET created_at=now()-interval '30 days'+interval '1 minute' WHERE id=$1",
    )
    .bind(creds.session_id.0)
    .execute(s.db.pool())
    .await
    .unwrap();
    let rotated = s
        .session_manager
        .refresh_cli(&creds.refresh_token)
        .await
        .unwrap()
        .unwrap();
    assert!(rotated.access_expires_at <= chrono::Utc::now() + chrono::Duration::minutes(1));
    assert_eq!(
        s.session_manager
            .has_recent_auth(creds.session_id)
            .await
            .unwrap(),
        Some(false)
    );
    let browser = s.sessions.create_browser_session(u).await.unwrap();
    sqlx::query("UPDATE sessions SET created_at=now()-interval '13 hours' WHERE id=$1")
        .bind(browser.session_id.0)
        .execute(s.db.pool())
        .await
        .unwrap();
    assert!(s
        .session_manager
        .validate_browser(&browser.access_token)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_ownership_transfers_revalidate_the_current_owner() {
    let (_db, s, _) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let carol = user(&s, 3).await;
    let oid = org(&s, alice).await;
    for u in [bob, carol] {
        sqlx::query("INSERT INTO memberships(organization_id,user_id,role,status) VALUES($1,$2,'viewer','active')")
            .bind(oid.0).bind(u.0).execute(s.db.pool()).await.unwrap();
    }
    let (a, b) = tokio::join!(
        s.orgs_service
            .transfer_ownership(alice, oid, bob, true, "a"),
        s.orgs_service
            .transfer_ownership(alice, oid, carol, true, "b")
    );
    assert_eq!([a, b].iter().filter(|r| r.is_ok()).count(), 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn member_and_org_pages_return_usable_cursors_without_cross_org_results() {
    let (_db, s, router) = context().await;
    let alice = user(&s, 1).await;
    let bob = user(&s, 2).await;
    let oid = org(&s, alice).await;
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role,status) VALUES($1,$2,'viewer','active')")
        .bind(oid.0).bind(bob.0).execute(s.db.pool()).await.unwrap();
    s.orgs_service
        .create_org(alice, "second", "Second", "two", "r")
        .await
        .unwrap();
    let token = s
        .sessions
        .create_cli_session(alice)
        .await
        .unwrap()
        .access_token;
    for endpoint in [
        "/api/v1/organizations".to_string(),
        format!("/api/v1/organizations/{oid}/members"),
    ] {
        let request = |path: String| {
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };
        let first = router
            .clone()
            .oneshot(request(format!("{endpoint}?limit=1")))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first: serde_json::Value =
            serde_json::from_slice(&to_bytes(first.into_body(), 65536).await.unwrap()).unwrap();
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = router
            .clone()
            .oneshot(request(format!("{endpoint}?limit=1&after={cursor}")))
            .await
            .unwrap();
        let second: serde_json::Value =
            serde_json::from_slice(&to_bytes(second.into_body(), 65536).await.unwrap()).unwrap();
        assert_ne!(first["items"][0], second["items"][0]);
        assert!(second["next_cursor"].is_null());
    }
}

#[test]
fn public_origin_and_secret_debug_output_are_safe() {
    let mut config = test_auth_config("http://attacker.example");
    assert!(config.validate_origin().is_err());
    config.public_url = "https://example.com".into();
    assert!(config.validate_origin().is_err()); // insecure cookies on non-loopback
    config.cookie_secure = true;
    assert!(config.validate_origin().is_ok());
    config.public_url = "https://example.com/path".into();
    assert!(config.validate_origin().is_err());
    let token = openflows_manager::auth::github::UserToken {
        access_token: "private-access".into(),
        refresh_token: Some("private-refresh".into()),
        expires_in: None,
    };
    let debug = format!("{token:?}");
    assert!(!debug.contains("private-"));
    assert!(!format!("{:?}", openflows_manager::config::MasterKey([42; 32])).contains("42"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn start_limits_are_independent_and_counts_never_decrease() {
    let (_db, s, _) = context().await;
    let bucket = "limit-regression";
    let hashed = openflows_manager::auth::crypto::hash_token(bucket);
    // Seed every nearby minute so crossing a window cannot make this flaky.
    sqlx::query(
        "INSERT INTO rate_limit_ledger (bucket_key,scope,window_start,count)
        SELECT $1,'start',date_trunc('minute',clock_timestamp()) + n * interval '1 minute',30
        FROM generate_series(-1,1) n",
    )
    .bind(&hashed)
    .execute(s.db.pool())
    .await
    .unwrap();
    assert!(s
        .rate_limiter
        .allow(bucket, LimitScope::DeviceStart, 20)
        .await
        .unwrap());
    assert!(!s
        .rate_limiter
        .allow(bucket, LimitScope::GithubStart, 20)
        .await
        .unwrap());
    assert!(!s
        .rate_limiter
        .allow(bucket, LimitScope::GithubStart, 30)
        .await
        .unwrap());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn refresh_response_reports_the_remaining_family_lifetime() {
    let (_db, s, router) = context().await;
    let u = user(&s, 1).await;
    let creds = s.sessions.create_cli_session(u).await.unwrap();
    sqlx::query(
        "UPDATE sessions SET created_at=now()-interval '30 days'+interval '1 minute' WHERE id=$1",
    )
    .bind(creds.session_id.0)
    .execute(s.db.pool())
    .await
    .unwrap();
    let response = router
        .oneshot(
            Request::post("/api/v1/auth/refresh")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"refresh_token":creds.refresh_token}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!((1..=60).contains(&body["expires_in"].as_i64().unwrap()));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn organization_requests_with_different_keys_do_not_deadlock() {
    let (_db, s, _) = context().await;
    let owner = user(&s, 1).await;
    let oid = org(&s, owner).await;
    // Delay after the foreign-key check so the old insert-before-lock order
    // reliably lets both requests acquire KEY SHARE before upgrading locks.
    sqlx::query(
        "CREATE FUNCTION delay_idempotency_insert() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN PERFORM pg_sleep(0.1); RETURN NEW; END $$",
    )
    .execute(s.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER zz_delay_idempotency AFTER INSERT ON idempotency_keys
        FOR EACH ROW EXECUTE FUNCTION delay_idempotency_insert()",
    )
    .execute(s.db.pool())
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        s.orgs_service.create_invitation_idempotent(
            owner,
            oid,
            "bob",
            Role::Viewer,
            s.github.as_ref(),
            "a",
            "r"
        ),
        s.orgs_service.create_invitation_idempotent(
            owner,
            oid,
            "alice",
            Role::Viewer,
            s.github.as_ref(),
            "b",
            "r"
        ),
    );
    assert!(a.is_ok(), "{a:?}");
    assert!(b.is_ok(), "{b:?}");
    let (a, b) = tokio::join!(
        s.orgs_service
            .request_deletion_idempotent(owner, oid, true, "a", "r"),
        s.orgs_service
            .request_deletion_idempotent(owner, oid, true, "b", "r"),
    );
    assert_eq!(a.unwrap(), b.unwrap());
}
