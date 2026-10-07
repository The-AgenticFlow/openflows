# Implementation backlog and deployment acceptance

## Scope and delivery strategy

Implement the three specifications as incremental, reviewable changes. Each work package must compile and preserve explicit local mode. Do not expose partial hosted endpoints publicly before their authorization exists. Feature-gate hosted mode until the end-to-end acceptance suite passes.

No production installation, migration, deployment, or secret creation is authorized by this document alone. This is an implementation specification. Use isolated development/test resources for validation; report operator prerequisites rather than pretending external accounts or licenses are configured.

## Work packages

### WP-00 — Compatibility and integration contract

Dependencies: none.

- Inspect repository instructions and current Git state. Inventory actual Coder/GitHub calls, CLI commands, hooks, and credentials used by each agent.
- Record Coder server/CLI/provider versions and the five template requirements. Default currently checked in is server v2.37.3.
- Exercise organization CRUD, provisioner registration, template import/version pinning, machine owner creation, restricted token issuance, and Agents/chat permissions against a licensed isolated Coder deployment.
- Select and document the production backend adapter. Proposed default is Kubernetes with workspace secret mounts and network isolation. Keep Docker for local development.
- If deployment access is unavailable, implement mock adapter contracts and mark live compatibility validation pending. Do not claim a public deployment is ready.
- Produce `compatibility-report.md` beside these specifications with verified methods, versioned request/response fixtures, unsupported features, and the exact required operator inputs. Redact credentials.

Done: adapter contracts are grounded in the target version; any upgrade/backend requirement is explicit. Offline coding may continue while external validation remains a release gate.

### WP-01 — Database and manager foundations

Dependencies: WP-00 interface inventory.

- Add PostgreSQL/SQLx persistence and numbered migrations covering the schemas in all three specifications, split by domain.
- Add typed IDs/DTOs, shared errors, cursor pagination, idempotency records, audit/outbox, operation leasing, and a secret-provider trait.
- Extend manager state with database pool and domain services. Preserve `/health` as process liveness; `/ready` checks essential database/schema and bounded dependency probes. Expose upstream capability failures separately rather than restarting healthy processes indefinitely.
- Add transaction helpers that make tenant/org scoping explicit. Avoid public repository methods accepting only a resource ID without authorization context.

Done: migrations work on empty PostgreSQL, a transaction rollback leaves no partial membership/outbox records, competing operation workers do not execute the same leased step concurrently, and manager health tests still pass.

### WP-02 — Human login and organization authorization

Dependencies: WP-01.

- Implement GitHub human authorization, browser sessions, Openflows CLI device approval, opaque access credentials, refresh rotation/reuse detection, logout, and user suspension checks.
- Implement org creation/list/detail/settings, membership lifecycle, GitHub-ID invitations, owner transfer, and deletion request authorization.
- Centralize policy checks. Owner remains a separate field; admin is the only GitHub-connection role.
- Create minimal server-rendered login/device-approval/invitation pages sufficient for CLI onboarding. A general dashboard is not required.

Done: spec 01 acceptance tests pass, including two-org role separation, callback replay, last-admin concurrency, wrong invitee, and session revocation. Org creation enqueues provisioning but does not require synchronous upstream success.

### WP-03 — GitHub connection lifecycle

Dependencies: WP-02.

- Implement connection attempts, separate OAuth/setup states, admin revalidation, GitHub account ownership verification, and immutable installation binding.
- Implement repository synchronization with pagination and immutable IDs; update names without changing tenant identity.
- Add signed webhook ingress, delivery deduplication, durable processing, out-of-order reconciliation, disconnect/reconnect, and periodic access refresh.
- Build App JWT signer and installation exchange client behind testable traits; keep user authorization and installation authentication separate.

Done: two-org binding tests pass; repo reader cannot claim installation; stale callbacks cannot connect after admin revocation; missed/reordered removal events converge safely.

### WP-04 — Credential broker and runtime authentication

Dependencies: WP-01, WP-03; backend credential-injection contract from WP-00.

- Implement workspace bootstrap, runtime registration/access/refresh, generations, revocation, single-flight credential renewal, and encrypted leases.
- Implement `GitHubCredentialProvider` in the GitHub crate, preserve static local/test adapter, and migrate hosted agent calls.
- Implement Git credential helper and token-aware wrappers for tools that cannot use it. Test host/path restrictions and refresh through a long operation.
- Implement immediate deny-new-access on disconnect with asynchronous revocation of recorded tokens.

