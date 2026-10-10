//! HTTP-level integration tests for human authentication and session lifecycle
//! (WP-02). These exercise the real axum handlers, cookies, CSRF, redirects,
//! and PostgreSQL-backed sessions against a fixture GitHub adapter.
//!
//! They require a live PostgreSQL (see `common/mod.rs`); they are `#[ignore]`d
//! and run by `scripts/run-integration-tests.sh` in CI.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::*;
use openflows_manager::auth::github::GithubAuth;
use std::sync::Arc;
use tower::ServiceExt;

/// A thin HTTP test harness around the real router.
struct App {
    router: axum::Router<()>,
}

impl App {
    async fn request(&self, req: Request<Body>) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("route responds")
    }

    fn get(&self, path: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }

    fn post_json(&self, path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
}

fn extract_header_value(resp: &axum::response::Response, name: &str) -> Option<String> {
    resp.headers()
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .last()
        .cloned()
}

fn extract_cookie_value(set_cookie: &str, name: &str) -> Option<String> {
    set_cookie.split(';').next().and_then(|c| {
        let mut parts = c.trim().splitn(2, '=');
        if parts.next() == Some(name) {
            parts.next().map(|v| v.to_string())
        } else {
            None
        }
    })
}

/// Drive a full browser OAuth login and return the session cookie header value.
async fn login(app: &App, github: &Arc<dyn GithubAuth>) -> String {
    let _ = github;
    // 1. Start login: GET /auth/github/start -> Location + tx cookie.
    let start_resp = app.request(app.get("/auth/github/start")).await;
    assert_eq!(start_resp.status(), StatusCode::TEMPORARY_REDIRECT);
    let location = start_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let tx_cookie =
        extract_header_value(&start_resp, header::SET_COOKIE.as_str()).expect("tx cookie set");

    // Extract the raw state from the authorize URL.
    let state = location
        .split('&')
        .find(|p| p.starts_with("state="))
        .map(|p| p.trim_start_matches("state=").to_string())
        .expect("state in authorize url");

    // 2. Complete login: GET /auth/github/callback?state&code with tx cookie.
    let callback_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/auth/github/callback?state={state}&code=authcode&next=%2F"
        ))
        .header(header::COOKIE, tx_cookie)
        .body(Body::empty())
        .unwrap();
    let callback_resp = app.request(callback_req).await;
    assert_eq!(callback_resp.status(), StatusCode::TEMPORARY_REDIRECT);
    let session_cookie = callback_resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("of_session="))
        .expect("session cookie set")
        .to_string();
    // The session cookie should be the of_session cookie.
    extract_cookie_value(&session_cookie, "of_session").expect("of_session")
}

