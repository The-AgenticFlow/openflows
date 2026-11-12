//! `/metrics` through the real router (#532): off unless enabled, and labelled
//! only by method, route template and status, so no request value leaks.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use tower::ServiceExt;

async fn call(app: &Router, method: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

// One test, because the switch is a process environment variable.
#[tokio::test]
async fn metrics_are_opt_in_and_carry_only_route_templates() {
    let app =
        openflows_manager::server::create_router(openflows_manager::server::AppState::for_tests());

    std::env::remove_var(openflows_manager::metrics::METRICS_ENV);
    let (status, body) = call(&app, "GET", "/metrics").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "metrics must be off by default"
    );
    assert!(!body.contains("openflows_manager_http_requests_total"));

    // A concrete id and a token-like query value must never appear in the output.
    let secret_like = "ghu_SECRETVALUE123";
    call(&app, "GET", "/health").await;
    call(
        &app,
        "GET",
        &format!("/api/v1/operations/op-4242?token={secret_like}"),
    )
    .await;
    let (credential_status, _) = call(&app, "POST", "/api/v1/local/github-credential").await;
    call(&app, "GET", "/no/such/path-7777").await;

    std::env::set_var(openflows_manager::metrics::METRICS_ENV, "true");
    let (status, body) = call(&app, "GET", "/metrics").await;
    std::env::remove_var(openflows_manager::metrics::METRICS_ENV);
    assert_eq!(status, StatusCode::OK);

    assert!(body.contains("openflows_manager_build_info{version="));
    assert!(body.contains(
        "openflows_manager_http_requests_total{method=\"GET\",route=\"/health\",status=\"200\"}"
    ));
    assert!(body.contains("route=\"/api/v1/operations/{id}\""), "{body}");
    // Token renewals are visible as the credential route's outcomes.
    assert!(body.contains(&format!(
        "openflows_manager_http_requests_total{{method=\"POST\",route=\"/api/v1/local/github-credential\",status=\"{}\"}} 1",
        credential_status.as_u16()
    )), "{body}");
    for leaked in ["op-4242", secret_like, "path-7777"] {
        assert!(
            !body.contains(leaked),
            "{leaked} leaked into metrics:\n{body}"
        );
    }
}
