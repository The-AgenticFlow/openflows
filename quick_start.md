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

OpenFlows agents authenticate to your GitHub repositories (including private repos) through a **GitHub App** using GitHub OIDC / Coder external auth. Creating the app is free.

1. On GitHub, create a new GitHub App.
2. Fill in:
   - **GitHub App name** — this becomes your app's URL slug (e.g. `my-openflows-app`).
   - **Homepage URL** — GitHub requires a value here, but it's not used for a local setup. Put any URL you own, e.g. your repo page or `http://localhost:7080`.
   - **Redirect URI** (under "Identifying and authorizing users") — this is the callback Coder expects. Enter exactly:
     ```
     http://localhost:7080/external-auth/primary-github/callback
     ```
   - **Webhook** — can be left **Active: false** (blank).
   - **Permissions → Repository permissions**, set the following to **Read and write** (note: **Metadata** stays **Read-only** — GitHub does not allow raising it, so it is not part of the read/write instruction):
     | Permission | Why |
     |------------|-----|
     | **Contents** | Clone/push repo code (required for private repos). |
     | **Pull requests** | Open/update PRs via the API. |
     | **Workflows** | Push/create `.github/workflows/` files (e.g. the "set up CI" ticket type). |
     | **Metadata** | Read-only — auto-required by GitHub for repo access. |
   - **Where can this GitHub App be installed?** — choose **Any account** (or limit to specific orgs).
3. Click **Create GitHub App**.
4. On the app's page, copy the **Client ID** (top of the page).
5. In the **Client secrets** section, click **Generate a new client secret** and copy it now — it is shown only once.
6. Your **Install URL** is `https://github.com/apps/<your-app-slug>/installations/new`. Replace `<your-app-slug>` with the app's **URL slug** — the value that appears in the app's page URL (e.g. `my-openflows-app`). The slug can differ from the display name if it contains spaces or capitals; copy it from the app page URL to be safe. This is how you (and your agents) install the app on your org/repos.

> **If you add permissions *after* installing the app:** editing the app's developer-settings permissions only *requests* the new permission — you must then **approve/update the installation** at `https://github.com/settings/installations` (open your app's installation and click **Approve/Update**), then get a **fresh token** (restart the workspace). Changing an app's permissions does **not** update already-issued tokens. The most common blockers:
> - *"refusing to allow a GitHub App to create or update workflow … without workflows permission"* → grant **Workflows** (≠ **Actions**) and approve the installation.
> - *"Resource not accessible by integration"* when creating a PR → grant **Pull requests** (read/write) and approve the installation.

Keep the Client ID, Client Secret, and Install URL — you'll need all three in the next step.

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
| `GITHUB_REPOSITORY` | The repo the controller watches, as `owner/repo`. |
| `CODER_SESSION_TOKEN` | Leave empty for now — you'll fill it in [Step 5](#step-5--get-your-coder-session-token). |

Then set the three GitHub external auth values in `.env` from [Step 1](#step-1--create-a-github-app):

```bash
CODER_EXTERNAL_AUTH_0_ID=primary-github
CODER_EXTERNAL_AUTH_0_TYPE=github
CODER_EXTERNAL_AUTH_0_CLIENT_ID=<your-github-app-client-id>
CODER_EXTERNAL_AUTH_0_CLIENT_SECRET=<your-github-app-client-secret>
CODER_EXTERNAL_AUTH_0_SCOPES=repo
CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL=https://github.com/apps/<your-app-slug>/installations/new
```

> **Why now?** The Coder container reads these vars from `.env` when it starts (see `docker-compose.yml`). Setting them here **before** starting Docker means Coder comes up with GitHub App auth already wired — no UI editing, and no risk of Coder failing to start with empty credentials.

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

> **Why this is required:** the `CODER_EXTERNAL_AUTH_0_*` vars only *configure* the provider. The actual grant happens when you complete this link — until then Coder has no token to give your agents. If you skip it, bootstrap later fails with `403 External authentication is required to create a workspace with this template`; see [Troubleshooting](#troubleshooting).

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

Bind a GitHub repo to the controller:

```bash
./scripts/prod.sh tenant <owner/repo> --name <my-team>
```

You'll see the tenant under **http://localhost:7080/workspaces**.

---

## Step 10 — Run the controller

Open a **separate terminal** (the controller runs in the foreground and streams logs) and run:

```bash
./scripts/prod.sh run
```

Create a GitHub issue in the bound repo → OpenFlows automatically assigns it, provisions a workspace, and starts working.

---

## Verify it's working

In a separate terminal:

```bash
./scripts/prod.sh doctor
```

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
