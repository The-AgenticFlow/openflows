# Local operator runbook

Run OpenFlows on one host for evaluation. Docker Compose starts Coder,
PostgreSQL, and Redis; the Manager runs from this checkout. This is not a
production multi-tenant installation.

## Before you start

- Install Docker Compose and Rust.
- Register the OpenFlows GitHub App and two GitHub clients: one for OpenFlows
  sign-in, one for Coder sign-in.
- Provide HTTPS URLs for OpenFlows and Coder. GitHub must reach the OpenFlows
  callbacks/webhook; workspaces must reach Coder. Compose does not provide TLS
  or a public tunnel.

| GitHub App setting | URL on your OpenFlows host |
| --- | --- |
| Sign-in callback | `/auth/github/callback` |
| App authorization callback | `/api/v1/github/oauth/callback` |
| Installation setup URL | `/api/v1/github/setup` |
| Webhook URL | `/api/v1/webhooks/github` |

Use Coder's callback URL for its separate GitHub client.

## 1. Configure

From the repository root:

```sh
cp .env.openflows.example .env.openflows
chmod 600 .env.openflows
```

Edit `.env.openflows`: replace the example URLs, database passwords, GitHub
IDs/slug, and workspace image digest. Keep local mode, the filesystem secret
provider, and the Docker provisioner. Keep `OPENFLOWS_HOOK_URL` on the private
`host.docker.internal` Manager route; Coder uses it for authenticated Chat
lifecycle hooks and their JWT audience. Keep the supplied
`OPENFLOWS_RUNTIME_RELAY_BASE_URL=http://127.0.0.1:7081/tenants/{tenant}`.
Keep `OPENFLOWS_RUNTIME_MANAGER_URL=http://host.docker.internal:3002` when
Manager runs on the Docker host. Workspaces use this private route for runtime
registration, heartbeats, and credential requests; GitHub and browsers still
use `OPENFLOWS_PUBLIC_URL`. Check that the private URL responds from a Coder
workspace before starting tenants.
Compose starts a local relay that forwards signed hooks to each tenant's
Nexus container on port 3001. Port 7081 is bound to loopback, not the public
tunnel. The Manager runs on the host in this runbook; a containerized Manager
would use `http://runtime-relay:8080/tenants/{tenant}` instead.

Use `OPENFLOWS_CODER_BOOTSTRAP_EMAIL` only for a **new, empty Coder**. For an
existing Coder, clear it and supply an administrator credential through
`OPENFLOWS_CODER_BOOTSTRAP_TOKEN_FILE` for setup.

## 2. Add secrets

```sh
mkdir -p .openflows/secrets
chmod 700 .openflows/secrets
printf '%s' "$(openssl rand -hex 32)" > .openflows/secrets/coder-hook-secret
chmod 600 .openflows/secrets/coder-hook-secret
```

Create four more mode-0600 files in `.openflows/secrets`:

| File | Put in it |
| --- | --- |
| `github-client-secret` | OpenFlows GitHub client secret |
| `github-app-private-key` | GitHub App PEM private key |
| `github-webhook-secret` | App webhook secret |
| `coder-github-client-secret` | Coder GitHub client secret |

Setup generates the remaining Manager secrets. Do not commit or discard this
directory; keep it with the control-plane database when backing up. If you
run Compose outside the checkout, set an absolute `OPENFLOWS_SECRET_DIR`.
The hook key must be printable ASCII because Compose passes it to Coder as an
environment variable. Coder and Manager must read the same file; setup rejects
binary keys rather than silently accepting a value Coder cannot use intact.

## 3. Start

If you will create or restart local Coder workspaces, first build the Linux
controller and harness binaries that the workspace templates mount:

```sh
make dev-sync
```

Run it again after changing controller or harness code. It is not needed just
to start the Manager or check its API. Then start the services:

```sh
docker compose --env-file .env.openflows -f deployment.compose.yml up -d
cargo run -p openflows-manager -- --config .env.openflows setup
cargo run -p openflows-manager -- --config .env.openflows doctor
cargo run -p openflows-manager -- --config .env.openflows serve
```

Wait for Coder to become ready before running `setup`; fix any `doctor`
failure before `serve`. Leave the Manager running. The last command applies
database migrations on startup.

Forge Chats are created by Manager with its operator credential and an explicit
tenant-owner ID. The tenant creator still links their Coder account to establish
that identity, but no extra Chat OAuth scope or personal Coder token is needed.

If a hosted workspace is restarted after its one-use runtime bootstrap was
consumed, recover its runtime registration before restarting it again:

```bash
cargo run -p openflows-manager -- --config .env.openflows repair-runtime <openflows-workspace-uuid>
```

This writes a new short-lived bootstrap only to the mounted workspace secret.
It never prints the credential. Use the OpenFlows workspace UUID, not the Coder
workspace UUID. This is an operator recovery step, not an end-user action.

## 4. Confirm it works

Check `http://127.0.0.1:3002/ready`, then open the configured public URL.
Sign in with GitHub, link a personal Coder account, create an organization,
install the GitHub App, and create a tenant from an App-accessible repository.
Follow the operation to success, start the tenant, and confirm its workspace
connects and its runtime heartbeats. If it fails, inspect the operation and
Coder build/agent logs.

For changed templates, use a new `OPENFLOWS_RELEASE_NAME`; release names are
immutable. For AWS staging/production, use the
[production runbook](operator-production-runbook.md).
