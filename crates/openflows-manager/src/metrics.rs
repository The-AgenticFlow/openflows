//! Request metrics in the Prometheus text format, served at `/metrics` (#532).
//!
//! Every label value comes from a fixed set: the HTTP method, the route
//! template axum matched (`/api/v1/tenants/{tenant_id}`, never the concrete
//! path) and the status code. No user, repository, tenant, token or query
//! value can reach a label, so the output is safe to scrape and store.
//!
//! The endpoint answers 503 unless `OPENFLOWS_MANAGER_METRICS=true`, so a
//! deployment that has not opted in, including hosted SaaS, exposes no metrics.

use axum::{
    extract::{MatchedPath, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::{Mutex, OnceLock},
    time::Instant,
};

/// Opt-in switch for the `/metrics` endpoint. Counting always runs; it is a
/// few map updates per request and keeps the numbers whole when enabled.
pub const METRICS_ENV: &str = "OPENFLOWS_MANAGER_METRICS";

/// Requests not matched to a route share one label so a scan of random paths
/// cannot create a label value per path.
const UNMATCHED: &str = "unmatched";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RequestKey {
    method: String,
    route: String,
    status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteKey {
    method: String,
    route: String,
}

#[derive(Debug, Default, Clone, Copy)]
struct DurationTotals {
    count: u64,
    sum_seconds: f64,
}

/// Counters for every request the Manager answered.
#[derive(Debug, Default)]
pub struct RequestMetrics {
    requests: Mutex<BTreeMap<RequestKey, u64>>,
    durations: Mutex<BTreeMap<RouteKey, DurationTotals>>,
}

impl RequestMetrics {
    /// Record one answered request.
    pub fn record(&self, method: &str, route: &str, status: u16, seconds: f64) {
        let method = known_method(method);
        if let Ok(mut requests) = self.requests.lock() {
            *requests
                .entry(RequestKey {
                    method: method.to_string(),
                    route: route.to_string(),
                    status,
                })
                .or_default() += 1;
        }
        if let Ok(mut durations) = self.durations.lock() {
            let entry = durations
                .entry(RouteKey {
                    method: method.to_string(),
                    route: route.to_string(),
                })
                .or_default();
            entry.count += 1;
            entry.sum_seconds += seconds;
        }
    }

    /// The metrics in the Prometheus text exposition format, version 0.0.4.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# HELP openflows_manager_build_info Manager version; always 1.\n");
        out.push_str("# TYPE openflows_manager_build_info gauge\n");
        let _ = writeln!(
            out,
            "openflows_manager_build_info{{version=\"{}\"}} 1",
            escape(env!("CARGO_PKG_VERSION"))
        );

        out.push_str("# HELP openflows_manager_http_requests_total Requests answered, by method, route template and status.\n");
        out.push_str("# TYPE openflows_manager_http_requests_total counter\n");
        if let Ok(requests) = self.requests.lock() {
            for (key, count) in requests.iter() {
                let _ = writeln!(
                    out,
                    "openflows_manager_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}",
                    escape(&key.method),
                    escape(&key.route),
                    key.status,
                    count
                );
            }
        }

        out.push_str("# HELP openflows_manager_http_request_duration_seconds Time to answer, by method and route template.\n");
        out.push_str("# TYPE openflows_manager_http_request_duration_seconds summary\n");
        if let Ok(durations) = self.durations.lock() {
            for (key, duration) in durations.iter() {
                let labels = format!(
                    "method=\"{}\",route=\"{}\"",
                    escape(&key.method),
                    escape(&key.route)
                );
                let _ = writeln!(
                    out,
                    "openflows_manager_http_request_duration_seconds_sum{{{labels}}} {}",
                    duration.sum_seconds
                );
                let _ = writeln!(
                    out,
                    "openflows_manager_http_request_duration_seconds_count{{{labels}}} {}",
                    duration.count
                );
            }
        }
        out
    }
}

/// The process-wide counters the middleware writes and `/metrics` reads.
pub fn global() -> &'static RequestMetrics {
    static METRICS: OnceLock<RequestMetrics> = OnceLock::new();
    METRICS.get_or_init(RequestMetrics::default)
}

/// Whether `/metrics` answers. Read on each request so an operator can tell
/// from the response alone whether the switch took effect.
pub fn enabled() -> bool {
    switch_on(std::env::var(METRICS_ENV).ok().as_deref())
}

/// Only an explicit `true` or `1` turns metrics on; anything else leaves them off.
fn switch_on(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1" | "true" | "TRUE" | "True"))
}

/// Middleware: count each request under its route template.
pub async fn track(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| UNMATCHED.to_string());
    let started = Instant::now();
    let response = next.run(request).await;
    global().record(
        &method,
        &route,
        response.status().as_u16(),
        started.elapsed().as_secs_f64(),
    );
    response
}

/// `GET /metrics`: the counters, or 503 when metrics are not enabled.
pub async fn metrics() -> Response {
    if !enabled() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        global().render(),
    )
        .into_response()
}

/// Standard methods keep their name; anything else is counted as `OTHER`, so
/// a client cannot invent label values with made-up methods.
fn known_method(method: &str) -> &str {
    match method {
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" => method,
        _ => "OTHER",
    }
}

/// Escape a label value as the text format requires.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_requests_by_route_template_and_status() {
        let metrics = RequestMetrics::default();
        metrics.record("POST", "/api/v1/local/github-credential", 200, 0.25);
        metrics.record("POST", "/api/v1/local/github-credential", 200, 0.75);
        metrics.record("POST", "/api/v1/local/github-credential", 403, 0.1);
        let out = metrics.render();
        assert!(out.contains(
            "openflows_manager_http_requests_total{method=\"POST\",route=\"/api/v1/local/github-credential\",status=\"200\"} 2"
        ));
        assert!(out.contains(
            "openflows_manager_http_requests_total{method=\"POST\",route=\"/api/v1/local/github-credential\",status=\"403\"} 1"
        ));
        assert!(out.contains(
            "openflows_manager_http_request_duration_seconds_count{method=\"POST\",route=\"/api/v1/local/github-credential\"} 3"
        ));
        assert!(out.contains(
            "openflows_manager_http_request_duration_seconds_sum{method=\"POST\",route=\"/api/v1/local/github-credential\"} 1.1"
        ));
    }

    #[test]
    fn invented_methods_and_odd_characters_cannot_create_free_labels() {
        let metrics = RequestMetrics::default();
        metrics.record("BREW", "/odd\"route\\\nx", 404, 0.0);
        let out = metrics.render();
        assert!(out.contains("method=\"OTHER\""));
        assert!(!out.contains("BREW"));
        assert!(out.contains("route=\"/odd\\\"route\\\\\\nx\""));
    }

    #[test]
    fn output_starts_with_build_info() {
        let out = RequestMetrics::default().render();
        assert!(out.starts_with("# HELP openflows_manager_build_info"));
        assert!(out.contains(&format!(
            "openflows_manager_build_info{{version=\"{}\"}} 1",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn switch_accepts_only_explicit_true_values() {
        for (value, expected) in [
            (Some("true"), true),
            (Some(" 1 "), true),
            (Some("false"), false),
            (Some("yes"), false),
            (Some(""), false),
            (None, false),
        ] {
            assert_eq!(switch_on(value), expected, "value {value:?}");
        }
    }
}
