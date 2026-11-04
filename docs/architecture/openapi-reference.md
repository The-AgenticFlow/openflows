# OpenFlows HTTP API Reference (OpenAPI)

**Document type:** Internal architecture + developer reference
**Scope:** The generated OpenAPI 3.1 documents for the OpenFlows Manager and Agent Nexus HTTP surfaces, how they are produced, and how to use them.
**Companion docs:** `openflows-system-architecture.md` (system-wide), `openflows-controller.md` (host process), `manager-hosted-runtime-architecture.md` (Manager control plane).

---

## 1. What this is

OpenFlows exposes several HTTP surfaces. Each now serves an OpenAPI 3.1 document generated with [`utoipa`](https://github.com/juhaku/utoipa) and a browsable reference UI, so a developer or operator can learn how to call an endpoint without reading its handler source.

| Surface | Router (source of truth) | OpenAPI endpoint | UI endpoint | Document module |
|---|---|---|---|---|
| Manager API | `crates/openflows-manager/src/routes/mod.rs` | `/openapi.json` | `/swagger-ui/` | `crates/openflows-manager/src/openapi.rs` |
| Agent Nexus — A2A relay | `crates/agent-nexus/src/a2a/http_server.rs` | `/openapi.json` | — | `crates/agent-nexus/src/openapi.rs` |
| Agent Nexus — lifecycle hooks | `crates/agent-nexus/src/hooks/server.rs` | `/openapi.json` | — | `crates/agent-nexus/src/openapi.rs` |

The Agent Nexus surfaces get their **own** document rather than being folded into the Manager's because they are protocol-specific and cannot be represented cleanly as REST: the A2A relay is a JSON-RPC endpoint (`POST /rpc`) and the hooks consumer is a JWT-signed webhook. The Manager document covers the versioned `/api/v1` JSON API plus browser, callback, webhook, and health routes.

> **Security:** the documentation endpoints are public and the documents contain **no credentials or runtime tokens**. The shared `ApiError` envelope and example schemas never embed real secrets. Runtime routes are documented with a `bearerAuth` security requirement that describes *how* to authenticate, without leaking any key material.

---

## 2. How the document is generated

The spec is **derived from a single route inventory**, not a second handwritten endpoint list. In `crates/openflows-manager/src/openapi.rs` the constant [`MANAGER_ROUTES`] and, in `crates/agent-nexus/src/openapi.rs`, [`NEXUS_ROUTES`] record every route with:

- HTTP method and **full path** (including the `/api/v1` prefix for the Manager),
- a functional tag used to group operations in the UI,
- a one-line summary,
- an authentication/visibility **class** (public, user-auth, runtime, webhook, callback, browser, health).

`utoipa`'s builders turn that inventory into an `OpenApi` document at runtime. Because the inventory lives right next to the routers that register the same routes, there is a single place to keep in sync.

## 3. Route-to-spec coverage check

To prevent a newly added HTTP route from silently staying undocumented, each document module ships a **coverage test**:

- `openapi::tests::every_inventoried_route_is_documented` — every entry in the inventory appears in the generated OpenAPI with the correct method and path.
- `openapi::tests::document_serializes_to_json` — the document serializes to valid JSON with the expected top-level structure.
- `openapi::tests::documented_routes_are_registered_on_the_router` (Manager) / `..._on_their_routers` (Agent Nexus) — probes the **real** router(s) for every inventoried `(method, path)` and requires a non-404, non-405 response, proving the documented route is actually registered.

Run them with:

```sh
cargo test -p openflows-manager --lib openapi::
cargo test -p agent-nexus --lib openapi::
```

If you add a route to `routes/mod.rs` (Manager) or to the A2A/hooks routers (Agent Nexus), add the matching entry to the inventory or these tests fail.

## 4. Regenerating and using the spec

The document is generated **at runtime** by the running service; there is no checked-in spec file to regenerate by hand. To capture a static copy for tooling or code generation:

```sh
# Manager
curl -s http://localhost:7080/openapi.json > manager-openapi.json

# Agent Nexus A2A relay / hooks consumer
curl -s http://localhost:3000/openapi.json  > nexus-openapi.json
curl -s http://localhost:3001/openapi.json > nexus-hooks-openapi.json
```

Validate a captured copy with any OpenAPI 3.1 validator, for example:

```sh
npx --yes @apidevtools/swagger-cli validate manager-openapi.json
```

To browse interactively, open the Manager's Swagger UI at `/swagger-ui/` (it loads the spec from `/openapi.json`). The reference describes, for each operation, the method, full path, JSON request/response body schemas, the shared error envelope, and — where applicable — the `bearerAuth` security requirement.

## 5. Representative schemas

The documents include reusable component schemas for the most-used shapes:

- **Manager** (`ApiError`, `User`, `Organization`, `Tenant`, `Operation`) — the shared error envelope `ApiError` is referenced on every JSON operation's 4xx/5xx responses; the DTO schemas mirror `crates/openflows-manager/src/dto.rs`.
- **Agent Nexus** (`JsonRpcRequest`, `JsonRpcResponse`, `VerifyRequest`, `VerifyResult`, `HookDecision`) — the A2A JSON-RPC envelope and `verify` task payloads (mirroring `crates/a2a-protocol/src/verify.rs`), plus Coder's lifecycle-hook response schema.

These are intentionally "representative": the goal is an accurate, navigable contract for the endpoints, not a byte-for-byte transcription of every internal struct. When an operation's body is not schema-mapped, it is still fully documented at the method/path/summary level so it can be discovered and called.
