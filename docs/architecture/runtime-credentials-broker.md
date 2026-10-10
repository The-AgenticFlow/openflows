# Runtime authentication and the GitHub credential broker (WP-04)

This document describes the WP-04 runtime principal and the GitHub credential
broker implemented in `crates/openflows-manager`. It covers how a workspace
runtime authenticates, how it obtains a short-lived GitHub installation token
scoped to exactly its authorized repository and permission profile, how
concurrency and revocation are handled, and the trusted provisioning
integration required by WP-05/06.

The source specification is the centralized-deployment documentation
(`02-github-app.md` §7–§10 and the WP-04 boundary in `04-implementation-plan.md`).

## Trust boundaries

There are three distinct principals. They must never be conflated:

| Principal | Credential | Holder | Forbidden use |
|---|---|---|---|
| Human (browser/CLI) | session cookie / CLI access token | `users` + `sessions` | Runtime broker endpoint |
| GitHub App | App JWT (never persisted/logged) | Manager `AppSigner` | Runtime repository access |
| Workspace runtime | workspace bearer credential (hash-stored) | a specific workspace | Organization administration |

The runtime broker endpoint (`POST /api/v1/runtime/github-credentials`) is
authenticated **only** by a workspace runtime credential. It deliberately does
not consult the human session manager, so a browser cookie or human CLI access
token can never authorize it. Human GitHub OAuth tokens, App JWTs, and the App
private key are never issued to runtimes.

## Runtime identity

A workspace runtime credential is a high-entropy bearer secret:

- Bound to a workspace (`runtime_credentials.workspace_id`).
- Stored **only** as a SHA-256 hash (`credential_hash`); the raw value is
  returned exactly once at issuance and never persisted or logged.
- Carries an `audience` (the constant `openflows-workspace`), an `expires_at`,
  a `revoked_at`, and a `generation`.
- Must match the workspace's current `credential_generation` to authenticate.

The runtime's organization and tenant are **derived from trusted database
relationships** (workspace → tenant → organization → connection → repository)
in a single scoped query. A caller can never supply an organization, tenant,
workspace, installation, or repository id to the broker.

### Issuance, rotation, revocation (internal services)

Provisioning APIs are not yet available (WP-05/06), so issuance, rotation, and
revocation are exposed as **internal services** only — there is no
unauthenticated bootstrap endpoint and no shared global runtime password.

- `RuntimeCredentialService::issue_for_workspace(workspace)` — mints a fresh
  credential at the workspace's current generation.
- `RuntimeCredentialService::rotate(workspace)` — advances the workspace
  generation, revokes every previous credential, and issues a fresh one at the
  new generation. Old credentials stop authenticating (generation mismatch).
- `RuntimeCredentialService::revoke(workspace)` — advances the generation,
  revokes all credentials, and revokes the workspace's outstanding GitHub
  credential leases.

WP-05/06 will call these from the trusted provisioning boundary.

### Authentication

`RuntimeCredentialService::authenticate(bearer)` hashes the presented value,
resolves the full scope, and rejects on:

- missing/unknown credential,
- wrong audience,
- expired credential,
- revoked credential,
- stale generation (rotated).

A human access token never matches a `runtime_credentials` hash and is rejected
with `401`.

## GitHub credential broker

`CredentialBroker::exchange(scope, purpose, request_id)`:

1. Validates resource state: organization available, tenant not paused/deleting,
   workspace `running`, connection `active`, repository `accessible`. Any
   failure returns a sanitized `403` and no upstream call.
2. Resolves the server-controlled permission profile from the workspace role
   and the requested `purpose` (`git` or `api`). Unknown roles fail closed.
3. Captures the connection and workspace generations **before** the external
   exchange.
4. Signs a fresh App JWT and exchanges it for an installation token restricted
   to `[repository_id]` and the profile permissions.
5. Validates the returned repository scope, permission set, and expiry
   (defense-in-depth on top of the `RealGithubAppApi` client check). Any
   mismatch fails closed.
