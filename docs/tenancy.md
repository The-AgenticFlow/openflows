# Tenancy

OpenFlows supports multiple teams on a single self-hosted Coder server.

## Model

A **tenant** is:
1. A **real Coder user** ("tenant owner") — not a service account
2. A **GitHub repo binding** — the repo this tenant works on
3. An **`openflows-nexus` workspace** — owned by the tenant user, running the Controller

All agent actions (PRs, commits, merges) are attributed to the tenant owner's GitHub identity via Coder external auth.

## Isolation

| Layer | Mechanism |
|-------|-----------|
| Coder workspaces | Each tenant's workspaces are owned by their user — Coder RBAC prevents cross-tenant access |
| Coder chats | Chats are owned by the tenant user; labeled with `tenant` for filtering |
| Redis SharedStore | All keys are prefixed with `ns:{tenant}:` — complete keyspace isolation |
| Templates | All tenants use the same role templates (`openflows-{role}`) but workspaces are separate |

## Adding a Tenant

```bash
openflows tenant add owner/repo --name my-team
```

This:
1. Creates the tenant-owner Coder user (member role, no admin)
2. Prints a GitHub OAuth link for the user to complete in the dashboard
3. Waits for the OAuth grant
4. Mints a scoped session token for that user
5. Creates the `openflows-nexus` workspace under that user

## Removing a Tenant

```bash
openflows tenant remove my-team
```

This archives all chats, deletes all workspaces, and optionally purges the `ns:{tenant}:*` Redis keyspace.

## Future: GitHub App

Currently, all agent actions appear as the tenant owner's GitHub identity. A GitHub App installation per tenant would provide finer-grained attribution and scoped repo access. This is a documented future path, not yet implemented.
