# Centralized Deployment — Compatibility and Integration Contract (WP-00)

**Status:** Investigation report; executable contract-verification harness delivered. Live checks are gated on a licensed Coder deployment and are **NOT_VERIFIED** where none was configured.
**Date:** 2026-10-07
**Scope:** WP-00 of the [centralized deployment roadmap](README.md). This PR inventories the current Openflows/Coder integration, establishes the Coder compatibility baseline, delivers a reproducible verification harness, and records open decisions. It does **not** implement the product database, authentication, tenant provisioning, or production infrastructure.

## 1. Executive findings

1. **One Openflows org → one Coder org is architecturally sound but not yet expressible in the current code.** Every customer-facing Coder operation resolves the **default** organization or the **current user**, not an explicit mapped organization. See the integration inventory (§2).
2. **Coder organizations and machine identities require a Premium license.** The official docs are explicit: "Organizations requires a Premium license" and service accounts (machine identities) are a Premium feature. This is an operator/licensing prerequisite, not a code change. **No licensed Coder deployment was configured for this PR**, so all live checks are reported `NOT_VERIFIED` and the next segment is blocked on obtaining one.
3. **Version drift exists across the repository.** The checked-in Compose default is Coder `v2.37.3` (`docker-compose.yml:29`), but `crates/config/src/env.rs:43` defaults `CODER_IMAGE_TAG` to `v2.37.1`, and the migration doc references `v2.37.0`. The specs (`README.md:82`) pin the compatibility baseline at **v2.37.3**. This must be reconciled before adapters are written.
4. **The current template publication path cannot pin versions.** `CoderClient::push_template` (`crates/coder-client/src/lib.rs:654`) shells out to `coder templates push` with **no `--name`/version** and claims "Coder v2 doesn't support template version updates via REST". The CLI does support named versions and explicit `--org`/`--template-version` (verified against source); the client simply does not use them.
5. **Workspace creation cannot express the required tenant boundaries.** `create_workspace_for_user` (`lib.rs:791`) posts `{template_id, name, rich_parameter_values}` to `POST /api/v2/users/{user}/workspaces` with no `organization_id` and no `template_version_id`. `template_id` is resolved by name from the token's default-org template list (`find_template_id_by_name`). The specs require explicit org, owner, and pinned version.
6. **Platform and cross-tenant credentials are embedded in workspace runtimes.** The Nexus template (`crates/coder-client/templates/openflows-nexus/main.tf`) receives the **admin's Coder session token** (`coder_session_token`), the **global hook signing secret** (`coder_chat_hook_secret`), the **legacy `registry_json`**, and the owner's GitHub external-auth token. The Forge template writes the owner's GitHub token to `.git-credentials`. These violate the fixed product decision that "customer runtimes must never receive platform administrator credentials."
7. **The legacy `registry.json` is load-bearing today.** It is persisted to disk, propagated via env and Redis, and passed into the Nexus workspace. It must be removed via a planned migration; this PR only inventories it (§6) and adds no new dependency on it.

## 2. Current integration inventory

Paths are relative to the repository root. "Assumption that blocks centralized deployment" flags each risk.

### 2.1 Coder organization selection

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| `CoderClient::list_organizations` | `crates/coder-client/src/lib.rs:500` | `GET /api/v2/organizations` | No (read-only) |
| `CoderClient::get_default_organization_id` | `lib.rs:522` | Caches the **default** org (`is_default`, else first). Used for model and chat scoping. | **Yes** — resolves the operator's default org, not the customer's mapped org. Cross-tenant model/chat resolution is impossible through this path. |
| `AgentIdentity`/Nexus default-org calls | `crates/agent-nexus/src/lib.rs:1491,2173,2422,2759` | All chat creation resolves `get_default_organization_id()`. | **Yes** — every customer chat would land in one org. |