Done: expired credentials renew; runtime callers cannot choose another tenant's scope; no credential is returned after generation invalidation; replay and secret/log tests pass.

### WP-05 — Coder adapter and template releases

Dependencies: WP-00, WP-01.

- Add explicit organization, owner, template, and version IDs to hosted Coder operations and response verification.
- Implement deterministic artifact packaging, manifest validation, immutable release records, import job polling, and per-org template publication.
- Replace host-local template hash cache for hosted mode. Do not infer release identity from names or updated timestamps.
- Add organization/provisioner/model readiness reconciliation and platform-only approval/promotion.

Done: same template names in two organizations cannot collide; failed import prevents ready; duplicate publish retries converge; version pins survive active-pointer changes.

### WP-06 — Tenant runtime integration and isolation

Dependencies: WP-04, WP-05.

- Add tenant CRUD and durable Nexus/worker provision operations with quotas, runtime identities, immutable namespace IDs, and owner-aware recovery.
- Implement production secret injection, tenant-scoped Redis credentials, network isolation, and credentials rotation without rebuild-only renewal.
- Route Nexus worker requests through manager runtime endpoints. Audit remaining Coder calls, use explicit org IDs, and broker any call that cannot be safely scoped upstream.
- Move global Coder hook verification to manager ingress and relay with tenant-scoped authentication while retaining hook deadlines and response semantics.
- Update all role templates to remove hosted external-auth token interpolation and deployment-wide credentials. Pin workspace images and release metadata.

Done: complete tenant can run the existing issue -> planning -> build/review -> PR flow; controller has no operator token; another tenant cannot read state, secrets, chats, or workspaces. Public deployment remains blocked if the chosen infrastructure cannot enforce this.

### WP-07 — Hosted CLI experience

Dependencies: WP-02 for login; WP-03/WP-06 for complete onboarding.

- Add hosted server configuration, login/logout/whoami, org create/list/use, member commands, GitHub connect/status/disconnect, repository listing, and operation wait/status.
- Route existing tenant add/list/remove/clean and status through authorized endpoints in hosted mode. Preserve `--fleet`, naming validation, and machine-readable output.
- Inventory gate/store/hooks commands: expose only approved, scoped product actions or reject with a clear operator-only message. Raw Redis administration, bootstrap, and hook simulation stay local/operator-only.
- Local mode requires explicit configuration. Missing hosted authentication or unavailable manager cannot trigger local fallback.

Example target workflow (new commands are proposed, not currently shipped):

```sh
openflows login --server https://openflows.example.com
openflows org create acme --display-name Acme
openflows org use acme
openflows github connect
openflows repo list
openflows tenant add acme/backend --name backend --fleet 2
openflows tenant list
openflows status --tenant backend
```

An ordinary invited developer starts at login/org selection and tenant creation; `github connect` must fail for that developer. The CLI resolves `owner/repo` to an accessible immutable repo ID before POST; the server revalidates access.

Done: a fresh customer admin onboards without `.env`, Redis access, Coder admin token, or template uploads. Invited developer can use the established connection without installing the App.

### WP-08 — Migration, operations, and release gate

Dependencies: WP-06, WP-07.

- Add operator configuration reference, scoped secret rotation runbook, reconciliation/cleanup commands, alerts, backup/restore procedure, and release rollback instructions.
- Provide an explicit legacy import command that produces a dry-run mapping first. Never infer ownership from matching tenant names. Operator maps each old tenant to an Openflows organization, installation/repo ID, and new runtime identity.
- Pause legacy automation, snapshot runtime data, create org resources, migrate Redis namespace to immutable tenant ID, recreate/rebuild workspaces with scoped credentials, verify, then revoke old credentials. Avoid running old and new controllers simultaneously.
- Preserve old templates/local bootstrap for local users until migration is validated; hosted paths must not reference them accidentally.
- Execute acceptance suite below and record evidence. Update quick-start/operator documentation to distinguish hosted customers, platform operators, and local development.

Done: clean onboarding, legacy migration rehearsal, isolation suite, recovery exercise, and all required upstream compatibility checks pass.

## Configuration contract

Names below are proposed new configuration keys, to be added centrally in the config crate with typed validation. Do not scatter direct environment lookups through handlers.

