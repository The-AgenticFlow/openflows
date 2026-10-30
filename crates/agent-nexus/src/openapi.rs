//! OpenAPI (OpenAPI 3.1) documentation for Agent Nexus HTTP surfaces.
//!
//! Agent Nexus hosts two separate listeners with disjoint routes, so they get
//! **filtered per-listener documents** rather than a single combined one:
//!
//! - **A2A relay** (`crates/agent-nexus/src/a2a/http_server.rs`): JSON-RPC
//!   (`POST /rpc`), an SSE stream (`GET /`), health, and an agent card.
//! - **Lifecycle hooks consumer** (`crates/agent-nexus/src/hooks/server.rs`):
//!   a JWT-signed webhook and its health probe.
//!
//! Each listener serves only the routes it actually registers. The documents
//! are generated from the **same route tables** that build the listeners
//! (`crate::a2a::http_server::routes`, `crate::hooks::server::routes`), so a
//! route cannot be registered without being documented.

use utoipa::openapi::{
    info::Info,
    path::{HttpMethod, OperationBuilder, Parameter, ParameterIn, PathItemBuilder, PathsBuilder},
    request_body::{RequestBody, RequestBodyBuilder},
    schema::{ArrayBuilder, SchemaType},
    security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme},
    Content, ObjectBuilder, OpenApi, OpenApiBuilder, Paths, Ref, RefOr, ResponseBuilder, Schema,
    Type,
};

/// Documentation metadata for one (method, path) operation, derived from the
/// shared route tables of the listeners.
struct DocRoute {
    method: &'static str,
    path: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
}

fn a2a_doc_routes() -> Vec<DocRoute> {
    crate::a2a::http_server::routes()
        .into_iter()
        .map(|rt| DocRoute {
            method: rt.method,
            path: rt.path,
            tag: "a2a",
            summary: rt.summary,
            description: rt.description,
        })
        .collect()
}

fn hooks_doc_routes() -> Vec<DocRoute> {
    crate::hooks::server::routes()
        .into_iter()
        .map(|rt| DocRoute {
            method: rt.method,
            path: rt.path,
            tag: "hooks",
            summary: rt.summary,
            description: rt.description,
        })
        .collect()
}

/// Build an operation for a route.
fn build_operation(route: &DocRoute) -> utoipa::openapi::path::Operation {
    let mut op = OperationBuilder::new()
        .summary(Some(route.summary))
        .tag(route.tag)
        .description(Some(route.description))
        .response(
            "200",
            ResponseBuilder::new().description("Successful response"),
        );

    match route.path {
        "/rpc" => {
            op = op.request_body(Some(json_body("JsonRpcRequest"))).response(
                "200",
                json_response(
                    "JSON-RPC response (carries either result or error)",
                    "JsonRpcResponse",
                ),
            );
            // Malformed JSON bodies are rejected by axum before the handler and
            // do not use the JSON-RPC envelope, so no 400/500 are documented here.
        }
        "/" => {
            // The SSE stream requires a task_id query parameter.
            let mut param = Parameter::new("task_id");
            param.parameter_in = ParameterIn::Query;
            param.schema = Some(RefOr::T(Schema::Object(
                ObjectBuilder::new().schema_type(Type::String).build(),
            )));
            op = op
                .parameter(param)
                .response(
                    "400",
                    ResponseBuilder::new().description("Missing or empty task_id query parameter"),
                )
                .response(
                    "200",
                    ResponseBuilder::new()
                        .description("Server-Sent Events stream of VerifyProgressEvent chunks")
                        .content(
                            "text/event-stream",
                            Content::new(Some(RefOr::T(Schema::Object(
                                ObjectBuilder::new().schema_type(Type::String).build(),
                            )))),
                        ),
                );
        }
        "/experimental/hooks/chat" => {
            op = op
                .request_body(Some(json_body("HookPayload")))
                .security(SecurityRequirement::new("coderHookJwt", [] as [&str; 0]))
                .response(
                    "200",
                    json_response("Hook decision in Coder's response schema", "HookDecision"),
                )
                .response(
                    "401",
                    ResponseBuilder::new()
                        .description("JWT signature, audience, or expiry verification failed"),
                );
        }
        _ => {}
    }

    op.build()
}

