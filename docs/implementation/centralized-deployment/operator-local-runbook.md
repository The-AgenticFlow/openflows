# Local operator runbook

The shared Docker/Kubernetes workspace contract is in [Portable workspace runtime](../../architecture/portable-workspace-runtime.md). This runbook describes the local installation.

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
Manager route (`http://manager:3002/...` when Manager runs in Compose, as in
the example; `host.docker.internal` when it runs on the host); Coder uses it for authenticated Chat
lifecycle hooks and their JWT audience. Keep the supplied
`OPENFLOWS_RUNTIME_RELAY_BASE_URL=http://127.0.0.1:7081/tenants/{tenant}`.
Keep `OPENFLOWS_RUNTIME_MANAGER_URL=http://manager:3002` when Manager runs in
Compose, or `http://host.docker.internal:3002` when it runs on the Docker host. Workspaces use this private route for runtime
registration, heartbeats, and credential requests; GitHub and browsers still
use `OPENFLOWS_PUBLIC_URL`. Check that the private URL responds from a Coder
workspace before starting tenants.
Compose starts a local relay that forwards signed hooks to each tenant's
Nexus container on port 3001. Port 7081 is bound to loopback, not the public
tunnel. The Compose `manager` service already uses
`http://runtime-relay:8080/tenants/{tenant}`; a Manager run on the host keeps
the loopback value above.