| Key | Scope and purpose |
|---|---|
| `OPENFLOWS_MODE` | `local` or `hosted`; explicit precedence with CLI profile |
| `OPENFLOWS_API_URL` | CLI/runtime manager origin |
| `OPENFLOWS_PUBLIC_URL` | Manager callback/verification origin |
| `OPENFLOWS_DATABASE_URL` | Manager product PostgreSQL credential |
| `OPENFLOWS_GITHUB_APP_ID` / `OPENFLOWS_GITHUB_CLIENT_ID` | Central App identifiers |
| `OPENFLOWS_GITHUB_CLIENT_SECRET_REF` | Secret-provider reference for OAuth exchange |
| `OPENFLOWS_GITHUB_PRIVATE_KEY_REF` | Secret-provider reference for App JWT signer |
| `OPENFLOWS_GITHUB_WEBHOOK_SECRET_REF` | Signature verification secret |
| `OPENFLOWS_SECRET_PROVIDER` | Selected secret adapter; dev adapter explicit |
| `OPENFLOWS_CODER_OPERATOR_TOKEN_REF` | Platform-only Coder adapter credential |
| `OPENFLOWS_TEMPLATE_MANIFEST_URI` / `OPENFLOWS_TEMPLATE_MANIFEST_SHA256` | Approved immutable release source |
| `OPENFLOWS_PROVISIONER_BACKEND` | Validated backend adapter |
| `OPENFLOWS_RUNTIME_CREDENTIAL_FILE` | Workspace-mounted credential bootstrap/renewal material |

Continue using existing `CODER_URL` configuration where appropriate. Exact secret-provider backend, infrastructure context, artifact storage, DNS/TLS, and licensed Coder access are operator inputs, not values to fabricate. App keys and encrypted credential data need independent rotation/versioning.

## Verification matrix

| ID | Scenario | Required observation |
|---|---|---|
| AUTH-01 | User in A only requests B resources | 404, no upstream call |
| AUTH-02 | Developer or owner-without-admin connects GitHub | 403, no connection attempt |
| AUTH-03 | Last two admins demoted concurrently | At least one active admin remains |
| AUTH-04 | Membership revoked during callback | Binding denied |
| GH-01 | Same App, installations A/B, repos A1/B1 | Tokens restricted to correct installation/repo/profile |
| GH-02 | Credential expires mid-run | Controlled renewal, continued Git/API access |
| GH-03 | Repo removed or App suspended | Immediate deny after receipt, revocation/stop job, no auto-resume |
| GH-04 | Missed/reordered webhook | Periodic reconciliation restores correct deny state |
| PROV-01 | Crash after each upstream create | Retry adopts only exact matching resource, no duplicates |
| PROV-02 | Template names equal in A/B | Correct org-specific IDs and pinned versions |
| PROV-03 | No license/provisioner/model | Explicit not-ready status, no default-org fallback |
| ISO-01 | Compromised A runtime requests B token/store/chat | Denied by service and infrastructure boundaries |
| ISO-02 | Inspect hosted parameters/state/logs | No platform or App secrets, no static GitHub token |
| ISO-03 | Tenant hook signing key used against another tenant | Rejected; global hook key absent from runtimes |
| OPS-01 | Manager restarts during onboarding | Leased operations resume safely |
| OPS-02 | Database/secret broker/GitHub outage | Fail closed for authorization/issuance, bounded retries |
| OPS-03 | Restore backup and reconcile upstream | No duplicate controllers, credentials revalidated/rotated |
| E2E-01 | Admin creates org and invited dev adds tenant | No customer Coder/Redis credentials required |
| E2E-02 | Existing issue-to-PR flow on hosted tenant | Planning/review gates and lifecycle cleanup preserved |

Use unit tests for policy/state machines, PostgreSQL integration tests for constraints and concurrency, HTTP fixtures for upstream adapters, licensed Coder integration tests for RBAC/version contracts, and two-org staging tests for true isolation. Mocks alone do not prove Coder or network isolation.

Run `cargo fmt --check`, targeted crate tests for each change, then workspace checks before the integrated release. Determine actual feature flags from Cargo manifests; do not invent test commands that silently omit hosted modules. Record skipped live checks explicitly.

## Out of scope for v1

Billing, arbitrary customer-authored templates, GitHub Enterprise Server, delegated GitHub installation managers, direct human Coder dashboard access, tenant-specific human ACLs, multi-region failover, and automated installation transfer between Openflows organizations. Preserve interfaces where useful without implementing unrelated product features.
