//! OpenAPI (OpenAPI 3.1) documentation for the OpenFlows Manager HTTP API.
//!
//! The spec is generated with [`utoipa`] from the **same route table** used to
//! build the axum router (`crate::routes`), so a route cannot be registered
//! without being documented and vice versa. See
//! `docs/architecture/openapi-reference.md`.

use utoipa::openapi::{
    info::Info,
    path::{HttpMethod, OperationBuilder, Parameter, ParameterIn, PathItemBuilder, PathsBuilder},
    request_body::{RequestBody, RequestBodyBuilder},
    schema::{ArrayBuilder, SchemaType},
    Content, ObjectBuilder, OpenApi, OpenApiBuilder, Paths, Ref, RefOr, ResponseBuilder, Type,
};

/// Authentication/visibility model of a route, used to attach the correct
/// OpenAPI `security` requirement and descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// No authentication.
    None,
    /// Bearer token (human session or workspace runtime credential).
    Bearer,
    /// Browser session cookie.
    Session,
    /// Signed webhook (GitHub signature or Coder hook JWT).
    Webhook,
}

/// Flattened documentation metadata for one (method, path) operation, derived
/// from the shared route table.
#[derive(Debug, Clone)]
pub(crate) struct RouteMeta {
    pub method: &'static str,
    pub path: String,
    pub tag: &'static str,
    pub summary: &'static str,
    pub auth: Auth,
    pub is_json: bool,
    pub success: u16,
    pub description: Option<&'static str>,
}

/// Derive the full list of documented operations from the shared route table.
/// API routes are prefixed with `/api/v1`; the `/openapi.json` endpoint served
/// by the Swagger UI is added explicitly.
pub(crate) fn manager_routes() -> Vec<RouteMeta> {
    let mut out = Vec::new();
    for rt in crate::routes::root_routes() {
        for m in rt.methods {
            out.push(RouteMeta {
                method: m,
                path: rt.path.to_string(),
                tag: rt.tag,
                summary: rt.summary,
                auth: rt.auth,
                is_json: rt.is_json,
                success: rt.success,
                description: rt.description,
            });
        }
    }
    for rt in crate::routes::api_v1_routes() {
        // The nested `/` route is registered at `/api/v1` (no trailing slash).
        let full_path = if rt.path == "/" {
            "/api/v1".to_string()
        } else {
            format!("/api/v1{}", rt.path)
        };
        for m in rt.methods {
            out.push(RouteMeta {
                method: m,
                path: full_path.clone(),
                tag: rt.tag,
                summary: rt.summary,
                auth: rt.auth,
                is_json: rt.is_json,
                success: rt.success,
                description: rt.description,
            });
        }
    }
    // The spec endpoint itself (served by the Swagger UI merge).
    out.push(RouteMeta {
        method: "GET",
        path: "/openapi.json".to_string(),
        tag: "root",
        summary: "OpenAPI specification document",
        auth: Auth::None,
        is_json: true,
        success: 200,
        description: None,
    });
    out
}

/// Extract path parameter names from a templated path, e.g. `{org}` from
/// `/organizations/{org}`.
fn path_parameters(path: &str) -> Vec<String> {
    let mut names = Vec::new();
    for segment in path.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            names.push(name.to_string());
        }
    }
    names
}

/// The OpenAPI security requirement for a route's auth model.
fn security_requirement(auth: Auth) -> Option<utoipa::openapi::SecurityRequirement> {
    match auth {
        Auth::Bearer => Some(utoipa::openapi::SecurityRequirement::new(
            "bearerAuth",
            [] as [&str; 0],
        )),
        _ => None,
    }
}

