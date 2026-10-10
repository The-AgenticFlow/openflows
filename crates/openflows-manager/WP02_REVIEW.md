# WP-02 review and implementation handoff

Review date: 2026-10-09. Scope: the three WP-02 commits after the WP-01 merge,
plus the junior's uncommitted pages and HTTP tests. This review makes local
changes only. It does not authorize a public deployment.

The source specification is the centralized deployment documentation at
`58f41fed01210a39ebcd5171834eae67cc348798`, in particular
`01-user-management.md` and the WP-02 boundary in `04-implementation-plan.md`.
Those documents are on the documentation branch, not the current develop base.

## Fixed findings

| Severity | Finding | Correction |
| --- | --- | --- |
| P1 | Generated credentials hashed raw random bytes; incoming credentials hashed their encoded strings. Legitimate sessions, OAuth states, device secrets, and invitations could not validate. | Hash the exact wire representation and test both directions. |
| P1 | Clearing the OAuth transaction cookie replaced the new session cookie. Login pages also created authorization links without setting the transaction cookie. | Append both cookies and route sign-in links through the start handler. |
| P1 | Device polling ignored expiry and created sessions outside the consumption transaction. | Check expiry and polling interval under the row lock; consume and issue credentials atomically. |
| P1 | A competing refresh could miss newly inserted reuse history; suspended users could still refresh. | Serialize equal-token rotations before history lookup, revoke on reuse, and require an active user. |
| P1 | Every rate-limit call used a different timestamp, making the counter ineffective. Client-controlled forwarding headers bypassed limits, and bearer credentials could be persisted as bucket keys. | Use database minute boundaries, hashed bucket identifiers, independent scopes, and socket-peer identity. |
| P1 | Permission checks preceded organization locks; mutations and audit writes used separate transactions. | Revalidate current membership/ownership under the organization lock and commit audit with the mutation. |
| P1 | Ownership required the admin role and transfer automatically promoted the recipient. | Ownership requires active membership only; transfer preserves roles. Last-admin protection remains independent. |
| P1 | Organization lifecycle status did not restrict mutations; invitation acceptance could reactivate a suspended administrator. | Reject mutations for suspended/deleting/deleted organizations and reject invitations that would overwrite existing memberships. |
| P1 | Browser logout lacked CSRF checks; invitation forms omitted their token and could not submit their CSRF value. | Validate session-bound CSRF for browser mutations, include the invitation token, and support ordinary form submission. |
| P2 | Organization creation ignored the Idempotency-Key header and generated a new key on every retry. Invitations and deletion lacked request-key handling. | Require header keys and atomically persist result references. |
| P2 | List endpoints always returned a null cursor. | Bounded database queries return usable opaque cursors for organization and member lists. |
| P2 | Operation lookup exposed an unscoped repository method. | Join the operation lookup to the caller's active membership and user state. |
| P2 | New PostgreSQL HTTP tests were never invoked by the integration runner; a concurrency test counted successful task joins instead of successful mutations. | Run all WP-02 suites in the existing PostgreSQL CI job and correct the assertions. |
| P2 | Upstream timeouts covered response headers but not explicitly the complete request; redirects were followed. | Set a reqwest request/body timeout and reject upstream redirects. |
| P2 | Public origin and insecure-cookie settings were not validated; sensitive structures exposed secrets through Debug. | Require HTTPS except explicit loopback development and redact secret-bearing Debug output. |
| P2 | CLI device delivery reset authentication freshness. | Preserve the approving browser's authentication timestamp; refresh never resets it. |

## Persistence and concurrency

Migration 0006 adds WP-02 session/auth fields, organization-less idempotency,
and rate-limit metadata. Review migration 0007 adds the device approval's
authentication timestamp. Migrations 0001–0006 are not rewritten by this review.
Existing approved device requests without that timestamp cannot issue new
credentials; start a new device request after upgrading.

OAuth state is consumed atomically before the external exchange, so only one
callback can proceed. A failed upstream exchange requires restarting login.
The PKCE verifier and validated return destination are encrypted together.
This prevents an invitation token in the return URL from being persisted
in plaintext. Callback query parameters cannot override the stored destination.

All organization mutation services lock the organization first, re-read current
permissions on that connection, and insert audit records in the same transaction.
GitHub login resolution occurs before this transaction, followed by permission
revalidation. No database transaction waits on GitHub.

Organization creation returns 202 after committing the organization, creator's
owner/admin membership, operation, audit, and outbox event. The outbox event is
`org.provision_requested`, not a claim that provisioning succeeded. Deletion
emits `org.deletion_requested`. No provisioning/teardown executor is started by
WP-02: queued operations remain pending for the later infrastructure packages.

Invitation acceptance never changes an existing membership. An admin must use
the member endpoint to reactivate or change a suspended/removed member. This
prevents old invitation links from undoing a later membership decision.

## HTTP contracts and minimal onboarding

Browser pages:

- `GET /auth/login` links to the OAuth start route.
- `GET /auth/github/start?next=...` begins login; the return destination is
  limited to the onboarding pages and current-user endpoint.
- `GET /auth/github/callback` completes login and sets the browser session.
- `GET /auth/cli/verify` accepts the human code; its form submits to
  `POST /auth/cli/approve`. Sign in again if authentication is over ten minutes old.
- `GET /invitations/accept?token=...` starts sign-in when needed, then displays
  the acceptance form only for the exact invited GitHub identity.

Product endpoints are below `/api/v1`:

