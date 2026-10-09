//! Liveness and readiness endpoints for the Manager process.

use crate::server::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

/// Sanitized dependency status for the readiness body. Only the dependency name
/// and its ok flag are exposed; internal error text is never sent to clients.
#[derive(Serialize)]
struct DependencyProbe {
    name: &'static str,
    ok: bool,
}

#[derive(Serialize)]
struct ReadinessResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
    dependencies: Vec<DependencyProbe>,
}

pub async fn health() -> Json<HealthResponse> {
    // Liveness answers whether the process can serve HTTP, not whether every
    // dependency is healthy. This endpoint intentionally does not touch Redis.
    Json(HealthResponse {
        status: "ready",
        service: "openflows-manager",
        api_version: "v1",
    })
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    // Readiness is stricter than liveness: callers should route traffic here
    // only after the manager can reach its dependencies. Upstream capability
    // failures are reported separately per dependency so an operator can tell
    // which backing store is unavailable instead of seeing one opaque failure.
    let report = state.readiness_report().await;

    if report.ready {
        (
            StatusCode::OK,
            Json(ReadinessResponse {
                status: "ready",
                service: "openflows-manager",
                api_version: "v1",
                dependencies: report
                    .dependencies
                    .iter()
                    .map(|d| DependencyProbe {
                        name: d.name,
                        ok: d.ok,
                    })
                    .collect(),
            }),
        )
    } else {
        // Log the specific failure for operators, but return a sanitized body
        // so dependency URLs, tokens, or internal errors are not exposed
        // through a public probe.
        for dep in &report.dependencies {
            if let Some(err) = &dep.error {
                tracing::warn!(
                    dependency = dep.name,
                    error = err,
                    "OpenFlows Manager readiness dependency failed"
                );
            }
        }
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                status: "unavailable",
                service: "openflows-manager",
                api_version: "v1",
                dependencies: report
                    .dependencies
                    .iter()
                    .map(|d| DependencyProbe {
                        name: d.name,
                        ok: d.ok,
                    })
                    .collect(),
            }),
        )
    }
}
