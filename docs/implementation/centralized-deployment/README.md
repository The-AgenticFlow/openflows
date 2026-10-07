# Centralized deployment implementation specifications

Date: 2026-10-07. Status: implementation specification, not shipped behavior.

## Read order

1. This document: decisions, boundaries, shared contracts.
2. [User management](01-user-management.md): identity, schema, authorization, sessions, CLI.
3. [GitHub integration](02-github-app.md): admin installation, authority verification, credentials, webhooks.
4. [Template provisioning](03-template-provisioning.md): Coder organizations, releases, workspaces, runtime isolation.
5. [Implementation backlog](04-implementation-plan.md): ordered work packages and release gates.
6. [Model handoff](05-model-handoff.md): prompt to start implementation.

These specifications supersede ambiguous details in the [initial deployment plan](../../architecture/centralized-deployment-plan.md). Requirements marked MUST are acceptance requirements. Proposed technical defaults below may change only with a documented reason, corresponding contract updates, and equivalent acceptance coverage. Do not silently weaken authorization to accommodate an upstream limitation.

## Confirmed product decisions

- Use Coder Premium. One Openflows organization maps to exactly one Coder organization in the shared deployment.
- Customers see Openflows organizations. Openflows owns product membership, repository tenant bindings, settings, and usage policies. Coder owns infrastructure resources.
- Only active Openflows organization admins manage GitHub installation connections. Developers and viewers never install or connect the App as part of onboarding.
- One centrally owned GitHub App serves all customers. Each installation is bound to at most one Openflows organization. An organization may bind multiple installations.
- A tenant is one repository automation environment, with its own Nexus and worker fleet. For v1, allow one non-deleted tenant per repository per Openflows organization.
- Preserve the existing agent flow, planning gates, and CLI tenant workflow. Extend their transport and identity boundaries.

## Detailed design decisions

- Extend the existing Axum `openflows-manager` as the product API, credential broker, and durable provisioning coordinator. Do not build a second competing control plane.
- Use PostgreSQL with SQLx migrations for product metadata and operation records. Use a separate database and credentials from Coder's database; never modify Coder tables directly.
- Use GitHub App user authorization for human sign-in in v1. This does not require an App installation and does not grant Openflows membership. Keep provider identity behind an interface for later OIDC.
- The legacy orchestration `registry.json` is not part of hosted provisioning or template releases. Replace it with typed Manager-owned release and role configuration; remove its hosted parameter, persistence, and runtime dependency during migration.
- Membership roles are `admin`, `developer`, `viewer`. Ownership is a separate `organizations.owner_user_id`, not a fourth exclusive role. The creator becomes owner AND admin atomically. Ownership alone never bypasses the admin-only GitHub rule.
- Human Coder accounts are out of scope for v1. Tenant machine identities own workspaces. No customer receives platform Coder credentials.
- CLI requests use Openflows credentials. Hosted CLI commands never receive Redis or operator Coder credentials.
- Retain explicit local mode for existing operator workflows. Hosted errors MUST NOT fall back to local bootstrap.
- Use UUIDs for Openflows IDs, signed 64-bit integers for GitHub IDs, opaque strings for upstream Coder IDs, UTC timestamps, and immutable external IDs for authorization.

## Existing code and required changes