/// Build a JSON request body referencing a component schema.
fn json_body(schema: &str) -> RequestBody {
    RequestBodyBuilder::new()
        .content(
            "application/json",
            Content::new(Some(Ref::from_schema_name(schema))),
        )
        .build()
}

/// Build a JSON response referencing a component schema.
fn json_response(description: &str, schema: &str) -> ResponseBuilder {
    ResponseBuilder::new().description(description).content(
        "application/json",
        Content::new(Some(Ref::from_schema_name(schema))),
    )
}

/// Group a route list into OpenAPI path items.
fn build_paths(routes: &[DocRoute]) -> Paths {
    let mut by_path: std::collections::BTreeMap<&str, Vec<&DocRoute>> =
        std::collections::BTreeMap::new();
    for route in routes {
        by_path.entry(route.path).or_default().push(route);
    }

    let mut builder = PathsBuilder::new();
    for (path, routes) in by_path {
        let mut path_item = PathItemBuilder::new();
        for route in routes {
            let method = match route.method {
                "GET" => HttpMethod::Get,
                "POST" => HttpMethod::Post,
                other => panic!("unsupported method {other} for {path}"),
            };
            path_item = path_item.operation(method, build_operation(route));
        }
        builder = builder.path(path, path_item.build());
    }
    builder.build()
}

/// A schema that is `string` OR `null` (OAS 3.1 multi-type).
fn nullable_string() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(SchemaType::Array(vec![Type::String, Type::Null]))
}

/// Build the OpenAPI document for a given route list.
fn build_openapi(routes: &[DocRoute], title: &str) -> OpenApi {
    OpenApiBuilder::new()
        .info(Info::new(title, env!("CARGO_PKG_VERSION")))
        .paths(build_paths(routes))
        .components(Some(components()))
        .build()
}

/// Reusable component schemas for Agent Nexus.
fn components() -> utoipa::openapi::schema::Components {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    // JSON-RPC id: the relay accepts and echoes string, integer, or null ids.
    let rpc_id = || {
        ObjectBuilder::new().schema_type(SchemaType::Array(vec![
            Type::String,
            Type::Integer,
            Type::Null,
        ]))
    };

    utoipa::openapi::schema::ComponentsBuilder::new()
        .security_scheme(
            "coderHookJwt",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Authorization"))),
        )
        // ---- Lifecycle hook payload ----
        .schema(
            "HookPayload",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some(
                    "A Coder lifecycle-hook dispatch, signed with the shared secret",
                ))
                .property("type", string())
                .property("meta", ObjectBuilder::new().schema_type(Type::Object))
                .property("data", ObjectBuilder::new().schema_type(Type::Object))
                .required("type"),
        )
        // ---- A2A JSON-RPC envelope ----
        .schema(
            "JsonRpcRequest",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some("A2A JSON-RPC 2.0 request (simplified for the relay)"))
                .property("jsonrpc", string())
                .property("method", string())
                .property("params", ObjectBuilder::new().schema_type(Type::Object))
                .property("id", rpc_id())
                .required("jsonrpc")
                .required("method"),
        )
        .schema(
            "JsonRpcResponse",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some("A2A JSON-RPC 2.0 response envelope"))
                .property("jsonrpc", string())
                // result and error may be absent (or null, e.g. tasks/claim
                // returns result: null when no pending task).
                .property(
                    "result",
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Array(vec![Type::Object, Type::Null])),
                )
                .property(
                    "error",
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Array(vec![Type::Object, Type::Null])),
                )
                .property("id", rpc_id())
                .required("jsonrpc")
                .required("id"),
        )
        // ---- A2A verify task payloads ----
        .schema(
            "VerifyRequest",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some(
                    "Sentinel → Forge (via nexus relay) verification request",
                ))
                .property("pair_id", string())
                .property("kind", string())
                .property("cwd", string())
                .property(
                    "argv",
                    ArrayBuilder::new().items(ObjectBuilder::new().schema_type(Type::String)),
                )
                .property(
                    "timeout_secs",
                    ObjectBuilder::new().schema_type(Type::Integer),
                )
                .property(
                    "env_allowlist",
                    ArrayBuilder::new().items(ObjectBuilder::new().schema_type(Type::String)),
                )
                .property("expect", ObjectBuilder::new().schema_type(Type::Object))
                .required("pair_id")
                .required("kind")
                .required("cwd")
                .required("argv")
                .required("timeout_secs"),
        )
        .schema(
            "VerifyResult",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some("Terminal result of a verify task"))
                .property("head_sha", nullable_string())
                .property("task_id", string())
                .property(
                    "exit_code",
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Array(vec![Type::Integer, Type::Null])),
                )
                .property("timed_out", ObjectBuilder::new().schema_type(Type::Boolean))
                .property(
                    "duration_ms",
                    ObjectBuilder::new().schema_type(Type::Integer),
                )
                .property("stdout_ref", string())
                .property("stderr_ref", string())
                .property(
                    "artifacts",
                    ArrayBuilder::new().items(ObjectBuilder::new().schema_type(Type::Object)),
                )
                .property("executor", ObjectBuilder::new().schema_type(Type::Object))
                .required("task_id")
                .required("timed_out")
                .required("duration_ms")
                .required("stdout_ref")
                .required("stderr_ref"),
        )
        // ---- Lifecycle hooks decision ----
        .schema(
            "HookDecision",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some(
                    "Coder's lifecycle-hook response schema produced by the hook consumer",
                ))
                .property("model_context", nullable_string())
                .property(
                    "permission",
                    ObjectBuilder::new()
                        .schema_type(Type::Object)
                        .description(Some(
                            "decision: allow|deny; input_override present on rewrites",
                        )),
                ),
        )
        .build()
}

