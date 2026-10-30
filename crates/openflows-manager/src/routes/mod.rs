//! Versioned HTTP route composition for the OpenFlows Manager API.

use crate::server::AppState;
use axum::{
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Serialize;

pub mod audit;
pub mod auth;
pub mod coder_hooks;
pub mod coder_identity;
pub mod connections;
pub mod device;
pub mod health;
pub mod invitations;
pub mod organizations;
pub mod platform;
pub mod platform_releases;
pub mod runtime;
pub mod tenants;
pub mod test_ui;
pub mod webhooks;

pub fn router() -> Router<AppState> {
    // Keep platform-oriented probes at the root because orchestrators and load
    // balancers generally expect stable, version-independent health URLs.
    Router::new()
        .route("/", get(test_ui::index))
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        // Product/API routes are versioned from the start so future public
        // endpoints can evolve without moving operational probes.
        .nest("/api/v1", api_v1_router())
        // Browser pages are served outside the versioned API path.
        .route("/auth/login", get(auth::login_page))
        .route("/auth/github/start", get(auth::github_start))
        .route("/auth/github/callback", get(auth::github_callback))
        .route("/auth/coder/callback", get(coder_identity::callback))
        .route("/auth/cli/verify", get(device::device_verify_page))
        .route("/auth/cli/approve", post(device::device_approve))
        .route(
            "/invitations/accept",
            get(invitations::accept_page).post(invitations::accept),
        )
}

fn api_v1_router() -> Router<AppState> {
    Router::new()
        .route("/", get(api_index))
        .route("/me", get(auth::me))
        .route("/auth/coder", get(coder_identity::status))
        .route("/auth/coder/start", post(coder_identity::start))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/csrf", get(auth::csrf_token))
        .route("/invitations/accept", post(invitations::accept_json))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/cli/start", post(device::device_start))
        .route("/auth/cli/token", post(device::device_token))
        .route(
            "/organizations",
            get(organizations::list_orgs).post(organizations::create_org),
        )
        .route(
            "/organizations/{org}",
            get(organizations::get_org)
                .patch(organizations::update_org)
                .delete(organizations::delete_org),
        )
        .route(
            "/organizations/{org}/members",
            get(organizations::list_members),
        )
        .route(
            "/organizations/{org}/members/{user}",
            patch(organizations::update_member).delete(organizations::remove_member),
        )
        .route(
            "/organizations/{org}/invitations",
            get(organizations::list_invitations).post(organizations::create_invitation),
        )
        .route(
            "/organizations/{org}/invitations/{id}",
            delete(organizations::revoke_invitation),
        )
        .route(
            "/organizations/{org}/transfer-ownership",
            post(organizations::transfer_ownership),
        )
        .route("/organizations/{org}/audit", get(audit::list_audit))
        .route("/platform/overview", get(platform::overview))
        .route("/operations/{id}", get(organizations::get_operation))
        .route(
            "/operations/{id}/retry",
            post(organizations::retry_operation),
        )
        .route(
            "/platform/organizations/{org}/template-release/promote-configured",
            post(platform_releases::promote_configured_release),
        )
        // Authorized organization-scoped tenant lifecycle.
        .route(
            "/organizations/{org}/tenants",
            get(tenants::list_tenants).post(tenants::create_tenant),
        )
        .route(
            "/organizations/{org}/tenants/{tenant}",
            get(tenants::get_tenant).delete(tenants::delete_tenant),
        )
        .route(
            "/organizations/{org}/tenants/{tenant}/start",
            post(tenants::start_tenant),
        )
        .route(
            "/organizations/{org}/tenants/{tenant}/stop",
            post(tenants::stop_tenant),
        )
        .route(
            "/organizations/{org}/tenants/{tenant}/clean",
            post(tenants::clean_tenant),
        )
        .route(
            "/organizations/{org}/tenants/{tenant}/status",
            get(tenants::tenant_status),
        )
        // GitHub connection lifecycle.
        .route(
            "/organizations/{org}/github/connect",
            post(connections::connect),
        )
        .route("/github/oauth/callback", get(connections::oauth_callback))
        .route("/github/setup", get(connections::setup_callback))
        .route(
            "/organizations/{org}/github/connections",
            get(connections::list_connections),
        )
        .route(
            "/organizations/{org}/github/connections/{id}/repositories",
            get(connections::list_repositories),
        )
        .route(
            "/organizations/{org}/github/connections/{id}/reconcile",
            post(connections::reconcile_connection),
        )
        .route(
            "/organizations/{org}/github/connections/{id}",
            delete(connections::disconnect),
        )
        // Public signed webhook ingress.
        .route("/webhooks/github", post(webhooks::receive_webhook))
        .route("/hooks/coder/chat", post(coder_hooks::receive))
        // Runtime credential broker: authenticated by a workspace runtime
        // credential, never by a human session.
        .route(
            "/runtime/github-credentials",
            post(runtime::github_credentials),
        )
        // Runtime worker provisioning and workspace lifecycle:
        // authenticated by a workspace runtime credential (the tenant Nexus).
        .route("/runtime/workspaces", post(runtime::request_worker))
        .route("/runtime/chats", post(runtime::create_chat))
        .route(
            "/runtime/chats/{chat}/messages",
            post(runtime::send_chat_message),
        )
        .route(
            "/runtime/workspaces/{workspace}",
            get(runtime::get_workspace).delete(runtime::delete_workspace),
        )
        .route(
            "/runtime/workspaces/{workspace}/stop",
            post(runtime::stop_workspace),
        )
        .route("/runtime/register", post(runtime::register))
        .route("/runtime/heartbeat", post(runtime::heartbeat))
        .route("/runtime/refresh", post(runtime::refresh))
        .route("/runtime/revoke", post(runtime::revoke))
}

#[derive(Serialize)]
struct ApiIndexResponse {
    version: &'static str,
}

async fn api_index() -> Json<ApiIndexResponse> {
    // Return a deliberately small payload. The index is a capability marker,
    // not a service-discovery document.
    Json(ApiIndexResponse { version: "v1" })
}