/// A standard JSON error response referencing the shared [`ApiError`] envelope.
fn json_error_response(description: &str) -> ResponseBuilder {
    ResponseBuilder::new().description(description).content(
        "application/json",
        Content::new(Some(Ref::from_schema_name("ApiError"))),
    )
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

/// The request-body component schema for a JSON operation, if it takes one.
fn request_body_schema(path: &str, method: &str) -> Option<&'static str> {
    match (method, path) {
        ("POST", "/api/v1/organizations") => Some("CreateOrgRequest"),
        ("PATCH", "/api/v1/organizations/{org}") => Some("UpdateOrgRequest"),
        ("PATCH", "/api/v1/organizations/{org}/members/{user}") => Some("UpdateMemberRequest"),
        ("POST", "/api/v1/organizations/{org}/invitations") => Some("CreateInvitationRequest"),
        ("POST", "/api/v1/organizations/{org}/transfer-ownership") => Some("TransferRequest"),
        ("POST", "/api/v1/organizations/{org}/tenants") => Some("CreateTenantRequest"),
        ("POST", "/api/v1/organizations/{org}/tenants/{tenant}/clean") => {
            Some("CleanTenantRequest")
        }
        ("POST", "/api/v1/organizations/{org}/github/connect") => Some("ConnectRequest"),
        ("POST", "/api/v1/auth/refresh") => Some("RefreshRequest"),
        ("POST", "/api/v1/auth/cli/token") => Some("TokenRequest"),
        ("POST", "/api/v1/runtime/register") => Some("RegisterRequest"),
        ("POST", "/api/v1/runtime/github-credentials") => Some("BrokerRequest"),
        ("POST", "/api/v1/runtime/workspaces") => Some("WorkerRequest"),
        ("POST", "/api/v1/runtime/chats") => Some("CreateChatRequest"),
        ("POST", "/api/v1/runtime/chats/{chat}/messages") => Some("SendChatMessageRequest"),
        ("POST", "/api/v1/local/github-credential") => Some("CredentialRequest"),
        _ => None,
    }
}

/// The response-body component schema for a successful JSON operation, if any.
fn response_schema(path: &str, method: &str) -> Option<&'static str> {
    match (method, path) {
        ("GET", "/api/v1/me") => Some("User"),
        ("GET", "/api/v1/organizations") => Some("OrganizationList"),
        ("POST", "/api/v1/organizations") => Some("Operation"),
        ("GET", "/api/v1/organizations/{org}") => Some("Organization"),
        ("PATCH", "/api/v1/organizations/{org}") => Some("Organization"),
        ("DELETE", "/api/v1/organizations/{org}") => Some("Operation"),
        ("GET", "/api/v1/organizations/{org}/members") => Some("MembershipList"),
        ("PATCH", "/api/v1/organizations/{org}/members/{user}") => Some("Membership"),
        ("GET", "/api/v1/operations/{id}") => Some("Operation"),
        ("GET", "/api/v1/organizations/{org}/tenants") => Some("TenantList"),
        ("GET", "/api/v1/organizations/{org}/tenants/{tenant}") => Some("Tenant"),
        ("GET", "/api/v1/organizations/{org}/tenants/{tenant}/status") => Some("Tenant"),
        ("GET", "/api/v1/organizations/{org}/github/connections") => Some("ConnectionsResponse"),
        ("GET", "/api/v1/runtime/workspaces/{workspace}") => Some("WorkspaceStatus"),
        ("POST", "/api/v1/auth/refresh") => Some("RefreshResponse"),
        ("POST", "/api/v1/auth/cli/start") => Some("DeviceStartResponse"),
        ("POST", "/api/v1/auth/cli/token") => Some("TokenResponse"),
        ("POST", "/api/v1/runtime/register") => Some("RegisterResponse"),
        ("POST", "/api/v1/runtime/github-credentials") => Some("BrokerResponse"),
        ("POST", "/api/v1/local/github-credential") => Some("CredentialResponse"),
        _ => None,
    }
}

/// Whether an operation requires the `Idempotency-Key` header (its handler
/// calls `idempotency_key`).
fn requires_idempotency_key(path: &str, method: &str) -> bool {
    matches!(
        (method, path),
        ("POST", "/api/v1/organizations")
            | ("DELETE", "/api/v1/organizations/{org}")
            | ("POST", "/api/v1/organizations/{org}/invitations")
            | ("POST", "/api/v1/operations/{id}/retry")
            | ("POST", "/api/v1/organizations/{org}/tenants")
            | ("DELETE", "/api/v1/organizations/{org}/tenants/{tenant}")
            | ("POST", "/api/v1/organizations/{org}/tenants/{tenant}/clean")
            | ("POST", "/api/v1/organizations/{org}/github/connect")
            | (
                "POST",
                "/api/v1/organizations/{org}/github/connections/{id}/reconcile"
            )
            | (
                "DELETE",
                "/api/v1/organizations/{org}/github/connections/{id}"
            )
            | ("POST", "/api/v1/runtime/workspaces")
            | ("DELETE", "/api/v1/runtime/workspaces/{workspace}")
    )
}