Because Coder and tenant workspaces reach the host Manager over the Docker
network, the Manager must bind to a container-reachable address, not only
loopback. Set `OPENFLOWS_MANAGER_ADDR` to the host's private LAN/Docker-bridge
address (or `0.0.0.0:3002` behind a host firewall), not `127.0.0.1:3002`, so
`host.docker.internal:3002` resolves from inside containers. On Linux Docker
(which has no built-in `host.docker.internal` alias), add the matching
`extra_hosts: ["host.docker.internal:host-gateway"]` mapping to every service
and workspace container that must reach the Manager — the Coder service in
`deployment.compose.yml` includes it, and the workspace templates add the same
mapping (`host { host = "host.docker.internal", ip = "host-gateway" }`) to
their `docker_container` resources. Without this mapping, runtime registration
and hook delivery fail on Linux. See [Linux hosts](#linux-hosts) below.

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

On Linux, workspaces cannot read their runtime credential by default. The
Manager writes each secret file as mode 0600 owned by your host user, the
file is bind-mounted into the workspace, and the workspace user (uid 1000) is
a different user, so the agent harness fails with "Permission denied" on
`/run/openflows/runtime.json`. Set `OPENFLOWS_SECRET_FILE_MODE=0644` for the
Manager to create secret files readable inside the workspace. The Manager
accepts a mode open to group or other only when `OPENFLOWS_SECRET_DIR` itself
is mode 0700 (`chmod 700`), so other host users still cannot reach the files.
Files created before the setting need `chmod 644` (or
`setfacl -m u:1000:r <file>`). macOS Docker Desktop maps ownership and does
not need this.

With the filesystem store the Manager mounts the store file itself. To keep
the store elsewhere, set `OPENFLOWS_RUNTIME_DELIVERY_DIR` to an absolute host
directory; the Manager then writes one bundle file per workspace there, with
the same `OPENFLOWS_SECRET_FILE_MODE` rule (mode 0700 directory when widened).

## 3. Start

If you will create or restart local Coder workspaces, first build the Linux
controller and harness binaries that the workspace templates mount:

```sh
make dev-sync
```

Run it again after changing controller or harness code. It is not needed just
to start the Manager or check its API. Then start everything (Coder, databases,
Redis, relay, the Manager, and the Console) with one command:

```sh
./scripts/up.sh
```

It detects the user and Docker group ids the Manager container needs, creates
the provisioner credential directory, points `OPENFLOWS_HOOK_URL` and
`OPENFLOWS_RUNTIME_MANAGER_URL` at the Compose Manager (keeping a
`.env.openflows.bak`), starts the stack, waits until the Manager and Console
answer, and prints their URLs. Add `--pull` to fetch a newer Console image. It is
safe to re-run, and it stops without changing anything if the install was saved
for a host Manager (see below). To add only the Console to a Manager that is already
running, however it was started, use `./scripts/console.sh`. The equivalent plain command is
`docker compose --env-file .env.openflows -f deployment.compose.yml up -d`.

The Manager image is built from this checkout the first time (a few minutes).
The Console is pulled from GHCR and served on `http://127.0.0.1:3000`; it
reaches Manager at `http://manager:3002` over the Compose network. On a new
install, run the one-time setup and checks through the same image, then restart
the Manager:

```sh
docker compose --env-file .env.openflows -f deployment.compose.yml run --rm manager setup
docker compose --env-file .env.openflows -f deployment.compose.yml run --rm manager doctor
docker compose --env-file .env.openflows -f deployment.compose.yml restart manager
```

Tenant provisioner credentials are bind-mounted into tenant containers by the
host Docker daemon, so Compose mounts `.local/provisioners` (or
`OPENFLOWS_PROVISIONER_SECRET_DIR`, an absolute path) at the same path in the
Manager container. Create it first (`mkdir -p .local/provisioners`) so it is
owned by your user, not root.

An install is tied to one Manager mode: setup saves `CODER_URL` in
`deployment.json`, and `doctor` stops with "deployment URLs differ from saved
setup state" if it changes. An install first set up with the host Manager
(`CODER_URL=http://localhost:7080`) needs `coder_url` in the saved
`deployment.json` changed to `http://coder:7080` before the Compose Manager
will start, and back again to return to the host Manager.

If your user id or `docker` group id is not 1000/999, set `OPENFLOWS_MANAGER_UID`,
`OPENFLOWS_MANAGER_GID` and `DOCKER_GID` in `.env.openflows`.

To run Manager on the host instead (for example to iterate on its code), stop
the Compose one with `docker compose ... stop manager`, set
`OPENFLOWS_MANAGER_URL=http://host.docker.internal:3002` for the Console, and
use the host Manager values from the [Linux hosts](#linux-hosts) table:

```sh
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
immutable. Template variables (`TF_VAR_docker_network`,
`TF_VAR_dev_binary_host_path`, …) are captured when a release is published,
so changing them also needs a new release name, followed by a promotion of
that release to each existing organization. For AWS staging/production, use the
[production runbook](operator-production-runbook.md).

## Linux hosts

A Linux host running the Manager directly (not in Compose) next to a Docker
Coder needs these settings; each one was a failure seen in practice.

| Setting | Value | Why |
| --- | --- | --- |
| `OPENFLOWS_MANAGER_ADDR` | the `docker0` bridge address, e.g. `172.17.0.1:3002` | Containers cannot reach a Manager bound to `127.0.0.1`. The bridge address is not exposed to your network, unlike `0.0.0.0`. Point the Console's `OPENFLOWS_MANAGER_URL` at the same address. |
| `OPENFLOWS_RUNTIME_MANAGER_URL` | `http://172.17.0.1:3002` (or `http://host.docker.internal:3002` with templates that map it) | Workspaces register, heartbeat and request credentials here. Check from inside a container with `curl http://<address>/health`. |
| `OPENFLOWS_HOOK_URL` | `http://host.docker.internal:3002/api/v1/hooks/coder/chat` | Coder calls this; the Coder service has the host-gateway mapping. |
| `OPENFLOWS_RUNTIME_REDIS_URL` | `redis://redis:6379` | Workspaces cannot reach a Redis published on host loopback. Attach the Redis container to the workspace network with the alias `redis` (`docker network connect --alias redis <network> <redis-container>`). |
| `OPENFLOWS_PROVISIONER_NETWORK` / `OPENFLOWS_PROVISIONER_CODER_URL` | the Coder Compose network and `http://coder:7080` | Per-organization provisioner containers otherwise dial `host.docker.internal:7080`, which is refused when Coder publishes on `127.0.0.1` only. |
| `TF_VAR_docker_network` | the same Coder Compose network | Workspaces must join the network the relay and Redis are on. |
| `TF_VAR_dev_binary_host_path` | absolute path to `.dev-binaries` | Run `make dev-sync` first; workspaces mount these binaries. |
| `OPENFLOWS_RUNTIME_RELAY_BASE_URL` | `http://127.0.0.1:7081/tenants/{tenant}` | Run the relay (`runtime-relay` in `docker-compose.yml`) on the workspace network, published on `127.0.0.1:7081`. |

Coder needs, in addition to its licence (multi-organization and external
provisioners are Premium features):

- `CODER_EXPERIMENTS=agent-lifecycle-hooks,oauth2` — the OAuth2 provider
  lets each user link their Coder account to OpenFlows.
- `CODER_CHAT_HOOK_ENABLED=true`, `CODER_CHAT_HOOK_URL` (the hook URL above)
  and `CODER_CHAT_HOOK_SECRET` (the same value as the Manager's
  `coder-hook-secret`). Coder refuses a plain-HTTP hook URL unless
  `CODER_CHAT_HOOK_ALLOW_INSECURE=true`; allow that on a local host only and
  use HTTPS everywhere else.
- GitHub sign-in (`CODER_OAUTH2_GITHUB_CLIENT_ID` / `_CLIENT_SECRET`,
  `_ALLOW_SIGNUPS`). Account linking accepts only GitHub-backed Coder
  accounts. Locally you may reuse the OpenFlows GitHub App: add
  `<coder-url>/api/v2/users/oauth2/github/callback` as an extra callback URL
  and grant the App the *Email addresses: read-only* account permission
  (Coder reads `/user/emails`). Use a separate client in production.
- An OAuth2 provider app for the Manager, with callback
  `<openflows-public-url>/auth/coder/callback`; put its client id in
  `OPENFLOWS_CODER_OAUTH_CLIENT_ID` and its secret in a secret file referenced
  by `OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF`.

A runtime that was offline for longer than its 24-hour refresh window (for
example a laptop that slept) can no longer re-authenticate; its tenant status
reports `runtime.health = "restart_required"`. Stop and start the tenant to
issue a fresh bootstrap, or use `repair-runtime` above.
