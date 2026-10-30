# OpenFlows — Manual Setup & Reference

Most people only need the [Quick Start](../quick_start.md) (`./scripts/setup.sh` does everything below automatically). Use this page to understand what the script does, run the steps by hand, or look up hooks, roles, configuration and troubleshooting.

The manual GitHub App instructions below describe `OPENFLOWS_GITHUB_AUTH_MODE=self-managed`. In the default connected-local mode, install the team-owned App on the repository, set its public `OPENFLOWS_GITHUB_CLIENT_ID` and `OPENFLOWS_GITHUB_APP_SLUG`, and run `./scripts/setup.sh owner/repo`. The local Manager handles device authorization and token renewal. To sign out, run `cargo run -p openflows-manager -- --config .env github-logout` and stop the local workspaces.

> **Working directory:** run all commands from the **project root** (the directory containing `docker-compose.yml`).

## Contents

- [Prerequisites](#prerequisites)
- [Step 1 — Create a GitHub App](#step-1--create-a-github-app)
- [Step 2 — Set up .env](#step-2--set-up-env)
- [Step 3 — Start Docker](#step-3--start-docker)
- [Step 4 — Sign in with GitHub](#step-4--sign-in-with-github)
- [Step 5 — Get your Coder session token](#step-5--get-your-coder-session-token)
- [Step 6 — Configure an LLM model](#step-6--configure-an-llm-model)
- [Step 7 — Bootstrap](#step-7--bootstrap)
- [Step 8 — Add a tenant](#step-8--add-a-tenant)
- [Step 9 — Let the controller run](#step-9--let-the-controller-run)
- [Verify it's working](#verify-its-working)
- [Install from published images (`--compose`)](#install-from-published-images---compose)
- [Lifecycle hooks](#lifecycle-hooks)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [More](#more)

---

## Prerequisites

- Docker 24+
- Rust 1.70+ (builds the `openflows` binary during bootstrap; not needed with [`--compose`](#install-from-published-images---compose))
- The `coder` CLI on your `PATH` — bootstrap shells out to `coder templates push`:
  ```bash
  curl -fsSL https://coder.com/install.sh | sh
  ```
- A GitHub account.

---

## Step 1 — Create a GitHub App

OpenFlows agents authenticate to your GitHub repos (including private ones) through a GitHub App. Create a new one at **https://github.com/settings/apps/new**:

> **Important:** If your repository belongs to an organization, you must create the GitHub App directly within that organization's settings (`https://github.com/organizations/<your-org>/settings/apps`), not your personal account.

1. Set the **Redirect URI** (under *Identifying and authorizing users*) to exactly:
   ```
   http://localhost:7080/external-auth/primary-github/callback
   ```
2. Under **Permissions → Repository permissions**, set **Checks** to **Read-only**, and **Contents**, **Issues**, **Pull requests**, and **Workflows** to **Read and write**.
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
| `CODER_CHAT_HOOK_SECRET` | The shared signing secret for lifecycle hooks. Generate 32+ random bytes: `openssl rand -hex 32`. The bundled stack requires it before enabling hooks (see [Lifecycle hooks](#lifecycle-hooks)). |
| `CODER_SESSION_TOKEN` | Leave empty for now — you'll fill it in [Step 5](#step-5--get-your-coder-session-token). |

> **Note:** The target repo is **not** configured in `.env`. It is bound per-tenant in [Step 8](#step-8--add-a-tenant) via `./scripts/prod.sh tenant <owner/repo> --name <team> --fleet <N>`. Each tenant gets its own nexus workspace and controller scoped to that repo.

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

Signing in is **not** enough. Each tenant workspace is owned by its **tenant user** (created by `tenant add`), and Coder refuses to build a workspace until the owning account links the GitHub App. The `CODER_SESSION_TOKEN` account is only the privileged **provisioning** actor — the tenant user is the actual workspace owner.

Since `tenant add` performs a **real API-driven grant check** (it polls `GET /api/v2/external-auth/primary-github` until the tenant user's GitHub link is confirmed), you no longer need to pre-link the app manually. When you run `tenant add`, it prints the link URL and (when a device flow is available) a one-time code you can authorize in a browser — complete that and onboarding continues automatically:

1. Run `./scripts/prod.sh tenant <owner/repo> --name <my-team> --fleet <N>`.
2. Onboarding prints a "GitHub Link Required" prompt with either:
   - a **device-flow URL + one-time code** (fastest — open it, enter the code, click **Authorize**), or
   - the Coder dashboard link `http://localhost:7080/external-auth/primary-github` to authorize as the tenant user.
3. Once you authorize, `tenant add` confirms the linked GitHub account (`user.login`) and proceeds to create the tenant workspace automatically — no need to press Enter.

> Do **not** link the GitHub App only as the `CODER_SESSION_TOKEN` (admin/provisioning) account — that account provisions the workspace but does not own it, so tenant workspace creation would still be blocked by Coder's external-auth `403`.

> Required for private repos: skipping this link makes bootstrap/`tenant add` time out (or fail with `403 External authentication is required to create a workspace with this template`); see [Troubleshooting](#troubleshooting).

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

## Step 7 — Bootstrap

Run the one-time setup to initialize Coder with the OpenFlows templates and config:

```bash
./scripts/prod.sh bootstrap
```

This builds the `openflows` binary into `.dev-binaries/`, creates the admin user, pushes the workspace templates, and verifies GitHub/LLM auth. With the published images, the programs come from the image instead; see [Install from published images](#install-from-published-images---compose).

Confirm the templates were pushed at **http://localhost:7080/templates**.

---

## Step 8 — Add a tenant

Bind a GitHub repo to OpenFlows. A tenant is scoped to a single `owner/repo` and provisions its own nexus workspace + controller:

```bash
./scripts/prod.sh tenant <owner/repo> --name <my-team> --fleet 3
```

`--fleet N` sets the number of **FORGE-SENTINEL pairs** for the tenant: `N` forge worker slots **and** `N` sentinel worker slots (a fleet of `3` → `forge-1..forge-3` and `sentinel-1..sentinel-3`). It is **mandatory** and must be >= 1. The fleet value is written into the tenant's `registry.json` (also persisted to the tenant store), which the in-workspace controller reads and applies automatically.

`tenant add` now onboards **self-serve**:

1. Creates the tenant-owner Coder user.
2. Checks (via the Coder API, as that user) whether GitHub is linked, and — if not — prints a device-flow **one-time code** and link URL to authorize.
3. Once GitHub is linked, mints a tenant token and provisions the `openflows-nexus-<tenant>` workspace, which auto-starts its controller.

You'll see the tenant's nexus workspace under **http://localhost:7080/workspaces**.

> **GitHub auth:** the GitHub App (Step 1) is the sole source of the GitHub token — each tenant links their GitHub account during `tenant add`, and the controller/agents use that linked token. No PAT is required.

> **Note:** Each tenant is isolated (per-tenant Redis namespaces, separate workspaces/controllers). The model supports multiple tenants; running several concurrently is part of the design and still being validated — start with one tenant per controller host for now.

---

## Step 9 — Let the controller run

The controller runs **inside the tenant's nexus workspace** and auto-starts when the workspace is ready. You don't run it on your machine — the workspace was created with `start_controller` enabled, and its `GITHUB_REPOSITORY`/`OPENFLOWS_TENANT` are injected from the tenant you added.

> **Note:** There is no host-side `run` command — the controller runs **inside** the tenant's nexus workspace and auto-starts when the workspace is ready (started by `tenant add`). Running the controller manually on the host has been removed as a duplicate of that in-workspace auto-start.

Create a GitHub issue in the bound repo → OpenFlows automatically assigns it, provisions a workspace, and starts working.

---

## Verify it's working

In a separate terminal:

```bash
./scripts/prod.sh doctor
```

---

## Install from published images (`--compose`)

`./scripts/setup.sh owner/repo --compose` installs OpenFlows without Rust or compiling. Every step above stays the same except where the programs come from:

1. Setup picks the version (`OPENFLOWS_VERSION`, or the latest release when empty), pulls `ghcr.io/the-agenticflow/openflows:<version>` and saves the image's digest in `.env`.
2. Setup starts the stack with `docker-compose.images.yml` added (`docker compose -f docker-compose.yml -f docker-compose.images.yml up -d`), whose `openflows-binaries` service which copies `openflows`, `openflows-doctor` and `openflows-harness` from the image into `.dev-binaries/` (owned by your user) and records the exact image in `.dev-binaries/IMAGE`.
3. Bootstrap pushes the templates without building (`prod.sh --skip-build bootstrap`), and the workspaces load the programs from `.dev-binaries/` as they do for a source install.

Setup remembers the mode in `.env`, so later runs without a flag keep using the images.

Compose installs use your own GitHub App (`OPENFLOWS_GITHUB_AUTH_MODE=self-managed`, the steps above). The team's shared App (`connected-local`) still builds and runs the local Manager from source, so it needs Rust and is not available with `--compose` yet.

### Settings

| Variable | Set by | Meaning |
| --- | --- | --- |
| `OPENFLOWS_VERSION` | You, or setup (latest release) | Version to install, e.g. `1.8.1` |
| `OPENFLOWS_IMAGE_DIGEST` | Setup | Fingerprint (`sha256:…`) of that version's image. A tag can be moved by a rebuild; the digest always names the same image |
| `OPENFLOWS_HOST_UID`, `OPENFLOWS_HOST_GID` | Setup | Your user, so the copied programs are owned by you |
| `OPENFLOWS_SETUP_MODE` | Setup | `compose` or `source`, reused when you run setup without a flag |

### Checking the digest

Each release's notes list its image digests under **Docker images**. Compare them with `OPENFLOWS_IMAGE_DIGEST` in your `.env`, or with the image your workspaces use:

```bash
cat .dev-binaries/IMAGE
```

### Upgrade and rollback

1. Make sure the stack and the workspaces are running (`docker compose start`). Setup waits for the tenant's workspace in its last step.
2. In `.env`, set the version and clear the digest:
   ```bash
   OPENFLOWS_VERSION=1.8.1
   OPENFLOWS_IMAGE_DIGEST=
   ```
3. Re-run `./scripts/setup.sh`. It pulls that version, pins its digest, copies the programs and pushes the templates.
4. Restart the workspaces. Each one copies the programs when it starts, so running workspaces keep the old version until restarted. Setup lists them and offers to restart them (default: no, since a restart briefly pauses work in progress). To do it later:
   ```bash
   coder restart openflows-nexus-<tenant> --yes
   ```

Rolling back is the same with an older version.

### Templates always match the programs

The workspace templates are built into the `openflows` program, so the image's program pushes that release's templates, whatever the checkout contains. Programs and templates always come from the same release.

### Switching between source and compose

Run setup with the other flag. `--source` rebuilds the programs from the checkout and replaces the ones copied from an image; `--compose` replaces compiled programs with the image's. Accept setup's offer to restart the workspaces, or restart them afterwards.

### Host requirements

Compose needs an **x86-64 Linux host with glibc 2.34 or newer** (e.g. Ubuntu 22.04+, Debian 12+). The image's programs are x86-64 Linux builds linked against glibc. Workspaces always run `linux/amd64`, and setup also runs `openflows` directly on your machine to push templates, add the tenant and run the health check. A source install builds a native copy for that; a compose install has only the image's, so on macOS, ARM64 hosts or older glibc `--compose` stops before starting anything and suggests `--source`.

### Two installs on one machine

`docker-compose.yml` names the project `openflows`, so a second checkout would take over the first one's containers. Give the second install its own project and network in its `.env` before running setup:

```bash
COMPOSE_PROJECT_NAME=openflows-test
TF_VAR_docker_network=openflows-test_default
```

Both installs use ports 7080, 6379 and 7081, so run one at a time.

---

## Lifecycle hooks

Lifecycle hooks let OpenFlows observe and steer agent behaviour as it happens. The bundled stack wires them automatically — you only provide the shared signing secret in [Step 2](#step-2--set-up-env). Do not set the Coder hook experiment/URL/bind variables manually.

### Verify hooks are working

After [Step 9](#step-9--let-the-controller-run), confirm the consumer started and the stack is not logging `InvalidAudience`. For explicit confirmation, set `OPENFLOWS_HOOK_LOGS=true` in `.env`, restart the controller, and watch for:

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

`CODER_CHAT_HOOK_SECRET` is an **immutable** tenant workspace parameter: Coder captures it when the workspace is built, and re-running bootstrap alone **does not** update an existing workspace (bootstrap only pushes templates; it no longer creates a workspace). To rotate, you must **recreate the tenant workspace** so it is rebuilt with the new secret:

1. Stop the tenant's Controller.
2. Set a new 32+ byte `CODER_CHAT_HOOK_SECRET` in `.env`.
3. Delete the tenant's existing Nexus workspace so it can be rebuilt. From the host with the `coder` CLI, logged in as the tenant user:
   ```bash
   coder delete openflows-nexus-<tenant>
   ```
   (If you changed `OPENFLOWS_HOOK_URL` at the same time, do the same — the hook URL is also immutable.)
4. Recreate the workspace through the tenant flow, which rebuilds it with the new secret and restarts its controller:
   ```bash
   ./scripts/prod.sh tenant <owner/repo> --name <tenant>
   ```

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
| `CODER_PORT` | `7080` | Host port for Coder. Change if the default is taken. |
| `OPENFLOWS_LLM_PROVIDER` / `OPENFLOWS_LLM_API_KEY` / `OPENFLOWS_LLM_MODEL` | unset | Read by `./scripts/setup.sh` for non-interactive LLM setup (`anthropic`, `openai`, `openrouter`, `google`). |
| `CODER_MAX_TOKEN_LIFETIME` / `CODER_DEFAULT_TOKEN_LIFETIME` | `2160h` | Coder API token lifetime cap and default (Coder's own default is 7 days). Applied to a running stack on the next `docker compose up -d`. |
| `OPENFLOWS_TENANT` | `default` | Namespace for Redis keys. |
| `CODER_CHAT_HOOK_SECRET` | required | Generate 32+ random bytes, for example `openssl rand -hex 32`; hook URL/experiment/bind values are wired automatically. |
| `OPENFLOWS_HOOK_URL` | `http://openflows-nexus:3001/experimental/hooks/chat` | Single source of truth for the hook endpoint, shared between Coder and the consumer. The consumer picks it up via bootstrap; after changing it on an existing deployment, recreate the Nexus workspace (see [Rotating the secret](#rotating-the-secret)). |
| `OPENFLOWS_HOOK_LOGS` | `false` | Set `true` only when debugging lifecycle hook traffic. |
| `SLACK_WEBHOOK_URL` / `DISCORD_WEBHOOK_URL` | unset | Escalation notifications. |

### Granting a non-admin (OAuth) user the needed permissions

When a team member signs in with GitHub OAuth, Coder creates them as a **regular member**, who can't create workspaces or push templates. If you want OpenFlows to run as that user, grant them these roles (or bootstrap fails with `403 Unauthorized to create workspace`):

| Role | Why |
|------|-----|
| `organization-admin` | Provision workspaces (e.g. tenant nexus workspaces) + template management. |
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

### My `CODER_SESSION_TOKEN` stopped working

Tokens created by `setup.sh` last 90 days (`CODER_MAX_TOKEN_LIFETIME` / `CODER_DEFAULT_TOKEN_LIFETIME`, default `2160h`, set in `docker-compose.yml`). Re-run `./scripts/setup.sh` — it refreshes the token (silently if the password admin login is saved in `.env`, otherwise it walks you through creating a new one).

The token is also copied into each tenant's nexus workspace when the tenant is created, and that copy is **not** refreshed. After rotating the token, delete the tenant's nexus workspace and add the tenant again.

### Changed `--fleet` or the repo for an existing tenant

`tenant add` leaves an existing tenant workspace unchanged, so a new `--fleet` does not apply until you delete that tenant's nexus workspace and add the tenant again. `setup.sh` also stops when a tenant name is already bound to a different repo, and prints how to fix it. The default tenant name is `owner-repo`; pass `--name` to choose another.

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

- The hook URL is set on a **custom** deployment via `OPENFLOWS_HOOK_URL`, but the **tenant workspace was not recreated** after the URL changed. The hook URL is an immutable workspace parameter, so an existing workspace keeps validating against the audience it was originally built with even after you change `OPENFLOWS_HOOK_URL`. Recreate the tenant workspace (see [Rotating the secret](#rotating-the-secret)) so it is rebuilt against the current URL, or keep the bundled defaults.
- You manually set `CODER_CHAT_HOOK_URL` or `OPENFLOWS_HOOK_ADDR` in your `.env` while using the default Docker setup. These variables are for custom host deployments only; the standard Docker setup automatically configures the internal networking. Remove them from `.env` and recreate the workspace.
- Coder and the consumer were started with different `CODER_CHAT_HOOK_SECRET` values or one was restarted out of order. Restart both with the same secret.

### `403 External authentication is required to create a workspace with this template`

Coder refuses to build a workspace until the owning account links the GitHub App (workspaces that request GitHub access require the owner to authenticate with it). Fix it by completing the link as the **tenant user** (see [Link the GitHub App](#link-the-github-app-required-for-private-repos)) — sign in as the workspace owner and visit `http://localhost:7080/external-auth/primary-github`, then **Authorize** on GitHub. Afterwards, recreate the tenant workspace via `./scripts/prod.sh tenant <owner/repo> --name <tenant>`.

### Agents can't clone/push the private repo

Even with the provider configured, git access only works when all three are true:

1. The workspace owner has **linked** the GitHub App (see [Step 4](#step-4--sign-in-with-github)).
2. The GitHub App is **installed** on the account/org that owns the repo, with access to it (`https://github.com/apps/<your-app-slug>/installations/new`).
3. The App is set to **public** (App → Advanced → "Make this GitHub App public") so other accounts can link it.

Verify inside a workspace **without printing the token**: `test -s ~/.git-credentials && echo 'git credentials configured'` (or run `git ls-remote <your-repo-url>` to confirm auth works). **Never** `cat ~/.git-credentials` — it prints your live GitHub token to the terminal/session logs.

### "No published image for OpenFlows X" (`--compose`)

That version has no image on GitHub Packages. Pick one listed at [ghcr.io/the-agenticflow/openflows](https://github.com/The-AgenticFlow/openflows/pkgs/container/openflows), set it as `OPENFLOWS_VERSION`, and re-run setup.

### "OPENFLOWS_IMAGE_DIGEST is for version X, not Y" (`--compose`)

You changed `OPENFLOWS_VERSION` but kept the old digest. Clear `OPENFLOWS_IMAGE_DIGEST` in `.env` and re-run setup; it pins the digest of the new version.

### "Image … was not found" (`--compose`)

The digest in `OPENFLOWS_IMAGE_DIGEST` does not exist. Check it against the release notes, or clear it to use the version's current image.

### "Cannot write to .dev-binaries as user X" (`--compose`)

The image's programs are copied as `OPENFLOWS_HOST_UID:OPENFLOWS_HOST_GID` (1000:1000 when unset), so the files stay owned by you. Setup fills these in for you; when running `docker compose -f docker-compose.yml -f docker-compose.images.yml` by hand, set them to the output of `id -u` / `id -g` and create `.dev-binaries/` yourself first (if Docker creates it, it is owned by root).

### "The shared OpenFlows GitHub App (connected-local) does not work with --compose yet"

`.env` has `OPENFLOWS_GITHUB_AUTH_MODE=connected-local`, which needs Rust today. Set it to `self-managed` to use your own App, or install with `--source`.

### "--compose currently needs a Linux host" / "an x86-64 (amd64) host" / "glibc 2.34 or newer"

The host cannot run the image's programs. Use `--source`. See [Host requirements](#host-requirements).

### "The OpenFlows programs from the image do not run on this machine" (`--compose`)

The copied `.dev-binaries/openflows` failed to start on the host, usually because the system's glibc is older than the image needs. Use `--source`. See [Host requirements](#host-requirements).

### Setup waits at "Connecting owner/repo" and never finishes

The tenant's nexus workspace is stopped, and setup waits for it to come up. Start the stack and workspaces, or restart the workspace: `coder restart openflows-nexus-<tenant> --yes`.

---

## More

- **Full docs:** [README.md](../README.md)
- **Testing & debugging:** [testing_quick_start.md](../testing_quick_start.md)
- **Token acquisition:** [token_guide.md](../token_guide.md)
- **Lifecycle hooks (design):** [docs/experiments/hook-driven-state-derivation.md](experiments/hook-driven-state-derivation.md) and [docs/experiments/coder-lifecycle-hooks-feedback.md](experiments/coder-lifecycle-hooks-feedback.md)