/// Whether a GET list operation uses `limit`/`after` cursor pagination.
fn is_paginated(path: &str, method: &str) -> bool {
    matches!(
        (method, path),
        ("GET", "/api/v1/organizations")
            | ("GET", "/api/v1/organizations/{org}/members")
            | ("GET", "/api/v1/organizations/{org}/tenants")
    )
}

/// Build an [`Operation`] for one (method, path) route.
fn build_operation(meta: &RouteMeta) -> utoipa::openapi::path::Operation {
    let success = if meta.success == 204 {
        ResponseBuilder::new().description("No content")
    } else if let Some(schema) = response_schema(&meta.path, meta.method) {
        json_response("Successful response", schema)
    } else {
        ResponseBuilder::new().description("Successful response")
    };

    let mut op = OperationBuilder::new()
        .summary(Some(meta.summary))
        .tag(meta.tag)
        .response(meta.success.to_string(), success);

    if let Some(desc) = meta.description {
        op = op.description(Some(desc));
    }

    // Declare required path parameters for templated paths.
    for name in path_parameters(&meta.path) {
        let mut param = Parameter::new(name);
        param.parameter_in = ParameterIn::Path;
        param.schema = Some(RefOr::T(utoipa::openapi::Schema::Object(
            ObjectBuilder::new().schema_type(Type::String).build(),
        )));
        op = op.parameter(param);
    }

    // Declare the required Idempotency-Key header (1-255 bytes).
    if requires_idempotency_key(&meta.path, meta.method) {
        let mut param = Parameter::new("Idempotency-Key");
        param.parameter_in = ParameterIn::Header;
        param.schema = Some(RefOr::T(utoipa::openapi::Schema::Object(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .min_length(Some(1))
                .max_length(Some(255))
                .build(),
        )));
        op = op.parameter(param);
    }

    // Declare limit/after cursor pagination on list operations.
    if is_paginated(&meta.path, meta.method) {
        let mut limit = Parameter::new("limit");
        limit.parameter_in = ParameterIn::Query;
        limit.required = utoipa::openapi::Required::False;
        limit.schema = Some(RefOr::T(utoipa::openapi::Schema::Object(
            ObjectBuilder::new().schema_type(Type::Integer).build(),
        )));
        op = op.parameter(limit);

        let mut after = Parameter::new("after");
        after.parameter_in = ParameterIn::Query;
        after.required = utoipa::openapi::Required::False;
        after.schema = Some(RefOr::T(utoipa::openapi::Schema::Object(
            ObjectBuilder::new().schema_type(Type::String).build(),
        )));
        op = op.parameter(after);
    }

    // Declare the request body for JSON operations.
    if let Some(schema) = request_body_schema(&meta.path, meta.method) {
        op = op.request_body(Some(json_body(schema)));
    }

    if let Some(sec) = security_requirement(meta.auth) {
        op = op.security(sec);
    }

    if meta.is_json {
        op = op
            .response("400", json_error_response("Bad request"))
            .response("401", json_error_response("Unauthenticated"))
            .response("403", json_error_response("Forbidden"))
            .response("404", json_error_response("Not found"))
            .response("409", json_error_response("Conflict"))
            .response("422", json_error_response("Unprocessable input"))
            .response("429", json_error_response("Rate limited"))
            .response("500", json_error_response("Internal error"))
            .response("503", json_error_response("Service unavailable"));
    }

    op.build()
}

/// Map a route's HTTP method string to the utoipa [`HttpMethod`] enum.
fn http_method(method: &str) -> HttpMethod {
    match method {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "PUT" => HttpMethod::Put,
        _ => HttpMethod::Get,
    }
}

/// Group the route metadata into OpenAPI path items (one per path, all methods).
fn build_paths() -> Paths {
    let routes = manager_routes();
    let mut by_path: std::collections::BTreeMap<&str, Vec<&RouteMeta>> =
        std::collections::BTreeMap::new();
    for meta in &routes {
        by_path.entry(meta.path.as_str()).or_default().push(meta);
    }

    let mut builder = PathsBuilder::new();
    for (path, metas) in by_path {
        let mut path_item = PathItemBuilder::new();
        for meta in metas {
            path_item = path_item.operation(http_method(meta.method), build_operation(meta));
        }
        builder = builder.path(path, path_item.build());
    }
    builder.build()
}