6. **Rechecks** authorization, resource state, and both generations **after**
   the exchange, before recording the lease. If anything changed, the token is
   discarded, upstream revocation is attempted, and a sanitized `403` is
   returned.
7. Records an encrypted credential lease and returns the token.

No database transaction is held open across the GitHub request. The broker uses
an in-process single-flight lock keyed by
`(app, installation, repository, profile_hash, connection_generation,
workspace_generation)` to avoid duplicate upstream exchanges. A distributed lock
is required when the Manager runs multiple replicas (operations follow-up).

### Endpoint

```
POST /api/v1/runtime/github-credentials
Authorization: Bearer <workspace-runtime-credential>
Content-Type: application/json
Cache-Control: no-store        (response)

{"purpose": "git"}             // or {"purpose": "api"}
```

Response (placeholders; never log the token):

```json
{
  "token": "<short-lived-github-installation-token>",
  "expires_at": "<rfc3339>",
  "repository_id": 9001,
  "username": "x-access-token",
  "permissions": {"contents": "write"}
}
```

All authorization scope is derived server-side. Extra request fields (e.g.
`repository_id`) are ignored; the broker always uses the authenticated
workspace's tenant repository.

### Permission profiles

Profiles are server-controlled allowlists grounded in the runtime role, never
supplied by a caller. `metadata` is implicit.

| Role | `git` | `api` |
|---|---|---|
| nexus | contents: read | contents: read, issues: write, pull_requests: write |
| forge | contents: write | contents: write, pull_requests: write |
| sentinel | contents: read | contents: read, pull_requests: write, checks: read |
| vessel | contents: write | contents: write, pull_requests: write, checks: read, commit_statuses: read, actions: read |
| lore | contents: write | contents: write, pull_requests: write |

## Concurrency and revocation

- Generations are captured before the external exchange and rechecked after it,
  before the lease is recorded and the token returned.
- If authorization changes during an exchange (connection disconnect,
  workspace rotation/revocation, installation removal), the token is discarded
  and upstream revocation is attempted.
- Disconnect immediately disables local access (status + connection
  `access_generation` increment), which stops new issuance. The disconnect
  worker's `github.disconnect_cleanup` event then revokes the connection's
  outstanding credential leases asynchronously.
- Webhook access-removing events (installation deleted/suspended, repository
  removed, reconciliation failures) also revoke the connection's leases.
- Workspace shutdown/deletion and runtime rotation revoke the workspace's
  leases via `revoke_leases_for_workspace`.

### Revocation limitations

GitHub provides **no endpoint to revoke an installation access token** before it
expires. The broker therefore:

- discards the local lease (`revoked_at`, clears `encrypted_ref`),
- attempts upstream revocation (`revoke_installation_token`, a documented no-op
  in the real client),
- relies on the token's short (1-hour) lifetime.

An already-issued GitHub token may remain usable until upstream revocation
succeeds or it expires. Local generation checks invalidate **future** issuance;
they do not invalidate a token already delivered to GitHub. This limitation is
explicit and documented.

Transient cleanup failures are retryable: the outbox event is released and a
restarted worker reclaims and completes it.

## Secret handling

- The runtime credential is stored only as a hash.
- The GitHub installation token is stored only in the lease's `encrypted_ref`,
  encrypted with the envelope cipher (the existing encrypted-secret mechanism).
  It never appears in an ordinary column, log, `Debug` output, audit event,
  outbox payload, URL, or error message.
- `token_fingerprint` (SHA-256) identifies a lease without persisting the value.
- Credential responses return `Cache-Control: no-store`.

## Provisioning integration (WP-05/06)

WP-05/06 will call `RuntimeCredentialService::issue_for_workspace` /
`rotate` / `revoke` from the trusted Coder provisioning/teardown paths. No
provisioning API is implemented in WP-04; the internal services and the
documented scope derivation are the integration surface. A production
`SecretProvider` adapter remains a deployment decision (WP-08); WP-04 preserves
hosted-mode restrictions and never silently falls back to development secrets.