/// Build the A2A relay OpenAPI document.
pub fn a2a_openapi() -> OpenApi {
    build_openapi(&a2a_doc_routes(), "Agent Nexus A2A Relay API")
}

/// Build the lifecycle hooks OpenAPI document.
pub fn hooks_openapi() -> OpenApi {
    build_openapi(&hooks_doc_routes(), "Agent Nexus Lifecycle Hooks API")
}

/// Serialize the A2A OpenAPI document to JSON.
pub fn a2a_openapi_json() -> serde_json::Value {
    serde_json::to_value(a2a_openapi()).expect("OpenAPI document serializes to JSON")
}

/// Serialize the hooks OpenAPI document to JSON.
pub fn hooks_openapi_json() -> serde_json::Value {
    serde_json::to_value(hooks_openapi()).expect("OpenAPI document serializes to JSON")
}

/// Axum handler serving the A2A document at `/openapi.json`.
pub async fn serve_a2a_openapi() -> axum::Json<serde_json::Value> {
    axum::Json(a2a_openapi_json())
}

/// Axum handler serving the hooks document at `/openapi.json`.
pub async fn serve_hooks_openapi() -> axum::Json<serde_json::Value> {
    axum::Json(hooks_openapi_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route in each listener's shared route table is documented with
    /// the correct method/path in that listener's OpenAPI document.
    #[test]
    fn every_route_is_documented() {
        let a2a = a2a_openapi();
        for rt in crate::a2a::http_server::routes() {
            assert_operation(&a2a, rt.method, rt.path);
        }
        let hooks = hooks_openapi();
        for rt in crate::hooks::server::routes() {
            assert_operation(&hooks, rt.method, rt.path);
        }
    }

    fn assert_operation(doc: &OpenApi, method: &str, path: &str) {
        let path_item = doc
            .paths
            .paths
            .get(path)
            .unwrap_or_else(|| panic!("route {path} not in OpenAPI paths"));
        let operation = match method {
            "GET" => &path_item.get,
            "POST" => &path_item.post,
            other => panic!("unhandled method {other}"),
        };
        assert!(
            operation.is_some(),
            "route {} {} missing operation in OpenAPI",
            method,
            path
        );
    }

    /// Documents serialize to valid JSON.
    #[test]
    fn documents_serialize_to_json() {
        for json in [a2a_openapi_json(), hooks_openapi_json()] {
            assert!(json.get("openapi").is_some());
            assert!(json.get("paths").is_some());
        }
    }

    /// Every documented route is registered on its listener (both directions
    /// are single-source by construction, but this probes the real router).
    #[tokio::test]
    async fn documented_routes_are_registered_on_their_listeners() {
        use config::env::CoderHooksConfig;
        use pocketflow_core::SharedStore;
        use std::sync::Arc;

        let store = Arc::new(SharedStore::new_in_memory());
        let relay = Arc::new(super::super::a2a::A2ARelay::new(store.clone()));
        let a2a_app = super::super::a2a::create_router(relay);

        let hook_config = CoderHooksConfig {
            chat_hook_secret: Some("0123456789abcdef0123456789abcdef".to_string()),
            chat_hook_timeout_ms: 1500,
            chat_hook_enabled: true,
            chat_hook_allow_insecure: true,
            hook_addr: "127.0.0.1:3001".to_string(),
            hook_host: "127.0.0.1".to_string(),
            chat_hook_url: None,
            hook_url: None,
            hook_logs: false,
        };
        let hooks_app = super::super::hooks::create_router(store, hook_config, None, None);

        let mut failures = Vec::new();
        for rt in crate::a2a::http_server::routes() {
            if let Some(f) = probe(&a2a_app, rt.method, rt.path).await {
                failures.push(f);
            }
        }
        for rt in crate::hooks::server::routes() {
            if let Some(f) = probe(&hooks_app, rt.method, rt.path).await {
                failures.push(f);
            }
        }
        assert!(
            failures.is_empty(),
            "routes not registered on their listeners:\n{}",
            failures.join("\n")
        );
    }

    async fn probe(app: &axum::Router, method: &str, path: &str) -> Option<String> {
        use axum::http::Request;
        use tower::ServiceExt;

        let request = Request::builder()
            .method(axum::http::Method::from_bytes(method.as_bytes()).unwrap())
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        let status = app.clone().oneshot(request).await.unwrap().status();
        if status == axum::http::StatusCode::NOT_FOUND {
            Some(format!("{method} {path} -> 404"))
        } else if status == axum::http::StatusCode::METHOD_NOT_ALLOWED {
            Some(format!("{method} {path} -> 405"))
        } else {
            None
        }
    }

    /// The A2A document's SSE operation declares the required task_id query
    /// parameter and a text/event-stream response.
    #[test]
    fn stream_operation_declares_task_id_query_param() {
        let doc = a2a_openapi();
        let path_item = doc.paths.paths.get("/").expect("/ path");
        let op = path_item.get.as_ref().expect("GET / operation");
        let params = op.parameters.as_ref().expect("parameters present");
        let has_task_id = params.iter().any(|p| match p {
            RefOr::T(parameter) => {
                parameter.name == "task_id"
                    && matches!(parameter.parameter_in, ParameterIn::Query)
                    && matches!(parameter.required, utoipa::openapi::Required::True)
            }
            RefOr::Ref(_) => false,
        });
        assert!(
            has_task_id,
            "GET / must declare a required task_id query parameter"
        );
    }

    /// The hooks document declares the JWT Authorization requirement and a
    /// request body on POST /experimental/hooks/chat.
    #[test]
    fn hook_operation_declares_jwt_security_and_request_body() {
        let doc = hooks_openapi();
        let path_item = doc
            .paths
            .paths
            .get("/experimental/hooks/chat")
            .expect("hook path");
        let op = path_item.post.as_ref().expect("POST hook operation");

        let has_security = op.security.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
        assert!(
            has_security,
            "hook operation must declare a security requirement"
        );

        assert!(
            op.request_body.is_some(),
            "hook operation must declare a request body"
        );

        // The security requirement must reference the declared coderHookJwt scheme.
        let has_scheme = doc
            .components
            .as_ref()
            .map(|c| c.security_schemes.contains_key("coderHookJwt"))
            .unwrap_or(false);
        assert!(
            has_scheme,
            "components must declare the coderHookJwt security scheme"
        );
    }
}
