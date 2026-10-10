# GitHub connection lifecycle: validation and handoff

## Configuration

Copy `.env.prod.example` to `.env.prod` and replace its values. The manager
loads `.env.prod` automatically when launched from the repository root. Shell
variables that are already exported take precedence over values in the file.

Hosted deployments require the existing OAuth and envelope-key configuration,
plus `OPENFLOWS_GITHUB_APP_ID`, `OPENFLOWS_GITHUB_APP_SLUG`,
`OPENFLOWS_GITHUB_PRIVATE_KEY_REF`, and `OPENFLOWS_GITHUB_WEBHOOK_SECRET_REF`.
Secret references resolve through the configured secret provider.
`OPENFLOWS_GITHUB_WEBHOOK_BODY_LIMIT` sets the maximum buffered webhook body
in bytes (default 2 MiB).

Configure the GitHub App with these manager URLs:

- Connection OAuth callback: `/api/v1/github/oauth/callback`
- Installation setup callback: `/api/v1/github/setup`
- Signed webhook endpoint: `/api/v1/webhooks/github`

The existing login callback remains `/auth/github/callback`. Subscribe to
installation, installation repository, and app authorization events.
Complete both returned connection URLs using the user who initiated the flow.
The service runs the connection outbox worker with the HTTP server.

## Automated acceptance checks

Run against a local PostgreSQL test role with permission to create databases:

```sh
cargo test -p openflows-manager -- --include-ignored
cargo clippy -p openflows-manager --all-targets -- -D warnings
git diff --check
```

The PostgreSQL tests use isolated databases, selected through
`OPENFLOWS_TEST_DATABASE_URL` (default localhost port 5544).
`connection_regressions.rs` covers binary credential storage and cleanup,
streamed request limits, JWT redaction, repository discovery authentication,
outbox event filtering, operation completion, and disconnect replay behavior.

Validation on 9 October 2026: all 98 manager tests passed with ignored
PostgreSQL tests enabled; Clippy passed with warnings treated as errors;
`git diff --check` passed. The real GitHub App smoke test below was not run.

## Release smoke test

These steps need a real configured GitHub App and are separate from local
automated acceptance:

1. Connect a personal installation and an organization-owned installation;
   verify both callback orders and reject a different callback session user.
2. Verify owner authority, immutable cross-organization binding, and repository
   discovery across multiple pages.
3. Deliver signed installation events; verify deduplication, suspension,
   repository removal, and recovery after a manager restart.
4. Disconnect and verify access stays disabled after stale add/unsuspend events.
   Reconnect explicitly, then replay the earlier disconnect idempotency key;
   the new connection must remain active.
5. Verify reconcile and disconnect operation status reaches `succeeded`.

Do not mark the release smoke test complete based solely on fixture tests.
The original WP-03 specification referenced by the source is not present in
this checkout, so this checklist records the acceptance scope used here.