- `POST /auth/cli/start`, `POST /auth/cli/token`, `POST /auth/refresh`,
  `POST /auth/logout`, and `GET /me`.
- `GET /auth/csrf` returns a session-bound CSRF value and sets its cookie.
  Browser JSON mutations echo the value in `X-CSRF-Token`. CLI bearer requests
  do not require CSRF.
- Organization create/list/detail/update/delete, member list/update/remove,
  invitation create/revoke, ownership transfer, and scoped operation status.
- `POST /invitations/accept` accepts JSON `{"token":"..."}`; the browser route
  also supports a regular HTML form.

List endpoints accept `limit` and `after`, returning `items` and
`next_cursor`. Member updates return the updated typed membership.

Organization creation, invitation creation, and deletion require an
`Idempotency-Key` header. Organization creation and deletion replay the
original durable operation. Invitation retries return the original invitation
ID with `invitation_url: null`: the URL is disclosed once and only its hash is
stored. A lost first response therefore requires revoking that invitation and
creating a replacement with a new key. Reusing a key with different input is 409.
This explicitly resolves the tension between retryable creation and one-time
secret disclosure; clients must handle the null URL.

Browser sessions have a 12-hour absolute lifetime. CLI access credentials last
at most 15 minutes, with a 30-day absolute family lifetime. Rotation invalidates
the old access token. Reusing a consumed refresh token revokes the family,
including under concurrent requests. A lost refresh response requires login
again. Ownership transfer and deletion require authentication within ten
minutes for both browser and CLI sessions; refreshing does not renew that time.

Responses carrying auth data/pages are not cacheable and use a no-referrer
policy. Request logs use matched route patterns, never raw query strings or
user-selected paths. The application generates request IDs. Public rate limits
use the actual socket peer; `X-Forwarded-For` is deliberately not trusted.
Behind a reverse proxy, users share its bucket until a validated trusted-proxy
configuration is implemented. Apply edge limits as part of deployment setup.

## Local verification

No external GitHub or Coder account is required for these tests. The HTTP
fixtures drive real Axum handlers and PostgreSQL persistence with an injected
GitHub adapter.

```sh
cargo test -p openflows-manager
cargo clippy -p openflows-manager --all-targets --all-features -- -D warnings
cargo fmt -p openflows-manager --check
bash crates/openflows-manager/scripts/run-integration-tests.sh
```

The integration runner executes `postgres`, `http_auth`, `http_org`,
`org_policy`, and `review_regressions`, followed by health/readiness checks.
It creates and removes only isolated test databases. Set
`OPENFLOWS_TEST_DATABASE_URL` to select an existing test server with CREATEDB
privileges; otherwise it uses the Compose database on loopback port 5544.

Review verification: 31 ordinary tests passed, plus 52 explicitly enabled
PostgreSQL tests (8 auth HTTP, 5 org HTTP, 9 policy/concurrency, 14 foundation,
16 review regressions, including each binary's database teardown check).
Strict Clippy with all targets/features passed. Formatting and whitespace
checks passed. Existing migration files 0001–0005 remain byte-for-byte unchanged.

To run only the review regressions against that database:

```sh
cargo test -p openflows-manager --test review_regressions -- --include-ignored
```

## Configuration and remaining deployment gates

Human auth uses `OPENFLOWS_PUBLIC_URL`, `OPENFLOWS_GITHUB_CLIENT_ID`,
`OPENFLOWS_GITHUB_CLIENT_SECRET_REF`, and
`OPENFLOWS_CRYPTO_MASTER_KEY_REF`. The registered GitHub callback must match
`<public-origin>/auth/github/callback`. Explicit local HTTP development uses a
loopback origin and `OPENFLOWS_AUTH_ALLOW_LOOPBACK=true`.

Only the in-memory secret provider is implemented. Its default development
key is test/local material. Hosted mode rejects that provider and unknown
providers; writing `vault` into configuration does not create a Vault adapter.
Real hosted sign-in remains blocked on an operator-selected durable secret
adapter and its provisioning. Tests inject secrets and GitHub fixtures; their
success is not evidence of a configured production deployment.

Ciphertext currently supports key version 1. The migration's encryption-key
registry does not implement online rotation or a retained key ring. Rotation
requires a documented later adapter/runbook; unsupported versions fail closed.

Still outside this package: GitHub App installation/connection flows (WP-03),
runtime credentials (WP-04), Coder/template provisioning (WP-05/06), complete
hosted CLI commands and keychain storage (WP-07), and production release gates.
Live GitHub authorization, licensed Coder compatibility, network isolation,
edge/proxy behavior, and production secret storage have not been verified here.
There is no platform user-suspension API in WP-02. Existing suspended users are
denied; any future suspension workflow must handle organization succession or
suspend affected organizations in the same administrative workflow.

### Local development secret loading

When using `OPENFLOWS_SECRET_PROVIDER=in-memory`, set
`OPENFLOWS_DEV_SECRETS_FILE` to a private JSON file outside the repository.
The file maps the configured secret reference names to UTF-8 secret values,
for example `{"local/github":"your OAuth client secret"}` with
`OPENFLOWS_GITHUB_CLIENT_SECRET_REF=local/github`. Startup loads this file
before resolving references. If a master-key reference is configured, its
value must encode exactly 32 bytes. Missing references and invalid files fail
startup; parser errors do not echo credentials. Restrict file permissions to
the local user. This ephemeral provider remains unsuitable for hosted mode.
