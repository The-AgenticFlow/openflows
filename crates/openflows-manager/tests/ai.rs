//! Integration tests for AI Provider, Model management, and OpenFlows Model Assignment Policy APIs (Issue #290).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::server::{create_router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn ai_provider_lifecycle_and_secret_redaction() {
    let app = create_router(AppState::for_tests());

    // 1. Create a provider with a sensitive API key
    let raw_secret = "sk-ant-api03-very-secret-token-12345";
    let create_body = json!({
        "provider_type": "anthropic",
        "name": "anthropic-main",
        "display_name": "Anthropic Claude",
        "api_key": raw_secret,
        "enabled": true
    });

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/ai/providers")
                .header("content-type", "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&bytes).to_string();

    // Verify secret is NOT in response body anywhere
    assert!(!body_str.contains(raw_secret));

    let created: Value = serde_json::from_str(&body_str).unwrap();
    let provider_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "anthropic-main");
    assert_eq!(created["type"], "anthropic");
    assert_eq!(created["has_api_key"], true);

    // 2. Fetch specific provider
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/ai/providers/{}", provider_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&bytes).to_string();
    assert!(!body_str.contains(raw_secret));
    let fetched: Value = serde_json::from_str(&body_str).unwrap();
    assert_eq!(fetched["id"], provider_id);
    assert_eq!(fetched["has_api_key"], true);

    // 3. List providers
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], provider_id);

    // 4. Update provider
    let update_body = json!({
        "display_name": "Anthropic Claude Enterprise",
        "enabled": false
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/ai/providers/{}", provider_id))
                .header("content-type", "application/json")
                .body(Body::from(update_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["display_name"], "Anthropic Claude Enterprise");
    assert_eq!(updated["enabled"], false);

    // 5. Delete provider
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/ai/providers/{}", provider_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    // 6. Verify provider is gone
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/ai/providers/{}", provider_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ai_model_lifecycle() {
    let app = create_router(AppState::for_tests());

    // 1. Create a model
    let create_body = json!({
        "ai_provider_id": "prov-1",
        "model": "claude-3-7-sonnet-20250219",
        "display_name": "Claude 3.7 Sonnet",
        "context_limit": 200000,
        "is_default": true,
        "enabled": true
    });

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/ai/models")
                .header("content-type", "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let model_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["model"], "claude-3-7-sonnet-20250219");
    assert_eq!(created["is_default"], true);

    // 2. List models
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], model_id);

    // 3. Update model
    let update_body = json!({
        "display_name": "Claude 3.7 Sonnet (Thinking)",
        "is_default": false
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/ai/models/{}", model_id))
                .header("content-type", "application/json")
                .body(Body::from(update_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["display_name"], "Claude 3.7 Sonnet (Thinking)");
    assert_eq!(updated["is_default"], false);

    // 4. Delete model
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/ai/models/{}", model_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    // 5. Verify list is empty
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.len(), 0);
}

#[tokio::test]
async fn model_policy_precedence_and_role_resolution() {
    let app = create_router(AppState::for_tests());

    // 1. First, create a coder default model in the mock backend
    let model_body = json!({
        "ai_provider_id": "prov-1",
        "model": "gpt-4o-mini",
        "display_name": "GPT-4o Mini",
        "context_limit": 128000,
        "is_default": true,
        "enabled": true
    });
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/ai/models")
                .header("content-type", "application/json")
                .body(Body::from(model_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    // With no policy set, role resolves to Coder default model
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=forge")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["role"], "forge");
    assert_eq!(resolved["model"], "gpt-4o-mini");
    assert_eq!(resolved["source"], "coder_default");

    // 2. Set Global Model Policy (default model: claude-3-5-sonnet, sentinel override: claude-3-opus)
    let global_policy = json!({
        "default_model": "claude-3-5-sonnet",
        "roles": {
            "sentinel": "claude-3-opus"
        }
    });

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/ai/model-policy")
                .header("content-type", "application/json")
                .body(Body::from(global_policy.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Check GET global policy
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let pol: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(pol["default_model"], "claude-3-5-sonnet");
    assert_eq!(pol["roles"]["sentinel"], "claude-3-opus");

    // Now forge resolves to global default model
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=forge")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["model"], "claude-3-5-sonnet");
    assert_eq!(resolved["source"], "default_policy");

    // sentinel resolves to global role override
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=sentinel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["model"], "claude-3-opus");
    assert_eq!(resolved["source"], "role_policy");

    // 3. Set Tenant-scoped Policy (default model: o3-mini, forge override: deepseek-r1)
    let tenant_policy = json!({
        "default_model": "o3-mini",
        "roles": {
            "forge": "deepseek-r1"
        }
    });

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/tenants/acme-corp/ai/model-policy")
                .header("content-type", "application/json")
                .body(Body::from(tenant_policy.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Verify tenant resolution:
    // a) forge in acme-corp has tenant role override -> "deepseek-r1" (source: role_policy)
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=forge&tenant=acme-corp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["model"], "deepseek-r1");
    assert_eq!(resolved["source"], "role_policy");

    // b) vessel in acme-corp has no role override -> tenant default_model "o3-mini" (source: default_policy)
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=vessel&tenant=acme-corp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["model"], "o3-mini");
    assert_eq!(resolved["source"], "default_policy");

    // c) forge in another tenant (beta-org) falls back to global default_model "claude-3-5-sonnet"
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/ai/model-policy/resolve?role=forge&tenant=beta-org")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resolved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resolved["model"], "claude-3-5-sonnet");
    assert_eq!(resolved["source"], "default_policy");
}
