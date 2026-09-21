# OpenFlows — Operator Guide (Production Serving, Model A)

This guide is for **operators running a deployed SaaS that serves per-trial users**
("Model A"). Each trial user links their own GitHub account and the OpenFlows
controller drives their repo with **that user's external-auth token**.

> For **local development only**, use [`quick_start.md`](quick_start.md). It uses a
> local `docker compose` stack and the GitHub App.

## Model A at a glance

OpenFlows runs a Coder deployment. Each tenant is one trial user's repo. That user:

1. **Installs the OpenFlows GitHub App** on the private repo/org they want monitored
   (or grants it repo access).
2. **Links their GitHub account** to their tenant's Coder user (the external-auth
   link). `tenant add` walks them through it with a device-flow one-time code.
3. **Provides the `owner/repo`** to watch.

The GitHub App is the sole source of the GitHub token — no operator `GITHUB_TOKEN`/
PAT is needed. The controller resolves each tenant's token on the fly.

## 1. Prerequisites

- A reachable Coder server (the operator provisions and monitors it).
- The GitHub App (external auth) configured on Coder: `CODER_EXTERNAL_AUTH_0_*`.
- Per-trial users with the ability to install the App and link GitHub.

## 2. Configure the operator environment

Required vars:

| Variable | Purpose |
|----------|---------|
| `CODER_URL` | Coder server base URL (e.g. `https://coder.example.com`) |
| `CODER_SESSION_TOKEN` | Operator/provisioning token for Coder |
| `CODER_CHAT_HOOK_SECRET` | Shared signing secret for lifecycle hooks (32+ random bytes) |
| `CODER_EXTERNAL_AUTH_0_ID` | External-auth provider id (default `primary-github`) |
| `CODER_EXTERNAL_AUTH_0_CLIENT_ID` | GitHub App client id |
| `CODER_EXTERNAL_AUTH_0_CLIENT_SECRET` | GitHub App client secret |
| `CODER_EXTERNAL_AUTH_0_SCOPES` | Requested scopes (default `repo`) |
| `CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL` | GitHub App install URL surfaced to users |

## 3. Bootstrap the deployment

```bash
./scripts/prod.sh bootstrap
```

This creates the admin user, pushes the workspace templates, and verifies GitHub/LLM
auth. Confirm templates under the Coder dashboard → Templates.

## 4. Onboard a tenant (self-serve)

```bash
./scripts/prod.sh tenant <owner/repo> --name <trial-team>
```

`tenant add` now onboards self-serve:

1. Creates the tenant-owner Coder user.
2. Checks (via the Coder API, as that user) whether GitHub is linked; if not, prints
   a **device-flow one-time code** and link URL to authorize, plus the GitHub App
   install URL.
3. Once GitHub is linked, mints a tenant token and provisions the
   `openflows-nexus-<tenant>` workspace, which auto-starts its controller.

The controller then reads issues and opens PRs on the trial's private repo using the
tenant's linked external-auth token.

## 5. Token resolution (single source)

The controller and workspace startup resolve one GitHub token, sourced solely from the
tenant's linked **Coder external-auth token**:
`CODER_EXTERNAL_AUTH_<ID>_{ACCESS_TOKEN|TOKEN|TOKEN_FILE}` — where `<ID>` is the
provider id (default `primary-github`) uppercased. `<ID>` is derived from
`CODER_EXTERNAL_AUTH_0_ID`. Each tenant's token is used for its own repo.

## 6. Monitor & operations

- Each tenant gets its own nexus workspace + controller (per-tenant Redis namespaces).
- Revoking GitHub in the tenant's Coder dashboard removes controller API access (same
  trust model as workspace git).
- Check tenant workspaces under Coder dashboard → Workspaces.

## 7. Reference

- [`token_guide.md`](token_guide.md) — token acquisition + precedence.
- [`quick_start.md`](quick_start.md) — local development setup.
