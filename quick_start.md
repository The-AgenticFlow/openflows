# OpenFlows — Quick Start

Get OpenFlows running on a fresh machine in 10 steps. For what OpenFlows is and how it works, see the [README](README.md).

> **Working directory:** all commands run from the **project root** (the directory containing `docker-compose.yml`). No need to `cd` into subdirectories.

## Contents

- [Prerequisites](#prerequisites)
- [Step 1 — Create a GitHub App](#step-1--create-a-github-app)
- [Step 2 — Set up .env](#step-2--set-up-env)
- [Step 3 — Start Docker](#step-3--start-docker)
- [Step 4 — Sign in with GitHub](#step-4--sign-in-with-github)
- [Step 5 — Get your Coder session token](#step-5--get-your-coder-session-token)
- [Step 6 — Configure an LLM model](#step-6--configure-an-llm-model)
- [Step 7 — Test the AI setup](#step-7--test-the-ai-setup)
- [Step 8 — Bootstrap](#step-8--bootstrap)
- [Step 9 — Add a tenant](#step-9--add-a-tenant)
- [Step 10 — Run the controller](#step-10--run-the-controller)
- [Verify it's working](#verify-its-working)
- [Lifecycle hooks](#lifecycle-hooks)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [More](#more)

---

## Prerequisites

- Docker 24+
- Rust 1.70+ (builds the `openflows` binary during bootstrap)
- The `coder` CLI on your `PATH` — bootstrap shells out to `coder templates push`:
  ```bash
  curl -fsSL https://coder.com/install.sh | sh
  ```
- A GitHub account.

---

## Step 1 — Create a GitHub App

OpenFlows agents authenticate to your GitHub repos (including private ones) through a GitHub App. Create a new one at **https://github.com/settings/apps/new**:

1. Set the **Redirect URI** (under *Identifying and authorizing users*) to exactly:
   ```
   http://localhost:7080/external-auth/primary-github/callback
   ```
2. Under **Permissions → Repository permissions**, set **Contents**, **Pull requests**, and **Workflows** to **Read and write**.
3. Create the app, then **Install it** on your org via your install URL (`https://github.com/apps/<your-app-slug>/installations/new`).
4. Copy three values for [Step 2](#step-2--set-up-env): the **Client ID**, a generated **Client Secret** (shown once), and your **Install URL**.

> If you change the app's permissions after installing, re-open the installation at **https://github.com/settings/installations** and click **Approve/Update**, then get a fresh token — otherwise the new permissions don't take effect.

---

## Step 2 — Set up `.env`

Create your `.env` from the template:

```bash
cp .env.example .env
```

Fill in the required values:

| Variable | What to put |
|----------|-------------|
| `GITHUB_TOKEN` | GitHub PAT with `repo` scope. |
| `CODER_CHAT_HOOK_SECRET` | The shared signing secret for lifecycle hooks. Generate 32+ random bytes: `openssl rand -hex 32`. The bundled stack requires it before enabling hooks (see [Lifecycle hooks](#lifecycle-hooks)). |
| `CODER_SESSION_TOKEN` | Leave empty for now — you'll fill it in [Step 5](#step-5--get-your-coder-session-token). |

> **Note:** The target repo is **not** configured in `.env`. It is bound per-tenant in [Step 9](#step-9--add-a-tenant) via `./scripts/prod.sh tenant <owner/repo> --name <team>`. Each tenant gets its own nexus workspace and controller scoped to that repo.

Then set the three GitHub external auth values in `.env` from [Step 1](#step-1--create-a-github-app):

```bash
CODER_EXTERNAL_AUTH_0_ID=primary-github
CODER_EXTERNAL_AUTH_0_TYPE=github
CODER_EXTERNAL_AUTH_0_CLIENT_ID=<your-github-app-client-id>
CODER_EXTERNAL_AUTH_0_CLIENT_SECRET=<your-github-app-client-secret>
CODER_EXTERNAL_AUTH_0_SCOPES=repo
CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL=https://github.com/apps/<your-app-slug>/installations/new
```

> **Set these before starting Docker** — Coder reads them from `.env` at startup, and won't start with empty GitHub App credentials.

---

## Step 3 — Start Docker

First, make sure ports 6379 (Redis) and 7080 (Coder) are free so there's no conflict. If another, unrelated container is already holding one of those ports, find and remove only that one by name:

```bash
docker ps --filter "publish=6379" --filter "publish=7080"
docker rm -f <conflicting-container-name>
```

Then bring up Redis, the Coder database, and the Coder server:

```bash
docker compose up -d
```

Wait until all three report healthy:

```bash
docker compose ps
```

> **Next:** visit your app's **Install URL** and install the GitHub App on your org/repos so the app can access them.

---

## Step 4 — Sign in with GitHub

Open **http://localhost:7080** and sign in with your GitHub account (Coder's device flow).

To confirm the GitHub App was set up correctly, open **http://localhost:7080/deployment/external-auth** — you should see a row with **ID `primary-github`** (with its Client ID and Match). If the row is missing, the external auth vars weren't picked up; see the troubleshooting note below.

### Link the GitHub App (required for private repos)

Signing in is **not** enough. You must also **link** the GitHub App so Coder can hand your agents a token to clone/push your repos (including private ones):

1. Make sure you are logged into the Coder dashboard **as the account that owns the workspaces** — the one whose session token you put in `CODER_SESSION_TOKEN` (bootstrap creates the control-plane workspace under this account).
2. Visit:
   ```
   http://localhost:7080/external-auth/primary-github
   ```
3. On GitHub, click **Authorize**. You'll be redirected back to Coder once linked.

> Required for private repos: skipping this link makes bootstrap fail with `403 External authentication is required to create a workspace with this template`; see [Troubleshooting](#troubleshooting).

---

## Step 5 — Get your Coder session token

1. Open **http://localhost:7080/settings/tokens**
2. Click **Create Token**, copy it.
3. Paste it into `.env`:
   ```bash
   CODER_SESSION_TOKEN=your_token_here
   ```

---

## Step 6 — Configure an LLM model

OpenFlows agents need at least one model.

1. Go to **http://localhost:7080/ai/settings/providers**
2. **Add a provider** (e.g. OpenAI, Anthropic).
3. Go to **http://localhost:7080/ai/settings/models** and **add a model** to that provider (e.g. `deepseek-v4-flash-0731`).

---

## Step 7 — Test the AI setup

Open **http://localhost:7080/agents** and confirm agents/models show up. Say "hello" in the chat to verify the model responds.

---

## Step 8 — Bootstrap

Run the one-time setup to initialize Coder with the OpenFlows templates and config:

```bash
./scripts/prod.sh bootstrap
```

This builds the `openflows` binary into `.dev-binaries/`, creates the admin user, pushes the workspace templates, and verifies GitHub/LLM auth.

Confirm the templates were pushed at **http://localhost:7080/templates**.

---

## Step 9 — Add a tenant

Bind a GitHub repo to OpenFlows. A tenant is scoped to a single `owner/repo` and provisions its own nexus workspace + controller:

```bash
./scripts/prod.sh tenant <owner/repo> --name <my-team>
```

You'll see the tenant's nexus workspace under **http://localhost:7080/workspaces**.

> **Note:** Each tenant is isolated (per-tenant Redis namespaces, separate workspaces/controllers). The model supports multiple tenants; running several concurrently is part of the design and still being validated — start with one tenant per controller host for now.

> **Upgrading from an earlier setup?** Tenant workspaces created before this change were built with `start_controller=false` and are returned unchanged if you re-run `tenant add`. Recreate an existing tenant's workspace **once** to pick up `start_controller=true` (the controller then auto-starts inside it). New tenants get this automatically — nothing extra to do.

---

## Step 10 — Let the controller run

The controller runs **inside the tenant's nexus workspace** and auto-starts when the workspace is ready. You don't run it on your machine — the workspace was created with `start_controller` enabled, and its `GITHUB_REPOSITORY`/`OPENFLOWS_TENANT` are injected from the tenant you added.

> For local development/debugging only, you can still run the controller manually on the host with `./scripts/prod.sh run` (this is not the production path).

Create a GitHub issue in the bound repo → OpenFlows automatically assigns it, provisions a workspace, and starts working.

---

## Verify it's working

In a separate terminal:

```bash
./scripts/prod.sh doctor
```

---

## Lifecycle hooks

Lifecycle hooks let OpenFlows observe and steer agent behaviour as it happens. The bundled stack wires them automatically — you only provide the shared signing secret in [Step 2](#step-2--set-up-env). Do not set the Coder hook experiment/URL/bind variables manually.

### Verify hooks are working

After [Step 10](#step-10--run-the-controller), confirm the consumer started and the stack is not logging `InvalidAudience`. For explicit confirmation, set `OPENFLOWS_HOOK_LOGS=true` in `.env`, restart the controller, and watch for:

```text
INFO ... Coder lifecycle hook consumer started
```

You can also exercise the consumer directly with the `openflows` binary's simulate command (run it from a shell that can reach the consumer, using the same secret):

```bash
export CODER_CHAT_HOOK_SECRET=<the same secret you put in .env>
export CODER_CHAT_HOOK_URL=http://openflows-nexus:3001/experimental/hooks/chat
./.dev-binaries/openflows hooks simulate --event user_prompt_submit --chat-id chat-verify
```

A healthy consumer responds `200`. If you see `InvalidAudience`, see [Troubleshooting](#audience-mismatch-invalidaudience).

### Rotating the secret

`CODER_CHAT_HOOK_SECRET` is an **immutable** Nexus workspace parameter: Coder captures it when the workspace is built, and re-running bootstrap alone **does not** update an existing workspace (bootstrap only rebuilds the Nexus workspace when the template itself changes). To rotate, you must recreate the workspace so it is rebuilt with the new secret:

1. Stop the Controller.
2. Set a new 32+ byte `CODER_CHAT_HOOK_SECRET` in `.env`.
3. Delete the existing Nexus workspace so bootstrap rebuilds it. You can do this from the host with the `coder` CLI (logged in as the OpenFlows user):
   ```bash
   coder delete openflows-nexus
   ```
   (If you changed `OPENFLOWS_HOOK_URL` at the same time, do the same — the hook URL is also immutable.)
4. Re-run bootstrap. It recreates the workspace with the new secret and the controller starts validating against it.

Coder and the consumer must always share the same secret, and both Coder and the Controller must be restarted after rotation.

---

## Configuration

These are optional — the defaults work out of the box. Only touch them if you need to.

| Variable | Default | Notes |
|----------|---------|-------|
| `CODER_ADMIN_USERNAME` | `admin` | Admin account created by bootstrap. |
| `CODER_ADMIN_EMAIL` | `admin@openflows.dev` | |
| `CODER_ADMIN_PASSWORD` | `Op3nFl0ws!` | Must be ≥8 chars with upper, lower, digit, and special char — otherwise bootstrap silently falls back to the default. |
| `REDIS_URL` | `redis://localhost:6379` | Set only if you host Redis elsewhere. |
| `CODER_URL` | `http://localhost:7080` | Set only if you host Coder elsewhere. |
| `OPENFLOWS_TENANT` | `default` | Namespace for Redis keys. |
| `CODER_CHAT_HOOK_SECRET` | required | Generate 32+ random bytes, for example `openssl rand -hex 32`; hook URL/experiment/bind values are wired automatically. |
| `OPENFLOWS_HOOK_URL` | `http://openflows-nexus:3001/experimental/hooks/chat` | Single source of truth for the hook endpoint, shared between Coder and the consumer. The consumer picks it up via bootstrap; after changing it on an existing deployment, recreate the Nexus workspace (see [Rotating the secret](#rotating-the-secret)). |
| `OPENFLOWS_HOOK_LOGS` | `false` | Set `true` only when debugging lifecycle hook traffic. |
| `SLACK_WEBHOOK_URL` / `DISCORD_WEBHOOK_URL` | unset | Escalation notifications. |

### Granting a non-admin (OAuth) user the needed permissions

When a team member signs in with GitHub OAuth, Coder creates them as a **regular member**, who can't create workspaces or push templates. If you want OpenFlows to run as that user, grant them these roles (or bootstrap fails with `403 Unauthorized to create workspace`):

| Role | Why |
|------|-----|
| `organization-admin` | Create the control-plane workspace + template management. |
| `organization-template-admin` | Push/update the `openflows-*` templates. |
| `organization-workspace-access` | Required for org workspaces. Keep it — `edit-roles` replaces the whole role set. |

> **Trap:** `organization-workspace-creation-ban` carries a *negative* `workspace:create` permission that **overrides** `organization-admin`. If you see `403 Unauthorized to create workspace`, make sure this role is **not** assigned.

Via CLI:

```bash
export CODER_URL=http://localhost:7080
export CODER_SESSION_TOKEN=<your-token>

# List orgs, then grant roles (include ALL existing roles or they'll be removed)
coder organizations list
coder organizations members edit-roles -O=<org> <username> \
  organization-admin \
  organization-template-admin \
  organization-workspace-access
```

Or via the dashboard: **Admin settings → Organizations → `<your org>` → Members → Edit roles** and select the roles above.

---

## Troubleshooting

### `Failed to run coder templates push` (during bootstrap)

`coder` is missing or not on your `PATH`. Install it and re-run bootstrap:

```bash
curl -fsSL https://coder.com/install.sh | sh
coder version
```

### "No LLM models configured in Coder"

Open **http://localhost:7080/ai/settings/providers** and add a provider/model, then re-run bootstrap.

### `cp: cannot create regular file '.dev-binaries/openflows': Permission denied`

The `.dev-binaries/` directory is root-owned:

```bash
sudo chown -R "$USER":"$USER" .dev-binaries/
```

### Port 6379 already in use

Another process/container holds port 6379. Stop or remove the conflicting container, or change the Redis port mapping in `docker-compose.yml`.

### Coder fails to start / external auth not showing in the UI

Confirm the external auth vars are set in `.env` **before** running `docker compose up -d`, then restart Coder:

```bash
docker compose restart coder
```

Then verify the provider at **http://localhost:7080/external-auth** (or the admin external-auth page).

### Agents can't push `.github/workflows/` files ("without workflows permission")

Grant the GitHub App **Workflows → Read and write** (this is a *different* permission from **Actions**), then **approve/update the installation** and get a fresh token — see the note in [Step 1](#step-1--create-a-github-app).

### Agents can't create a PR ("Resource not accessible by integration")

Grant the GitHub App **Pull requests → Read and write**, then **approve/update the installation** and get a fresh token — see the note in [Step 1](#step-1--create-a-github-app).

### Controller not picking up issues

1. Confirm a tenant is bound (`./scripts/prod.sh tenant <owner/repo> --name <my-team>`).
2. Watch the controller's foreground terminal for errors.
3. Verify Coder is reachable: `curl http://localhost:7080/api/v2/buildinfo`.

### Chat creation fails with 502 / `Lifecycle hook dispatch ... failed (http_error)`

This usually means the hook consumer rejected or couldn't be reached during Coder Chat creation. The most common specific cause is the one below (`InvalidAudience`). Other possibilities: the consumer isn't running (no `CODER_CHAT_HOOK_SECRET` set), or the Coder container can't reach the consumer endpoint. Confirm the secret is set in `.env` (Step 2), the controller is running, and — for a custom topology — that the hook URL is reachable from the Coder container.

### Audience mismatch (`InvalidAudience`)

If the controller logs `Hook consumer: JWT verification failed ... InvalidAudience`, the JWT's `aud` claim (Coder's `CODER_CHAT_HOOK_URL`) does not match the audience the consumer expects. In the bundled stack this should not happen — `OPENFLOWS_HOOK_URL` drives both Coder's URL and the consumer's expected `aud` via bootstrap. It appears when those two get out of sync:

- The hook URL is set on a **custom** deployment via `OPENFLOWS_HOOK_URL`, but the **Nexus workspace was not recreated** after the URL changed. The hook URL is an immutable workspace parameter, so an existing workspace keeps validating against the audience it was originally built with even after you change `OPENFLOWS_HOOK_URL`. Recreate the workspace (see [Rotating the secret](#rotating-the-secret)) so bootstrap rebuilds it against the current URL, or keep the bundled defaults.
- Coder and the consumer were started with different `CODER_CHAT_HOOK_SECRET` values or one was restarted out of order. Restart both with the same secret.

### `403 External authentication is required to create a workspace with this template`

Coder refuses to build a workspace until the owning account links the GitHub App (workspaces that request GitHub access require the owner to authenticate with it). Fix it by completing the link in [Step 4](#step-4--sign-in-with-github) — sign in as the workspace owner and visit `http://localhost:7080/external-auth/primary-github`, then **Authorize** on GitHub. Afterwards, re-run bootstrap.

### Agents can't clone/push the private repo

Even with the provider configured, git access only works when all three are true:

1. The workspace owner has **linked** the GitHub App (see [Step 4](#step-4--sign-in-with-github)).
2. The GitHub App is **installed** on the account/org that owns the repo, with access to it (`https://github.com/apps/<your-app-slug>/installations/new`).
3. The App is set to **public** (App → Advanced → "Make this GitHub App public") so other accounts can link it.

Verify inside a workspace **without printing the token**: `test -s ~/.git-credentials && echo 'git credentials configured'` (or run `git ls-remote <your-repo-url>` to confirm auth works). **Never** `cat ~/.git-credentials` — it prints your live GitHub token to the terminal/session logs.

---

## More

- **Full docs:** [README.md](README.md)
- **Testing & debugging:** [testing_quick_start.md](testing_quick_start.md)
- **Token acquisition:** [token_guide.md](token_guide.md)
- **Lifecycle hooks (design):** [docs/experiments/hook-driven-state-derivation.md](docs/experiments/hook-driven-state-derivation.md) and [docs/experiments/coder-lifecycle-hooks-feedback.md](docs/experiments/coder-lifecycle-hooks-feedback.md)