/// Reusable component schemas + security schemes for the Manager API.
fn build_components() -> utoipa::openapi::schema::ComponentsBuilder {
    utoipa::openapi::schema::ComponentsBuilder::new()
        .schema(
            "ApiError",
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .description(Some(
                    "Standard error envelope returned for 4xx/5xx responses",
                ))
                .property("error", error_object_schema())
                .required("error"),
        )
        .security_scheme(
            "bearerAuth",
            utoipa::openapi::security::SecurityScheme::Http(utoipa::openapi::security::Http::new(
                utoipa::openapi::security::HttpAuthScheme::Bearer,
            )),
        )
        .schema("User", user_schema())
        .schema("Organization", organization_schema())
        .schema("Tenant", tenant_schema())
        .schema("Operation", operation_schema())
        // ---- Request bodies ----
        .schema("CreateOrgRequest", create_org_schema())
        .schema("UpdateOrgRequest", update_org_schema())
        .schema("UpdateMemberRequest", update_member_schema())
        .schema("CreateInvitationRequest", create_invitation_schema())
        .schema("TransferRequest", transfer_schema())
        .schema("CreateTenantRequest", create_tenant_schema())
        .schema("CleanTenantRequest", clean_tenant_schema())
        .schema("ConnectRequest", connect_schema())
        .schema("RefreshRequest", refresh_request_schema())
        .schema("TokenRequest", token_request_schema())
        .schema("RegisterRequest", register_request_schema())
        .schema("BrokerRequest", broker_request_schema())
        .schema("WorkerRequest", worker_request_schema())
        .schema("CreateChatRequest", create_chat_schema())
        .schema("SendChatMessageRequest", send_chat_message_schema())
        .schema("CredentialRequest", credential_request_schema())
        // ---- Response bodies ----
        .schema("Membership", membership_schema())
        .schema("OrganizationList", list_schema("Organization"))
        .schema("MembershipList", list_schema("Membership"))
        .schema("TenantList", list_schema("Tenant"))
        .schema("ConnectionsResponse", connections_schema())
        .schema("WorkspaceStatus", workspace_status_schema())
        .schema("RefreshResponse", refresh_response_schema())
        .schema("DeviceStartResponse", device_start_schema())
        .schema("TokenResponse", token_response_schema())
        .schema("RegisterResponse", register_response_schema())
        .schema("BrokerResponse", broker_response_schema())
        .schema("CredentialResponse", credential_response_schema())
}

fn error_object_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("code", string())
        .property("message", string())
        .property("request_id", string())
        .property("retryable", ObjectBuilder::new().schema_type(Type::Boolean))
        .required("code")
        .required("message")
        .required("request_id")
        .required("retryable")
}

/// A schema that is a `string` OR `null` (OAS 3.1 multi-type), matching fields
/// that serialize absent `Option` values as explicit `null`.
fn nullable_string() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(SchemaType::Array(vec![Type::String, Type::Null]))
}

fn user_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("A user as returned by `/api/v1/me`"))
        .property("id", string())
        .property("display_name", string())
        .property("status", string())
        .required("id")
        .required("display_name")
        .required("status")
}

fn organization_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("An organization as returned to members"))
        .property("id", string())
        .property("slug", string())
        .property("display_name", string())
        .property("status", string())
        .property("owner_user_id", string())
        .property("coder_organization_id", nullable_string())
        .required("id")
        .required("slug")
        .required("display_name")
        .required("status")
        .required("owner_user_id")
}

fn tenant_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("A tenant within its organization"))
        .property("id", string())
        .property("organization_id", string())
        .property("slug", string())
        // `github_repository_id` is the numeric `GithubId` wrapper.
        .property(
            "github_repository_id",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("desired_state", string())
        .property("observed_state", string())
        .property(
            "fleet_size",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("error_code", nullable_string())
        .required("id")
        .required("organization_id")
        .required("slug")
        .required("github_repository_id")
        .required("desired_state")
        .required("observed_state")
        .required("fleet_size")
}

fn operation_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("An operation within its organization (sanitized)"))
        .property("id", string())
        .property("organization_id", string())
        .property("kind", string())
        .property("state", string())
        .property("current_step", nullable_string())
        .property(
            "attempt_count",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("error_code", nullable_string())
        .required("id")
        .required("organization_id")
        .required("kind")
        .required("state")
        .required("attempt_count")
}