### 2.2 Template packaging, publication, workspace creation

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| Template archives embedded | `crates/coder-client/src/bootstrap.rs:224-273` (`include_bytes!("../templates/*.tar.gz")`) | 5 bundled role templates (forge, sentinel, nexus, vessel, lore) shipped as build-time archives. | Partially — no release manifest/version identity. |
| `CoderClient::push_template` | `lib.rs:654` | Writes archive to temp dir, shells out to `coder templates push` (no `--name`, no `--org`). Comment claims version updates unavailable via REST. | **Yes** — cannot publish into a customer org nor pin a version. |
| `list_templates` / `find_template_id_by_name` | `lib.rs:748` / `862` | `GET /api/v2/templates`, then resolves by name. | **Yes** — name-based, default-org-scoped; ambiguous across orgs. |
| `create_workspace` / `create_workspace_for_user` | `lib.rs:781` / `791` | `POST /api/v2/users/{user}/workspaces` body `{template_id, name, rich_parameter_values}`. | **Yes** — no `organization_id`, no `template_version_id`, owner is the URL-path user. |
| Import-completion detection | `lib.rs:654` | Infers success from `coder templates push` exit code + re-list. | Yes — spec §3.3 requires polling the import job and matching version metadata, not just exit code. |

### 2.3 Workspace ownership and Coder service credentials

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| `CoderBootstrapper::ensure_tenant` | `crates/coder-client/src/bootstrap.rs:342` | Creates the Nexus workspace **under the session user (admin)** via `create_workspace_for_user(&admin.id, …)`. Passes `coder_session_token = client.session_token()` into workspace parameters. | **Yes** — the tenant owner is the human admin, not a machine identity; the admin's session token is copied into the workspace. |
| `create_api_token` | `lib.rs:568` | Creates 168h API tokens for a user; used by bootstrap for the admin. | **Yes** — long-lived operator-scoped token; no tenant machine token. |
| Nexus template `coder_session_token` | `crates/coder-client/templates/openflows-nexus/main.tf:25-30,188,255` | `CODER_SESSION_TOKEN` is set from the workspace parameter and exported into the Nexus container. | **Yes** — platform credential reaches the runtime. |

### 2.4 Nexus worker provisioning and chat/model access

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| `Nexus::create_worker_workspace` | `crates/agent-nexus/src/lib.rs:900-992` | Builds `CreateWorkspaceRequest` and calls `client.create_workspace(&request)` (current user). | **Yes** — workers created under Nexus's token, not per-tenant machine identity. |
| `create_chat_for_assignment` / `create_chat_for_ticket_id` | `lib.rs:1268,1783` | Resolve default org and call `create_chat`. | **Yes** — chat ownership is default-org. |
| `list_chat_models` | `crates/coder-client/src/lib.rs:1603/1645` | `GET /api/v2/organizations/{default}/chats/models`. | **Yes** — org-scoped endpoint but always the default org. |
| `create_chat` | `lib.rs:1365` | `POST /api/v2/chats` with `organization_id` from the request. | Partial — `CreateChatRequest.organization_id` is `Option`; callers pass the default org. |

### 2.5 GitHub credentials and consumers

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| `GithubRestClient` token storage | `crates/github/src/rest.rs:26` | `token: String` (static, no expiry/refresh). | **Yes** — the specs require an async credential provider with expiry and bounded refresh. |
| `GithubRestClient::new` | `rest.rs:31` | Takes a plain token string. | Yes — construction is synchronous/static. |
| Global token resolution | `crates/config/src/registry.rs:487` | `resolve_global_github_token` reads `CODER_EXTERNAL_AUTH_*_TOKEN` / `GITHUB_TOKEN`. | **Yes** — one global token, not per-tenant installation tokens. |
| Consumers | `crates/agent-vessel/src/node.rs:66`, `crates/agent-nexus/src/lib.rs:286`, `crates/agent-forge/src/lib.rs:59` | Each builds a client from a static resolved token. | Yes — no broker, no refresh. |

### 2.6 Lifecycle hook authentication and routing

| Symbol | Location | Behavior | Blocks centralized deployment? |
|---|---|---|---|
| Hook dispatch | `docker-compose.yml:69` | `CODER_CHAT_HOOK_URL=http://openflows-nexus:3001/experimental/hooks/chat`; `CODER_CHAT_HOOK_SECRET` global. | **Yes** — hook URL points at the single Nexus service. |
| Hook JWT verify | `crates/agent-nexus/src/hooks/server.rs:131`, `jwt.rs:58` | Verifies HS256 against the **shared** `CODER_CHAT_HOOK_SECRET` and `aud == CODER_CHAT_HOOK_URL`. | **Yes** — the global hook secret is distributed to every workspace (Nexus template). Spec §6 requires moving verification into the manager ingress with tenant-scoped signing keys. |
| Hook secret in workspace | `crates/coder-client/templates/openflows-nexus/main.tf:193,260` | `CODER_CHAT_HOOK_SECRET` exported into the Nexus container. | **Yes** — global signing secret present in a customer runtime. |

