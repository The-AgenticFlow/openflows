# OpenFlows Manager API research for issues #283 and #284

Primary issue sources: [#283](https://github.com/The-AgenticFlow/openflows/issues/283) and [#284](https://github.com/The-AgenticFlow/openflows/issues/284).

## Target architecture

Issue #283 introduces `openflows-manager` as the trusted Rust backend for `openflows-console`: Browser -> Console -> Manager -> shared OpenFlows Rust services -> Coder / Redis / GitHub. The proposed crate is `crates/openflows-manager/`, with versioned APIs under `/api/v1/...`, and the epic scope covers Fleet, Tenants, Kanban, GitHub-auth architecture investigation, and AI provider/model management. Issue #284 is the first slice: create the workspace crate, async HTTP server, health/readiness endpoint, `/api/v1` router structure, shared app state/dependency wiring for Redis/OpenFlows services and future Coder access, graceful shutdown, tests, and no secret-bearing responses or logs.

The repository already has the control-plane model that Manager should expose rather than duplicate. The controller is documented as the single long-lived control process that provisions workspaces, creates Coder chats, coordinates role nodes, writes SharedStore state, and hosts the A2A relay (`docs/architecture/openflows-controller.md`). Its boot path loads centralized config, validates controller-required variables, opens a tenant-scoped Redis `SharedStore`, starts Axum HTTP services, writes registry state, and builds the PocketFlow graph in `binary/src/bin/agentflow.rs`. Manager should become a product-facing HTTP boundary over the same domain services, not a second orchestration engine.

## Why Console should call Manager

Console should call Manager instead of Redis or Coder directly because #283 makes the Console a UI/client and assigns privileged infrastructure access to Manager. That matches the existing design: the controller is the only OpenFlows component intended to talk to the Coder control-plane API and the only normal writer to SharedStore, aside from the worker harness surface (`docs/architecture/openflows-controller.md`). Sending browser code directly to Redis would expose internal key schema such as `tickets`, `worker_slots`, `pending_prs`, `ticket:{id}:status`, `ticket:{id}:chat:{role}`, and tenant-prefixed Redis keys; Manager can normalize those into Fleet/Kanban resources without coupling the frontend to Redis layout (`docs/architecture/openflows-controller.md`, `crates/pocketflow-core/src/store.rs`).

Direct Coder access has the same problem at a sharper privilege boundary. `CoderClient` owns workspace CRUD, chat lifecycle, organization/model lookup, and bootstrap/admin APIs (`crates/coder-client/src/lib.rs`, `crates/coder-client/src/bootstrap.rs`). #283 explicitly says Redis/Coder credentials and internal secrets never reach the browser, and normal users should not need the Coder UI. Manager is therefore the authorization, redaction, tenant-scoping, API-versioning, and product-policy layer between browser actions and Coder/Redis/GitHub.

## Why #284 is the first slice

#284 is first because every later #283 child needs the same process shell: a crate in the Cargo workspace, an async HTTP server, route versioning, health/readiness, shared state, dependency injection, graceful shutdown, and safe response/error conventions. Fleet (#285), Tenants (#286), Kanban (#287), hardening (#288), GitHub architecture (#289), and AI provider/model policy (#290) all depend on an HTTP host and app-state boundary. A scaffold-only slice also lets the team settle reusable patterns and tests before exposing privileged Fleet/Tenant/Kanban operations.

Keep #284 intentionally thin: it should wire the server and dependencies, prove startup/health in tests, and establish response hygiene. It should not implement Fleet, Tenant lifecycle, Kanban, GitHub onboarding, or model/provider setup, because those are explicit non-goals in #284 and separate child tickets under #283.

## Repo components to reuse

- `crates/config/src/env.rs`: centralized startup config via `EnvConfig`, with `CoderConfig`, `InfraConfig`, `TenantConfig`, `GithubConfig`, and redacted `Debug` output for secrets. Manager should load config once, reuse `effective_redis_url()` and explicit tenant handling, and preserve redaction behavior.
- `crates/pocketflow-core/src/store.rs`: `SharedStore` already provides Redis/in-memory backends, typed `get_typed`/`set_typed`, event access, and tenant key namespacing as `ns:{tenant}:{key}`. Manager should use this rather than raw Redis calls.
- `crates/coder-client/src/lib.rs` and `crates/coder-client/src/bootstrap.rs`: Coder workspace/chat/model/bootstrap logic already exists behind `CoderClient` and `CoderBootstrapper`. Manager should reuse these for future Coder-backed APIs rather than introducing another Coder REST client.
- `crates/agent-nexus/src/a2a/http_server.rs`, `crates/agent-nexus/src/a2a/mod.rs`, and `crates/agent-nexus/src/hooks/server.rs`: existing Axum patterns for `Router`, shared `State`, JSON responses, health endpoints, background `tokio::spawn` servers, and structured internal-error responses. Manager can copy the pattern, while adding #284's graceful shutdown requirement.
- `docs/architecture/openflows-controller.md` and `binary/src/bin/agentflow.rs`: authoritative behavior for fleet state, role graph, recovery, Redis keys, control modes, and controller boot wiring. Manager API resources should reflect these domain facts instead of inventing parallel semantics.
- `docs/env-config-audit.md`: current environment inventory and secret/auth decisions, including removed PAT flow, required `CODER_SESSION_TOKEN`, `REDIS_URL`, `OPENFLOWS_TENANT`, runtime-injected `GITHUB_REPOSITORY`, and external-auth token handling.

## Risks, secrets, and tenant scoping

Secrets must not cross the Manager/Console boundary. The repo already redacts Coder session token, admin password, external-auth client secret, and Nexus API token in config debug output (`crates/config/src/env.rs`). The Manager should treat `CODER_SESSION_TOKEN`, Coder admin credentials, Redis URL/password material, GitHub external-auth tokens, hook secrets, and notifier webhooks as server-only values. It should also avoid logging request/response bodies that might contain these values.

Tenant scoping is a hard constraint. `SharedStore::new_redis_with_tenant` bakes the tenant into every operation and prefixes keys with `ns:{tenant}:...`; raw scan/delete intentionally bypass namespacing and are documented for admin/CLI use only (`crates/pocketflow-core/src/store.rs`, `docs/architecture/openflows-controller.md`). Manager endpoints that read Fleet/Kanban/Tenant state must derive or authorize a tenant before opening the store and must not expose raw Redis key operations to normal Console users.

AuthZ should not inherit internal trust assumptions blindly. The A2A relay currently notes v1 best-effort checks based on Docker-network trust and self-declared `pair_id`, with TODOs for workspace-identity-backed ownership (`crates/agent-nexus/src/a2a/http_server.rs`). A browser-facing Manager needs stronger session/user/tenant authorization from the start, especially before Tenant lifecycle or control-plane mutation APIs are exposed.

GitHub auth is also intentionally unsettled. #283 puts GitHub onboarding/repository authentication into investigative child #289, and current repo docs say the PAT flow has been removed in favor of tenant user's Coder external-auth token and runtime-injected repository identity (`docs/env-config-audit.md`, `docs/architecture/openflows-controller.md`, `crates/config/src/env.rs`). Manager should not add a new production GitHub secret path before #289 defines it.

Finally, Coder remains the runtime backend. #283 explicitly keeps Coder as the workspace/runtime and underlying AI provider/model backend while OpenFlows owns the product-facing journey and model-assignment policy. Manager should wrap and constrain Coder operations through OpenFlows policy rather than exposing an open-ended Coder proxy.