/// A list wrapper `{ items: [...], next_cursor: string|null }` for paginated
/// GET endpoints.
fn list_schema(item: &str) -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("A cursor-paginated list"))
        .property(
            "items",
            ArrayBuilder::new().items(Ref::from_schema_name(item)),
        )
        .property("next_cursor", nullable_string())
        .required("items")
}

fn create_org_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("slug", string())
        .property("display_name", string())
        .required("slug")
        .required("display_name")
}

fn update_org_schema() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(Type::Object).property(
        "display_name",
        ObjectBuilder::new().schema_type(Type::String),
    )
}

fn update_member_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("role", string())
        .property("status", string())
}

fn create_invitation_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("github_login", string())
        .property("role", string())
        .required("github_login")
        .required("role")
}

fn transfer_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "new_owner_user_id",
            ObjectBuilder::new().schema_type(Type::String),
        )
        .required("new_owner_user_id")
}

fn create_tenant_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "repository_url",
            ObjectBuilder::new().schema_type(Type::String),
        )
        .property(
            "repository_id",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("name", ObjectBuilder::new().schema_type(Type::String))
        .required("name")
}

fn clean_tenant_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("reset_all", ObjectBuilder::new().schema_type(Type::Boolean))
        .required("reset_all")
}

fn connect_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("flow_type", ObjectBuilder::new().schema_type(Type::String))
}

fn refresh_request_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "refresh_token",
            ObjectBuilder::new().schema_type(Type::String),
        )
        .required("refresh_token")
}

fn token_request_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "device_code",
            ObjectBuilder::new().schema_type(Type::String),
        )
        .required("device_code")
}

fn register_request_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("bootstrap", ObjectBuilder::new().schema_type(Type::String))
        .required("bootstrap")
}

fn broker_request_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("purpose", ObjectBuilder::new().schema_type(Type::String))
        .required("purpose")
}

fn worker_request_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("role", string())
        .property("slot", ObjectBuilder::new().schema_type(Type::Integer))
        .property("ticket_id", string())
        .property("branch", string())
        .property("a2a_pair_token", string())
        .required("role")
}

fn create_chat_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("workspace_id", string())
        .property("ticket_id", string())
        .property("prompt", string())
        .required("workspace_id")
        .required("ticket_id")
        .required("prompt")
}

fn send_chat_message_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("message_key", string())
        .property("prompt", string())
        .required("message_key")
        .required("prompt")
}

fn credential_request_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("repository", ObjectBuilder::new().schema_type(Type::String))
        .required("repository")
}

fn membership_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("organization_id", string())
        .property("role", string())
        .property("status", string())
        .required("organization_id")
        .required("role")
        .required("status")
}

fn connections_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("The GitHub connections for an organization"))
        .property(
            "items",
            ArrayBuilder::new().items(ObjectBuilder::new().schema_type(Type::Object)),
        )
        .required("items")
}

fn workspace_status_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("workspace_id", string())
        .property("status", string())
}

fn refresh_response_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("access_token", string())
        .property("refresh_token", string())
        .property(
            "expires_in",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property(
            "refresh_expires_in",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .required("access_token")
        .required("refresh_token")
}

fn device_start_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("device_code", string())
        .property("user_code", string())
        .property("verification_url", string())
        .property(
            "expires_in",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("interval", ObjectBuilder::new().schema_type(Type::Integer))
        .required("device_code")
        .required("user_code")
        .required("verification_url")
}

fn token_response_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("status", string())
        .property("access_token", nullable_string())
        .property("refresh_token", nullable_string())
        .required("status")
}

fn register_response_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("workspace_id", string())
        .property("access_token", string())
        .property("access_expires_at", string())
        .property("refresh_token", string())
        .property("refresh_expires_at", string())
        .property(
            "generation",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .required("workspace_id")
        .required("access_token")
        .required("refresh_token")
}