### 2.7 `registry.json` loading, generation, propagation, runtime use

| Symbol | Location | Behavior |
|---|---|---|
| Source of truth | `orchestration/agent/registry.json` | Legacy role/fleet config (team, slots, CLI, modules). |
| Load | `crates/config/src/registry.rs` (`Registry::load`, `RegistryEntry`) | Parsed; `deny_unknown_fields`. |
| Generation/persistence | `binary/src/bin/agentflow.rs:314-337` | Writes `OPENFLOWS_REGISTRY_JSON` to the registry path file, sets it as env, stores it in Redis under `registry_json`. |
| Propagation to runtime | `crates/coder-client/src/bootstrap.rs:504` | `registry_json` passed as a Nexus workspace parameter. |
| Runtime consumers | `crates/agent-nexus/src/lib.rs:300-326,754,928,1131,5310`; `crates/config/src/identity.rs` | Worker fleet (`sync_registry`), worker template CLI selection, token resolution. |

**Removal impact:** §6 inventories the consumers. This PR adds no new dependency on `registry.json`.

## 3. Version and capability baseline

### 3.1 Configured versions

| Component | Configured value | Source |
|---|---|---|
| Coder server (Compose default) | **v2.37.3** | `docker-compose.yml:29` (`${CODER_IMAGE_TAG:-v2.37.3}`) |
| Coder server (config default) | **v2.37.1** | `crates/config/src/env.rs:43` — **drift vs Compose** |
| Coder server (migration doc) | v2.37.0 | `docs/architecture/coder-v2.37-migration.md:29,77-80` |
| Terraform provider | `coder/coder ~> 2.18.0` | all five `crates/coder-client/templates/*/main.tf:3` |
| Coder CLI | not pinned; `resolve_coder_cli()` requires it on PATH | `crates/coder-client/src/lib.rs:1836` |

**Baseline chosen for WP-00:** Coder **v2.37.3** (the checked-in Compose default and the value named by the specs). The `env.rs` v2.37.1 default is a drift to reconcile before WP-05. `coder` CLI is not pinned; the harness assumes whatever `coder` binary is on PATH and records its version.

### 3.2 Capability matrix

Statuses: **VERIFIED_LIVE** (exercised against the stated version), **VERIFIED_SOURCE** (confirmed from version-matched source/documentation), **NOT_VERIFIED** (assumption or unavailable evidence), **UNSUPPORTED** (confirmed unavailable).

Live checks are `NOT_VERIFIED` because **no licensed Coder deployment was configured** for this PR. Source checks are grounded in the pinned v2.37 line's official CLI/API reference and the repository's own v2.37 migration record.