fn with_session(mut req: Request<Body>, session: &str) -> Request<Body> {
    req.headers_mut().insert(
        header::COOKIE,
        format!("of_session={session}").parse().unwrap(),
    );
    req
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn oauth_wrong_state_and_cookie_mismatch_are_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };

    // Start a login to get a valid transaction cookie/state.
    let start_resp = app.request(app.get("/auth/github/start")).await;
    let location = start_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let tx_cookie = extract_header_value(&start_resp, header::SET_COOKIE.as_str()).unwrap();
    let real_state = location
        .split('&')
        .find(|p| p.starts_with("state="))
        .map(|p| p.trim_start_matches("state=").to_string())
        .unwrap();

    // Wrong state (but correct cookie) must fail.
    let wrong_state = Request::builder()
        .method("GET")
        .uri("/auth/github/callback?state=WRONG&code=code")
        .header(header::COOKIE, &tx_cookie)
        .body(Body::empty())
        .unwrap();
    let resp = app.request(wrong_state).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "wrong state rejected"
    );

    // Correct state but MISSING cookie (no transaction-cookie binding) fails.
    let no_cookie = Request::builder()
        .method("GET")
        .uri(format!(
            "/auth/github/callback?state={real_state}&code=code"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.request(no_cookie).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "cookie mismatch rejected"
    );

    // Correct state + cookie succeeds.
    let ok = Request::builder()
        .method("GET")
        .uri(format!(
            "/auth/github/callback?state={real_state}&code=code&next=%2F"
        ))
        .header(header::COOKIE, &tx_cookie)
        .body(Body::empty())
        .unwrap();
    let resp = app.request(ok).await;
    assert_eq!(
        resp.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "login succeeds"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn oauth_callback_replay_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };

    let start_resp = app.request(app.get("/auth/github/start")).await;
    let tx_cookie = extract_header_value(&start_resp, header::SET_COOKIE.as_str()).unwrap();
    let location = start_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let state = location
        .split('&')
        .find(|p| p.starts_with("state="))
        .map(|p| p.trim_start_matches("state=").to_string())
        .unwrap();

    let cb = |state: &str| {
        Request::builder()
            .method("GET")
            .uri(format!("/auth/github/callback?state={state}&code=code"))
            .header(header::COOKIE, tx_cookie.clone())
            .body(Body::empty())
            .unwrap()
    };

    let first = app.request(cb(&state)).await;
    assert_eq!(first.status(), StatusCode::TEMPORARY_REDIRECT);
    // Replay with the same state must fail (transaction consumed once).
    let second = app.request(cb(&state)).await;
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED, "replay rejected");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn unsafe_redirect_targets_are_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };
    let start_resp = app.request(app.get("/auth/github/start")).await;
    let tx_cookie = extract_header_value(&start_resp, header::SET_COOKIE.as_str()).unwrap();
    let location = start_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let state = location
        .split('&')
        .find(|p| p.starts_with("state="))
        .map(|p| p.trim_start_matches("state=").to_string())
        .unwrap();

    // An absolute external URL in `next` must NOT produce an open redirect.
    let evil = Request::builder()
        .method("GET")
        .uri(format!(
            "/auth/github/callback?state={state}&code=code&next=https%3A%2F%2Fevil.example%2Fsteal"
        ))
        .header(header::COOKIE, tx_cookie)
        .body(Body::empty())
        .unwrap();
    let resp = app.request(evil).await;
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    assert!(
        location.is_none() || !location.unwrap().starts_with("https://evil.example"),
        "must not redirect to an external URL"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn device_approval_cannot_occur_via_get_and_token_is_single_use() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };

    // A user logs in via the browser.
    let session = login(&app, &github).await;

    // Start a CLI device request.
    let start = app
        .request(app.post_json("/api/v1/auth/cli/start", serde_json::json!({})))
        .await;
    assert_eq!(start.status(), StatusCode::OK);
    let start_body: serde_json::Value = axum::body::to_bytes(start.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    let device_code = start_body["device_code"].as_str().unwrap().to_string();
    let user_code = start_body["user_code"].as_str().unwrap().to_string();

    // GET the verify page must NOT approve.
    let verify_get = app.request(app.get("/auth/cli/verify")).await;
    assert_ne!(verify_get.status(), StatusCode::FORBIDDEN);
    // The request is still pending.
    let poll_pending = app
        .request(app.post_json(
            "/api/v1/auth/cli/token",
            serde_json::json!({ "device_code": device_code }),
        ))
        .await;
    assert_eq!(poll_pending.status(), StatusCode::OK);
    let pending_body: serde_json::Value = axum::body::to_bytes(poll_pending.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    assert_eq!(pending_body["status"], "authorization_pending");

    // Approve via POST with a valid browser session and CSRF.
    let verify_page = app
        .request(with_session(
            app.get("/auth/cli/verify?code=ABCD-EFGH"),
            &session,
        ))
        .await;
    let csrf_cookie = extract_header_value(&verify_page, header::SET_COOKIE.as_str())
        .map(|c| extract_cookie_value(&c, "of_csrf").unwrap())
        .unwrap();
    let approve = Request::builder()
        .method("POST")
        .uri("/auth/cli/approve")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(
            header::COOKIE,
            format!("of_session={session}; of_csrf={csrf_cookie}"),
        )
        .header("x-csrf-token", csrf_cookie.clone())
        .body(Body::from(format!("code={user_code}&_csrf={csrf_cookie}")))
        .unwrap();
    let approve_resp = app.request(approve).await;
    assert_eq!(approve_resp.status(), StatusCode::OK, "approve succeeds");

    // Simulate the next permitted poll without sleeping.
    sqlx::query("UPDATE cli_login_requests SET last_poll_at=now()-interval '6 seconds'")
        .execute(db.pool())
        .await
        .unwrap();
    // Poll delivers credentials exactly once.
    let token_req = app.post_json(
        "/api/v1/auth/cli/token",
        serde_json::json!({ "device_code": device_code }),
    );
    let token_resp = app.request(token_req).await;
    assert_eq!(token_resp.status(), StatusCode::OK);
    let body: serde_json::Value = axum::body::to_bytes(token_resp.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    assert_eq!(body["status"], "authorized");
    assert!(body["access_token"].is_string());
    assert!(body["refresh_token"].is_string());

    // Second poll must NOT deliver again (single-use).
    let second = app
        .request(app.post_json(
            "/api/v1/auth/cli/token",
            serde_json::json!({ "device_code": device_code }),
        ))
        .await;
    let second_body: serde_json::Value = axum::body::to_bytes(second.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    assert_eq!(
        second_body["status"], "expired_token",
        "single-use delivery"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn device_approve_requires_csrf() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };
    let session = login(&app, &github).await;
    let start = app
        .request(app.post_json("/api/v1/auth/cli/start", serde_json::json!({})))
        .await;
    let start_body: serde_json::Value = axum::body::to_bytes(start.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    let user_code = start_body["user_code"].as_str().unwrap().to_string();

    // Approve with a valid session but NO CSRF header/cookie must fail.
    let no_csrf = Request::builder()
        .method("POST")
        .uri("/auth/cli/approve")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("of_session={session}"))
        .body(Body::from(format!("code={user_code}")))
        .unwrap();
    let resp = app.request(no_csrf).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "CSRF required");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn refresh_rotation_and_reuse_detection_revoke_family() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };

    // Get CLI credentials via the device flow.
    let session = login(&app, &github).await;
    let start = app
        .request(app.post_json("/api/v1/auth/cli/start", serde_json::json!({})))
        .await;
    let start_body: serde_json::Value = axum::body::to_bytes(start.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    let device_code = start_body["device_code"].as_str().unwrap().to_string();
    let user_code = start_body["user_code"].as_str().unwrap().to_string();

    let csrf_cookie = extract_header_value(
        &app.request(with_session(
            app.get("/auth/cli/verify?code=ABCD-EFGH"),
            &session,
        ))
        .await,
        header::SET_COOKIE.as_str(),
    )
    .map(|c| extract_cookie_value(&c, "of_csrf").unwrap())
    .unwrap();
    let approve = Request::builder()
        .method("POST")
        .uri("/auth/cli/approve")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(
            header::COOKIE,
            format!("of_session={session}; of_csrf={csrf_cookie}"),
        )
        .header("x-csrf-token", &csrf_cookie)
        .body(Body::from(format!("code={user_code}&_csrf={csrf_cookie}")))
        .unwrap();
    app.request(approve).await;
    let token_body: serde_json::Value = axum::body::to_bytes(
        app.request(app.post_json(
            "/api/v1/auth/cli/token",
            serde_json::json!({ "device_code": device_code }),
        ))
        .await
        .into_body(),
        1024 * 64,
    )
    .await
    .unwrap()
    .iter()
    .copied()
    .collect::<Vec<_>>()
    .into_iter()
    .map(|b| b as char)
    .collect::<String>()
    .parse()
    .unwrap();
    let refresh_1 = token_body["refresh_token"].as_str().unwrap().to_string();

    // Refresh with refresh_1 -> new pair.
    let refresh_resp = app
        .request(app.post_json(
            "/api/v1/auth/refresh",
            serde_json::json!({ "refresh_token": refresh_1 }),
        ))
        .await;
    assert_eq!(refresh_resp.status(), StatusCode::OK);
    let refresh_body: serde_json::Value = axum::body::to_bytes(refresh_resp.into_body(), 1024 * 64)
        .await
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect::<String>()
        .parse()
        .unwrap();
    let refresh_2 = refresh_body["refresh_token"].as_str().unwrap().to_string();

    // Reusing the consumed refresh_1 must fail (401) and revoke the family.
    let reuse = app
        .request(app.post_json(
            "/api/v1/auth/refresh",
            serde_json::json!({ "refresh_token": refresh_1 }),
        ))
        .await;
    assert_eq!(
        reuse.status(),
        StatusCode::UNAUTHORIZED,
        "reused refresh rejected"
    );

    // The rotated refresh_2 must now be rejected too (family revoked).
    let after = app
        .request(app.post_json(
            "/api/v1/auth/refresh",
            serde_json::json!({ "refresh_token": refresh_2 }),
        ))
        .await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED, "family revoked");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn logout_revokes_server_side() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 100,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let app = App {
        router: router_with(db.pool().clone(), github.clone(), "http://127.0.0.1:3002"),
    };
    let session = login(&app, &github).await;

    // /me works before logout.
    let me_before = app
        .request(with_session(app.get("/api/v1/me"), &session))
        .await;
    assert_eq!(me_before.status(), StatusCode::OK);

    // Logout (POST) requires a session-bound CSRF token.
    let mut request = with_session(
        app.post_json("/api/v1/auth/logout", serde_json::json!({})),
        &session,
    );
    let csrf = openflows_manager::auth::csrf::for_session(&session);
    request.headers_mut().insert(
        header::COOKIE,
        format!("of_session={session}; of_csrf={csrf}")
            .parse()
            .unwrap(),
    );
    request
        .headers_mut()
        .insert("x-csrf-token", csrf.parse().unwrap());
    let logout = app.request(request).await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    // /me must now be rejected.
    let me_after = app
        .request(with_session(app.get("/api/v1/me"), &session))
        .await;
    assert_eq!(
        me_after.status(),
        StatusCode::UNAUTHORIZED,
        "session revoked"
    );
}