fn broker_response_schema() -> ObjectBuilder {
    let string = || ObjectBuilder::new().schema_type(Type::String);
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("token", string())
        .property("expires_at", string())
        .property(
            "repository_id",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .property("username", string())
        .property(
            "permissions",
            ObjectBuilder::new().schema_type(Type::Object),
        )
        .required("token")
        .required("repository_id")
}

fn credential_response_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("token", ObjectBuilder::new().schema_type(Type::String))
        .property(
            "repository_id",
            ObjectBuilder::new().schema_type(Type::Integer),
        )
        .required("token")
        .required("repository_id")
}

/// Build the complete Manager OpenAPI document.
pub fn manager_openapi() -> OpenApi {
    OpenApiBuilder::new()
        .info(Info::new(
            "OpenFlows Manager API",
            env!("CARGO_PKG_VERSION"),
        ))
        .paths(build_paths())
        .components(Some(build_components().build()))
        .build()
}

/// Serialize the OpenAPI document to JSON for the `/openapi.json` endpoint.
pub fn manager_openapi_json() -> serde_json::Value {
    serde_json::to_value(manager_openapi()).expect("OpenAPI document serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route in the shared table appears in the generated OpenAPI
    /// document with the correct HTTP method and full path.
    #[test]
    fn every_inventoried_route_is_documented() {
        let doc = manager_openapi();
        let paths = &doc.paths.paths;
        assert!(!paths.is_empty(), "OpenAPI document must contain paths");

        for meta in manager_routes() {
            let path_item = paths
                .get(meta.path.as_str())
                .unwrap_or_else(|| panic!("route {} not in OpenAPI paths", meta.path));
            let operation = match meta.method {
                "GET" => &path_item.get,
                "POST" => &path_item.post,
                "PATCH" => &path_item.patch,
                "DELETE" => &path_item.delete,
                other => panic!("unhandled method {other} for {}", meta.path),
            };
            assert!(
                operation.is_some(),
                "route {} {} missing operation in OpenAPI",
                meta.method,
                meta.path
            );
        }
    }

    /// Templated paths declare their required path parameters.
    #[test]
    fn templated_paths_declare_path_parameters() {
        let doc = manager_openapi();
        let paths = &doc.paths.paths;
        for meta in manager_routes() {
            let names = path_parameters(&meta.path);
            if names.is_empty() {
                continue;
            }
            let path_item = paths.get(meta.path.as_str()).unwrap();
            let operation = match meta.method {
                "GET" => path_item.get.as_ref(),
                "POST" => path_item.post.as_ref(),
                "PATCH" => path_item.patch.as_ref(),
                "DELETE" => path_item.delete.as_ref(),
                _ => None,
            }
            .expect("operation exists");
            let declared = operation
                .parameters
                .as_ref()
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| match p {
                            RefOr::T(param) => Some(param),
                            RefOr::Ref(_) => None,
                        })
                        .filter(|p| matches!(p.parameter_in, ParameterIn::Path))
                        .map(|p| p.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for name in &names {
                assert!(
                    declared.contains(name),
                    "path parameter {name} not declared for {} {}",
                    meta.method,
                    meta.path
                );
            }
        }
    }

    /// The document serializes to valid JSON.
    #[test]
    fn document_serializes_to_json() {
        let json = manager_openapi_json();
        assert!(json.get("openapi").is_some());
        assert!(json.get("paths").is_some());
        assert!(json.get("components").is_some());
    }

    /// Every route in the shared table is actually registered on the router
    /// (probed with a non-404, non-405 response).
    #[tokio::test]
    async fn documented_routes_are_registered_on_the_router() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::server::create_router(crate::server::AppState::for_tests());

        let mut failures = Vec::new();
        for meta in manager_routes() {
            let method = axum::http::Method::from_bytes(meta.method.as_bytes()).unwrap();
            let request = Request::builder()
                .method(method)
                .uri(meta.path.as_str())
                .body(Body::empty())
                .unwrap();
            let status = app.clone().oneshot(request).await.unwrap().status();
            if status == axum::http::StatusCode::NOT_FOUND {
                failures.push(format!(
                    "{} {} -> 404 (not registered)",
                    meta.method, meta.path
                ));
            } else if status == axum::http::StatusCode::METHOD_NOT_ALLOWED {
                failures.push(format!(
                    "{} {} -> 405 (method not accepted)",
                    meta.method, meta.path
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "routes not registered on the router:\n{}",
            failures.join("\n")
        );
    }

    /// The Swagger UI documentation page gets a CSP permissive enough to load
    /// its same-origin JS/CSS and fetch `/openapi.json`, while every other
    /// route keeps the strict, network-restricted policy.
    #[tokio::test]
    async fn swagger_ui_csp_does_not_weaken_other_routes() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::server::create_router(crate::server::AppState::for_tests());

        async fn csp_of(app: axum::Router, path: &str) -> String {
            let request = Request::builder().uri(path).body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            response
                .headers()
                .get("content-security-policy")
                .and_then(|v| v.to_str().ok())
                .expect("CSP present")
                .to_string()
        }

        let swagger_csp = csp_of(app.clone(), "/swagger-ui/").await;
        assert!(
            swagger_csp.contains("script-src 'self'"),
            "swagger-ui CSP must allow same-origin scripts, got: {swagger_csp}"
        );
        assert!(
            swagger_csp.contains("connect-src 'self'"),
            "swagger-ui CSP must allow same-origin fetches, got: {swagger_csp}"
        );

        for path in ["/health", "/api/v1/me", "/"] {
            let csp = csp_of(app.clone(), path).await;
            assert!(
                csp.starts_with("default-src 'none'"),
                "route {path} must keep the strict CSP, got: {csp}"
            );
        }
    }

    /// JSON operations declare request bodies, success response schemas,
    /// the Idempotency-Key header where required, and list pagination inputs.
    #[test]
    fn operations_declare_bodies_schemas_headers_and_pagination() {
        let doc = manager_openapi();
        let paths = &doc.paths.paths;

        let op = |m: &str, p: &str| {
            let pi = paths.get(p).unwrap_or_else(|| panic!("missing {p}"));
            match m {
                "POST" => pi.post.as_ref(),
                "DELETE" => pi.delete.as_ref(),
                _ => None,
            }
            .expect("operation exists")
        };

        // Request body for POST /organizations.
        let create_org = op("POST", "/api/v1/organizations");
        let schema_ref = create_org
            .request_body
            .as_ref()
            .and_then(|body| match body {
                RefOr::T(rb) => rb.content.values().next().and_then(|c| match c {
                    RefOr::T(c) => c.schema.clone(),
                    RefOr::Ref(_) => None,
                }),
                RefOr::Ref(_) => None,
            })
            .expect("request body content schema present");
        let ref_name = match schema_ref {
            RefOr::Ref(r) => r.ref_location.clone(),
            _ => panic!("expected a schema ref"),
        };
        assert!(ref_name.contains("CreateOrgRequest"), "got {ref_name}");

        // Idempotency-Key header on DELETE /organizations/{org}.
        let del_org = op("DELETE", "/api/v1/organizations/{org}");
        let headers = del_org.parameters.as_ref().unwrap();
        assert!(
            headers
                .iter()
                .any(|p| matches!(p, RefOr::T(p) if p.name == "Idempotency-Key")),
            "DELETE /organizations/{{org}} must declare Idempotency-Key"
        );

        // Pagination inputs on the organization list GET.
        let list_orgs = paths
            .get("/api/v1/organizations")
            .and_then(|pi| pi.get.as_ref())
            .expect("GET /organizations");
        let q = list_orgs.parameters.as_ref().unwrap();
        for name in ["limit", "after"] {
            assert!(
                q.iter().any(|p| matches!(p, RefOr::T(p) if p.name == name)),
                "GET /organizations must declare {name}"
            );
        }

        // Success response body schema on GET /organizations/{org}.
        let get_org = paths
            .get("/api/v1/organizations/{org}")
            .and_then(|pi| pi.get.as_ref())
            .expect("GET /organizations/{org}");
        let resp = get_org
            .responses
            .responses
            .get("200")
            .expect("200 response");
        let content = match resp {
            RefOr::T(r) => r.content.get("application/json"),
            RefOr::Ref(_) => None,
        };
        assert!(
            content.is_some(),
            "GET /organizations/{{org}} success response must declare JSON content"
        );
    }
}
