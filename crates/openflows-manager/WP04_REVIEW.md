# WP-04 review and implementation handoff — Runtime authentication and GitHub credential broker

Review date: 2026-10-10. Scope: WP-04 on the `codex/wp04-runtime-credentials` branch,
based on `develop` at merge of PR #402 (WP-03 GitHub connection lifecycle). This
review makes local, reviewable changes only; it does not authorize a public
deployment, and untested production behavior is not marked verified.

The source specification is the centralized-deployment documentation
(`docs/implementation/centralized-deployment/02-github-app.md` §7–§10 and the
WP-04 boundary in `04-implementation-plan.md`) plus the task scope. Where a
detail was absent, assumptions are recorded below.

## What was implemented

### Runtime identity
- A workspace runtime principal distinct from human browser/CLI sessions and
  human API credentials. Credentials are workspace-scoped, hash-stored, and
  carry `audience`, `expires_at`, `revoked_at`, and `generation`.
- Organization/tenant/connection/repository are derived from trusted database
  relationships in a single scoped query; no caller-supplied IDs.
- Authentication requires the correct audience, a live (unexpired, unrevoked)
  credential, and the current workspace credential generation.
- Issuance, rotation, and revocation are **internal services**
  (`RuntimeCredentialService`); there is no unauthenticated bootstrap endpoint
  and no shared global runtime password.
- New secrets are returned only through the trusted provisioning boundary
  (internal services, to be called by WP-05/06 provisioning).

### GitHub credential broker
- `POST /api/v1/runtime/github-credentials`, authenticated exclusively by the
  workspace runtime credential. Human/browser credentials are rejected.
- All scope (organization, installation, repository, permission profile) is
  derived server-side; the only caller input is the coarse `purpose`.
- Server-controlled permission profiles per (role, purpose), grounded in the
  spec's role profiles.
- Short-lived installation token exchange restricted to the tenant repository
  and profile, with validation of returned repository scope, permissions, and
  expiry (fail closed). The `GithubAppApi` trait was extended with a
  `permissions` argument and a best-effort `revoke_installation_token`.
- Encrypted credential lease recorded; the token is stored only as an
  envelope-encrypted `encrypted_ref`.

### Concurrency and revocation
- Connection/workspace generations captured before the external exchange and
  rechecked (with resource state) before the lease is recorded and the token
  returned. No DB transaction held across GitHub requests.
- If authorization changes during an exchange, the token is discarded, upstream
  revocation is attempted, and a sanitized `403` is returned.
- In-process single-flight per broker key to coalesce duplicate exchanges.
- Lease cleanup wired into the disconnect worker (`github.disconnect_cleanup`)
  and into webhook access-removing paths (installation deleted/suspended,
  repository removed, reconciliation failures). Workspace rotation/revocation
  revokes the workspace's leases.
- Transient cleanup failures are retryable via the existing outbox lease/release
  cycle and recover after worker restart.

### Secret handling
- Runtime credential stored only as a hash; GitHub token only in the encrypted
  lease reference; fingerprint (SHA-256) identifies leases.
- No raw credentials in ordinary DB fields, logs, `Debug`, audit events, outbox
  payloads, URLs, or error messages.
- Credential responses return `Cache-Control: no-store`.
- Hosted-mode restrictions preserved; no silent fallback to dev secrets.

## Assumptions and deviations

1. **GitHub installation-token revocation.** GitHub has no revocation endpoint
   for installation access tokens. `revoke_installation_token` is a documented
   no-op in the real client; revocation is local (discard lease) plus reliance
   on the token's short expiry. This is documented, not hidden.
2. **Single-flight is in-process only.** The spec's distributed lock is deferred
   to a multi-replica operations follow-up; the single-replica Manager is the
   WP-04 target.
3. **`encrypted_ref` uses the envelope cipher**, not a secret-provider entry,
   because no durable production provider is implemented (WP-08). The cipher
   key lives in the configured master key.
4. **Unknown-role path is covered by unit tests only.** The `workspaces.role`
   DB CHECK prevents constructing an unknown role in an integration test; the
   `resolve_profile` fail-closed path is unit-tested.
5. **No HTTP provisioning endpoint.** Issuance/rotation/revocation are internal
   services only, per the task ("if provisioning APIs are not yet available").
   WP-05/06 provisioning is the integration surface.
6. **Runtime credential TTL** defaults to 30 days (a workspace identity bearer
   used only to authenticate to the broker, which enforces scope per exchange).
   Rotation shortens exposure.

## Migrations

`0010_runtime_credentials_and_broker.sql` (additive, forward-only):

- `runtime_credentials`: partial index for live workspace credentials.
- `credential_leases`: added `connection_id UUID`, `workspace_generation BIGINT
  NOT NULL DEFAULT 0`, `purpose TEXT`; index on `(connection_id, workspace_id)`
  for unrevoked lease cleanup.

No applied migration is rewritten. Migrations 0001–0009 are unchanged.

## Tests

Added `crates/openflows-manager/tests/runtime_credentials.rs` (23 tests) plus
unit tests in `runtime/permissions.rs`. All use HTTP fixtures (a controllable
`GithubAppApi`) and isolated PostgreSQL databases. Concurrency tests use
deterministic oneshot-channel barriers, not timing sleeps.

Coverage:
- Successful runtime exchange + restricted scope/permissions; `api` purpose.
- Expired, revoked, wrong-audience, stale-generation credentials.
- Rejection of human credentials at the runtime endpoint.
- Cross-tenant scope is derived (caller body cannot override repository).
- Suspended org, disconnected connection, non-running workspace, inaccessible
  repository.
- Upstream wrong repository scope, excessive permissions, invalid expiry.
- Revocation during in-flight exchange and disconnect during in-flight exchange
  (deterministic barriers), asserting the token is discarded and upstream
  revocation attempted.
- Rotation invalidating the previous runtime credential.
- Cleanup retries after a temporary failure and worker restart.
- Secret redaction, no-store responses, and encrypted lease round-trip.

Validation run (all pass):

```
cargo fmt --all -- --check
cargo test -p openflows-manager -- --include-ignored
cargo clippy -p openflows-manager --all-targets -- -D warnings
git diff --check
```

Test results: all lib, integration, and `--include-ignored` PostgreSQL suites
pass (0 failures). These are fixture/database tests; no live GitHub verification
was performed. Live GitHub token exchange behavior depends on a real App
private key and installation, which are operator prerequisites.

## Remaining blockers / follow-ups

- No live GitHub verification (requires real App credentials/installation).
- Distributed single-flight locking for multi-replica Managers.
- A durable production `SecretProvider` adapter (WP-08).
- WP-05/06 provisioning calling the internal runtime-credential services and
  the Coder adapter / Git credential helper (out of WP-04 scope).

## Out of scope (not implemented)

Full Coder provisioning/template changes, tenant provisioning workflows, console
redesign, hosted CLI onboarding, new production secret-provider adapters, the
Git credential helper / `GitHubCredentialProvider` in the GitHub crate, and
unrelated WP-03 review cleanups.