| # | Capability | API method/path or CLI | Permissions / license | Minimal request / response | Expected failure | Status | Evidence |
|---|---|---|---|---|---|---|---|
| C1 | Verify server version | `GET /api/v2/buildinfo` (unauthenticated) | none | resp `version` | non-200 | **NOT_VERIFIED** (live); harness `ro_buildinfo_matches_pinned_version` | Compose `healthcheck` hits this path (`docker-compose.yml:86`); harness |
| C2 | List organizations | `GET /api/v2/organizations` | member | `[{id,name,is_default}]` | 401/403 | **NOT_VERIFIED** (live); source confirmed | `CoderClient::list_organizations` `lib.rs:500` |
| C3 | Create organization | `coder organizations create <name>` | **Owner** role; **Premium** license | name; returns org | 403 if not Owner | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | [docs/admin/users/organizations](https://coder.com/docs/admin/users/organizations) "requires a Premium license"; "User with Owner role"; harness `cap_create_organization` |
| C4 | Register org-scoped provisioner | `POST /api/v2/organizations/{org}/provisionerkeys` → `{"key":"<43-char secret>"}`; `coder provisionerd start --org <org>`; readiness at `GET /organizations/{org}/provisionerdaemons` | org admin; Enterprise (key API) | create returns raw 43-char secret (no `coder_` prefix); daemon listing reports `key_name` + `status` | 403 without org perms | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | `provisionerkey.New` (`apikey.GenerateSecret(43)`); `provisionerdaemons` route in `coderd.go`; AGPL CLI exposes no `provisioner keys create`; harness `create_provisioner_key_api`/`wait_for_provisioner_ready` |
| C5 | Create machine identity (service account) | `coder users create --service-account --org <org>` | **Premium** | username/email; returns user | 403 without license | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | [docs/reference/cli/users/create](https://coder.com/docs/reference/cli/users/create) `--service-account` (Premium); harness `cap_create_machine_identity_and_assign_permissions` |
| C6 | Assign permissions to identity | `coder organizations members edit-roles <u> <roles...>` (org-scoped via `CODER_ORGANIZATION`); `default_org_member_roles` | org admin; Premium | role names | rejected invalid role | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | `coder organizations members edit-roles` (v2.37.3); docs "Service accounts don't inherit `agents-access`… assign the role directly"; harness `cap_create_machine_identity_and_assign_permissions` |
| C7 | Publish template + observe import | `coder templates push --org <org> --name <version> -d <dir>` | org template admin | version name; import job | import failure | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | [docs/reference/cli/templates/push](https://coder.com/docs/reference/cli/templates/push) `--name`, `--org`; harness `cap_publish_template_version_and_observe_import` |
| C8 | Create workspace with explicit org/owner/version | `coder create <owner>/<ws> --org <org> --template <t> --template-version <v>` | owner permission; member | org, template, version | 403 wrong org | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | [docs/reference/cli/create](https://coder.com/docs/reference/cli/create) `--org`, `--template-version`, `<username>/<name>`; harness `cap_create_workspace_explicit_org_owner_version` |
| C9 | Agents/chat: list models org-scoped | `GET /api/v2/organizations/{org}/chats/models` | member with `agents-access` | `{providers:[{models:[{id}]}]}` | 403 without agents-access | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | `lib.rs:1603/1645`; migration doc §1.2 (default-org routes removed in v2.37); harness `ro_list_chat_models_org_scoped` |
| C10 | Agents/chat: create/get/send | `POST/GET /api/v2/chats…` | member of chat org | `organization_id` required | 403/404 | **VERIFIED_SOURCE**; **NOT_VERIFIED** live | `lib.rs:1365-1589`; migration doc §1.3 |
| C11 | Cross-tenant isolation | token of org A accesses org B resources (both directions: models, workspaces, chats) | scoped machine identity | — | authenticated denial (`403`/`404`, not `401`) both ways; cross-org workspace invisible (empty 200 is correct isolation) | **NOT_VERIFIED** — must be demonstrated with two live machine identities; positive controls (each identity reads its own explicit org + its own workspace fixture + creates a chat in its own org) + both-direction negatives required; **never inferred from names** | harness `iso_cross_tenant` (fixtures from `mut_scenario_provision_tenant` via `--isolation`) |

**Unsupported / gated findings:**

- **Machine identity for workspace ownership is gated on Premium.** If the deployed license lacks it, WP-05 must stop and document an explicit upgrade requirement (spec `03-template-provisioning.md` §1). This is `UNSUPPORTED` only on a non-Premium license, which is not the assumed baseline.
- **`agents-access` is not inherited by service accounts.** Even with a machine identity, chat/model use requires assigning `agents-access` (and `organization-workspace-access`) directly to the service account (C6). This is a concrete, version-matched requirement the manager adapter must enforce.
- **Built-in provisioners are default-org only.** Every additional org needs a dedicated provisioner (C4). The adapter cannot rely on the compose `CODER_PROVISIONER_DAEMONS` built-in daemon for customer orgs.

### 3.3 Sanitized request/response examples

These are the **expected contracts** the harness asserts. Credentials are redacted; none were captured from a live deployment.

**C1 buildinfo (expected):**
```json
{ "version": "v2.37.3", "external_url": "<REDACTED>", "dashboard_url": "<REDACTED>" }
```

**C2 organizations (expected, redacted):**
```json
[ { "id": "<REDACTED>", "name": "coder", "display_name": "Default", "is_default": true } ]
```

**C9 models org-scoped (expected shape, parsed by `parse_chat_models_body` `lib.rs:137`):**
```json
{ "providers": [ { "provider": "openai-compat", "models": [ { "id": "openai-compat:model-x", "model": "model-x" } ] } ] }
```

**Workspace create payload used today (`lib.rs:819`):**
```json
{ "template_id": "<REDACTED>", "name": "openflows-nexus-<tenant>", "rich_parameter_values": [ { "name": "coder_session_token", "value": "<REDACTED>" }, { "name": "coder_chat_hook_secret", "value": "<REDACTED>" }, { "name": "registry_json", "value": "<REDACTED>" } ] }
```

**Required (spec §3.3) — explicit version pinning the client must gain:**
```json
{ "template_id": "<REDACTED>", "template_version_id": "<REDACTED>", "name": "<name>", "organization_id": "<REDACTED>", "rich_parameter_values": [] }
```

## 4. Verification commands and results

### 4.1 Harness

Delivered in this PR:

- `crates/coder-client/tests/compatibility.rs` — Rust integration harness (read-only `ro_*`, ordered mutating scenario `mut_*`, isolation `iso_*`, explicit cleanup `cleanup_*`, plus non-`#[ignore]`d offline regression tests for the parsing/validation helpers).
- `tests/integration/coder_compatibility.sh` — reproducible shell wrapper (mirrors `tests/integration/gated_workflow_test.sh`), bash-3.2 safe, with explicit `--mutating`, `--isolation`, and `--cleanup` modes.

Safety/design notes:
- **Group selection:** tests are named with stable prefixes so the wrapper selects exactly one group (`ro_`, `mut_`, `iso_`, or `cleanup_`). Destructive cleanup **never** runs during a read-only or mutating verification; it runs only via `--cleanup`.
- **Single ordered mutating scenario:** `mut_scenario_provision_tenant` provisions **two** isolated orgs (A and B), each: org → provisioner key (structured API) + running daemon → machine identity + roles → template version → workspace → build waited to completion → token minted. Each workspace is **recorded in the ledger immediately after creation** (before polling), so a failed or timed-out build still leaves the resource tracked for cleanup. Provisioner readiness (identity + `status`) and all subprocesses are bounded by timeouts; `kill_on_drop` prevents orphaned children.
- **Workspace build semantics:** per the pinned API, `latest_build.status` is a `WorkspaceStatus` (a successful *start* reports `running`, never `succeeded`) while `latest_build.job.status` is a `ProvisionerJobStatus` (`succeeded` when the provisioning job finishes). Polling and identity validation accept the job reaching `succeeded` and/or the workspace reaching `running`.
- **Provisioner key parsing:** the pinned server returns a **raw 43-char alphanumeric secret** (`{"key":"…"}`), not a `coder_`-prefixed string; the harness creates keys via the REST API and parses/validates the secret offline.
- **Persistent, atomic ledger:** every created resource (org, template, user, workspace, provisioner key) is recorded **immediately** in `OPENFLOWS_CODER_LEDGER_FILE` (default `<tmp>/ofci-ledger.json`) together with the **deployment URL, organization, and immutable resource ID**, under a cross-process file lock.
- **Org-scoped, validated cleanup:** `--cleanup` only deletes entries whose recorded deployment URL matches `CODER_URL` and whose org matches the test prefix; it deletes children before parents, org-scopes every delete, spins a provisioner daemon back up (from a stored 0600 secret) so Terraform deletion jobs can run, and **retains** entries whose deletion failed. If a child (workspace/template) deletion fails for an org, its **dependencies (template, user, provisioner key, org) are preserved** for retry, and cleanup **fails with a non-zero status** whenever resources remain.
- **Credentials never printed:** command output is redacted; provisioner-key and session tokens are written to 0600 temp files and consumed programmatically, never echoed.
- **Isolation is never inferred:** `iso_cross_tenant` uses explicit fixture orgs and workspace fixtures created by `--mutating`, requires positive controls (each identity reads its own org models, sees its own workspace, creates a chat in its own org) plus authenticated denials (`403`/`404`, not `401`) in **both** directions for models, workspaces, and chats. Only a valid response (empty 200 or explicit 403/404) establishes isolation — a request failure, 5xx, or 401 is an error, never a pass. **Missing mandatory fixtures is a hard failure**, not a silent green.

Read-only:
```sh
export CODER_URL=https://coder.example.com
export CODER_SESSION_TOKEN=<operator token>   # or CODER_SESSION_TOKEN_FILE=/path
./tests/integration/coder_compatibility.sh
```

Mutating scenario (creates the two isolation orgs + workspace fixtures; explicit opt-in, isolated org prefix):
```sh
export OPENFLOWS_CODER_MUTATE=1
export OPENFLOWS_CODER_TEST_ORG_PREFIX=ofci-$(whoami)-
./tests/integration/coder_compatibility.sh --mutating
```

Isolation (MUST run after `--mutating` populated the fixtures; the wrapper sources the fixture manifest and mints/uses the org A/B identity tokens):
```sh
export OPENFLOWS_CODER_MUTATE=1
./tests/integration/coder_compatibility.sh --isolation
```

Cleanup (deletes only resources recorded in the ledger):
```sh
export OPENFLOWS_CODER_MUTATE=1
./tests/integration/coder_compatibility.sh --cleanup
```

### 4.2 Results (this PR)

| Check | Result |
|---|---|
| `cargo test -p coder-client --lib` | **PASS** (18 tests) — existing checks unchanged |
| `cargo test -p coder-client --test compatibility --no-run` (default + `chats-api`) | **PASS** (compiles both) |
| `cargo test -p coder-client --test compatibility offline` (default + `chats-api`) | **PASS** (20 offline regression tests) — key parsing, workspace/build-response validation, ledger identity, daemon status, isolation decision logic (errors never prove invisibility), incomplete-result handling; no deployment needed |
| Wrapper read-only mode (bash 3.2) | **PASS (argument parsing/group selection)** — runs only the `ro_` group; no bash/cargo-flag errors. This is NOT a successful verification run: a live deployment is still required for the `ro_*` assertions. |
| Wrapper mutating mode (bash 3.2) | **PASS (argument parsing/group selection)** — runs only `mut_scenario_provision_tenant` with `--features coder-client/chats-api` placed before `--`; no `Unrecognized option`. NOT a successful verification run. |
| Wrapper isolation mode (bash 3.2) | **PASS (argument parsing/group selection)** — sources the fixture manifest and runs only `iso_cross_tenant`; fails loudly (exit 1) if the `--mutating` manifest is absent. |
| Wrapper cleanup mode (bash 3.2) | **PASS (argument parsing/group selection)** — runs only `cleanup_created_resources`; cleanup is never selected by read-only, mutating, or isolation modes. |
| Harness `env_summary` | **PASS** (reports `token_set=false`) |
| Harness read-only (no live env) | **NOT_VERIFIED** — fails with explicit NOT_VERIFIED message, never fabricated evidence |
| Live C1–C11 against a deployment | **NOT_VERIFIED** — no licensed Coder deployment configured for this PR |

**Skipped live checks with reasons:** all live checks (C1–C11) require a licensed Coder deployment with a Premium license. None was configured. They remain the release gate for WP-05 (spec `04-implementation-plan.md` WP-00 "Done" criterion: "adapter contracts are grounded in the target version").

## 5. Security boundary findings

1. **Platform Coder session token reaches a customer runtime.** `ensure_tenant` passes `coder_session_token` (the admin's token) into the Nexus workspace; `openflows-nexus/main.tf:188,255` exports it as `CODER_SESSION_TOKEN`. Violates "customer runtimes must never receive platform administrator credentials."
2. **Global hook signing secret is distributed.** `coder_chat_hook_secret` is passed into every Nexus workspace (`openflows-nexus/main.tf:193,260`). A compromised tenant could forge lifecycle hooks for any tenant. Spec §6 requires moving verification to the manager ingress with per-tenant signing keys.
3. **Workspace owner is the human admin, and the owner's GitHub token is inherited.** Forge template (`openflows-forge/main.tf:221-230`) writes `data.coder_external_auth.github.access_token` into `.git-credentials`. Because the owner is the admin (not a tenant machine identity), the tenant commits/pushes as the admin's account.
4. **Static GitHub token, no refresh.** `github/rest.rs:26` stores one `String`; agents resolve a global token (`registry.rs:487`). No expiry, no per-tenant installation scope.
5. **No cross-tenant isolation demonstrated.** Because there is no org-scoped model/chat path (everything uses default org) and no per-tenant machine identity, isolation cannot even be *tested* today. It is **NOT_VERIFIED**, never inferred.

## 6. Registry-removal impact inventory

This PR does **not** remove `registry.json` (per scope). It inventories every consumer so a later migration is bounded:

| Consumer | Location | Dependency |
|---|---|---|
| Persisted to file from env | `binary/src/bin/agentflow.rs:314-327` | writes `registry_path` |
| Stored in Redis | `agentflow.rs:336`; `bootstrap.rs:753` | `registry_json` key |
| Passed to Nexus workspace | `bootstrap.rs:504` (`registry_json` param); `openflows-nexus/main.tf:53,192,259` | template parameter |
| Loaded at Nexus start | `crates/agent-nexus/src/lib.rs:300-326` (`load_registry` file/env/`registry_json`) | startup |
| Worker fleet init | `lib.rs:754` (`sync_registry`) | worker slots |
| Template CLI/module selection | `lib.rs:928`, `crates/config/src/registry.rs:266` (`resolve_coder_module`) | role → cli/module |
| GitHub token resolution | `crates/config/src/identity.rs:295`; `registry.rs:440` | per-agent token env |

**Removal strategy (future, not this PR):** replace with typed Manager-owned release/role configuration (spec `README.md:30`); the hosted path must reject a missing/legacy `registry.json` rather than read it. No new dependency on `registry.json` is introduced here.

## 7. Open decisions and required operator inputs

### D1 — Production backend adapter
- **Decision required:** Docker (current) vs Kubernetes (proposed default) for tenant isolation, secret injection, and networking.
- **Recommendation:** Kubernetes with per-workspace mounted Secrets, namespace/service-account RBAC, and network policy, per spec `03-template-provisioning.md` §6. Docker Compose is not an accepted public isolation boundary without validation.
- **Blocks:** WP-06 secret injection and network isolation; WP-00 "select and document the production backend adapter" (`04-implementation-plan.md` WP-00).

### D2 — Secret provider
- **Decision required:** which secret manager (Vault, cloud KMS, SOPS, …) for the App private key, GitHub installation tokens, Coder operator token, and workspace bootstrap credentials.
- **Recommendation:** a key-management abstraction behind a trait (spec `README.md:74`), with `OPENFLOWS_SECRET_PROVIDER` selecting the adapter; hash high-entropy random tokens with SHA-256, encrypt recoverable secrets with versioned keys.
- **Blocks:** WP-04 (credential broker) and WP-01 (secret-provider trait).

### D3 — Tenant credential model
- **Decision required:** how a tenant machine identity's Coder token is minted and scoped (per-tenant service account vs a scoped token per workspace), and whether Coder Agents/chat operations are direct (tenant token) or manager-mediated.
- **Recommendation:** per-tenant Coder machine owner with narrowly required workspace + Agents permissions; move org-scoped chat/model listing behind manager authorization because the current default-org resolution cannot be tenant-scoped (spec `03-template-provisioning.md` §6).
- **Blocks:** WP-05 (explicit org/owner/version), WP-06 (runtime isolation).

### D4 — Direct vs manager-mediated Coder operations
- **Decision required:** which Coder operations the tenant runtime performs directly with its scoped token vs which must be brokered by the manager (to prevent a tenant token reading another tenant's chats/workspaces).
- **Recommendation:** broker any operation upstream RBAC cannot tenant-scope; contract-test each remaining direct operation, especially org-scoped chat/model listing (spec `03-template-provisioning.md` §6).
- **Blocks:** WP-06 tenant integration and the cross-tenant isolation acceptance (ISO-01).

### Operator inputs required (not fabricatable)
- A **licensed Coder Premium deployment** at v2.37.3 with two isolated test organizations/identities for the isolation check.
- A **`coder` CLI** on PATH (version recorded by the harness).
- Coder **Owner-role** credentials for org/machine-identity/template-version checks.
- The **production backend**, **secret provider**, and **artifact storage** decisions (D1–D2) are deployment choices supplied by operators, not values to invent.

## 8. Bounded recommendation for the next PR (WP-01)

**Next segment (WP-01) can proceed on the interface inventory, but cannot be fully validated until a licensed deployment is configured.** Specifically:

- WP-01 (database/manager foundations) has no dependency on a live Coder deployment and can proceed now.
- WP-05 (Coder adapter + template releases) is **blocked** on (a) a licensed Coder deployment to run the harness C1–C11, and (b) reconciling the version drift (§3.1) and the explicit org/owner/version client changes.
- WP-06 (isolation) is **blocked** on D1/D4 and the two-tenant live isolation check (C11).

Recommended order for the reviewer: accept this contract and harness; provision the licensed v2.37.3 deployment; run `./tests/integration/coder_compatibility.sh`; then start WP-01 with the version drift and org/owner/version client work queued for WP-05.