| Current entry point | Observed behavior | Required direction |
|---|---|---|
| `crates/openflows-manager/src/server.rs` | Axum state holds one tenant-scoped Redis store | Add database and services; resolve tenant scope after authorization per request |
| `crates/openflows-manager/src/routes/mod.rs` | Health/readiness and API index only | Add versioned identity, organization, installation, tenant, runtime, operation routes |
| `binary/src/bin/agentflow.rs` | CLI root; `run_tenant` bootstraps Coder directly and persists the legacy agent `registry.json` | Add hosted API client; preserve explicit local path only during migration; hosted provisioning must remove this file and parameter dependency |
| `crates/coder-client/src/bootstrap.rs` | Publishes bundled templates; uses session owner and passes session token to Nexus | Separate platform bootstrap from customer onboarding; remove global credentials from hosted templates |
| `crates/coder-client/src/lib.rs` | Template lookup by name, default-org cache, workspace creation | Explicit organization, template/version, owner IDs; validate returned resource ownership |
| `crates/agent-nexus/src/lib.rs` | Creates workers directly; default-org model/chat calls | Hosted provisioning through manager; explicit organization for every remaining Coder operation |
| `crates/github/src/rest.rs` | Client stores a static token string | Async credential provider with expiry and bounded refresh |
| `crates/config/src/identity.rs` | Agent identity includes resolved GitHub token | Hosted identity carries a credential provider/reference, not permanent env-token assumptions |
| `crates/coder-client/templates/*/main.tf` | GitHub external-auth interpolation; Nexus Coder and hook secrets | Hosted credential retrieval and deployment-specific isolation |
| `crates/coder-client/build.rs` | Packages template archives during build | Deterministic release packaging and manifest hashing |

Repository paths above are relative to the repository root. Inspect current code before editing; these are integration targets, not instructions to replace entire modules.

## Shared API conventions

All product routes use `/api/v1`. Use `Authorization: Bearer <opaque Openflows access token>` for CLI calls. Browser sessions use Secure, HttpOnly, SameSite=Lax cookies and CSRF protection on mutations. Runtime authentication is a separate credential audience and cannot call human administration routes.

- `401`: missing, expired, or revoked authentication.
- `403`: authenticated member lacks an action permission.
- `404`: organization/resource is absent or outside the caller's membership; do not enumerate another customer's IDs.
- `409`: uniqueness conflict, invalid transition, ownership conflict, last-admin violation, or mismatched idempotency request.
- `422`: invalid input. `429`: quota/rate limit, with retry guidance. `503`: unavailable dependency or credential validation required but unavailable.

Error envelope: `{ "error": { "code": "ORG_ADMIN_REQUIRED", "message": "An organization admin must connect GitHub.", "request_id": "...", "retryable": false } }`. Never include credentials, raw upstream bodies, or other customers' resource details.

Lists use opaque cursors and `limit` (default 50, maximum 100), returning `{ "items": [], "next_cursor": null }`.

Mutations creating resources or durable work require `Idempotency-Key`. Persist `(actor_id, org_id, route, key, request_hash, response_reference)` with a unique constraint. Same key and body returns the original result; a different body returns 409. Retain records at least 7 days. Independently enforce resource uniqueness after retention expires.

Provisioning returns `202 { "operation_id": "...", "resource_id": "...", "status": "queued" }`. Operations expose sanitized progress via `GET /operations/{id}` after resource-scoped authorization. Never hold a database transaction while waiting on an upstream network call.

## Shared persistence and event rules

Every business table has `created_at`, `updated_at` where appropriate. Preserve audit history on soft deletion. Secrets are hashed if only verification is needed, encrypted through a key-management abstraction if recovery is required; store key version alongside ciphertext. Hash high-entropy random tokens using SHA-256; do not use this scheme for passwords (v1 has no passwords).

Audit records include organization, actor type/ID, action, resource type/ID, result, request ID, and timestamp. Allowlist metadata. Record auth failures without leaking secret-bearing input. Insert audit and outbox events in the same transaction as state changes. Workers lease durable outbox/operation rows with expiry and bounded retries.

PostgreSQL is authoritative for permission checks. Tenant slugs and `X-Org` headers are selectors only. Every repository, workspace, operation, and runtime lookup must be joined to its organization or authenticated machine principal before use.

## External compatibility gate

The checked-in Compose default is Coder `v2.37.3`. Current online documentation can describe later features. Before implementing adapters, run contract tests against that exact version and the licensed deployment: organization creation, service-account ownership, template version pinning, provisioner assignment, scoped credentials, and Coder Agents/chat permissions. Record the supported version and fixtures. If an upgrade is necessary, propose and test it explicitly; never silently substitute default-organization resources or global tokens.

No public deployment is complete until the two-organization acceptance suite in the backlog passes.
