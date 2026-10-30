# OpenFlows — Quick Start

Turn GitHub issues into reviewed pull requests, on your own machine. For what OpenFlows is, see the [README](README.md).

## You need

- Docker (running), Rust (`cargo`), `curl`, `python3`
- A GitHub account with access to the repository and permission to install the OpenFlows App there (or approval from its owner)
- An API key for an LLM provider (Anthropic, OpenAI, OpenRouter or Google)

## Run

```bash
./scripts/setup.sh owner/repo
```

That is the whole setup. The script is safe to re-run: it checks what is already done and continues from there. If something is missing, it stops and tells you exactly what to do.

It will:

1. Check your machine (and install the `coder` CLI if missing).
2. Create `.env` and generate secrets.
3. Install the team-owned **OpenFlows GitHub App** on the selected repository, then authorize your GitHub account using its device code. No personal App or App secret is needed. Set the App's public client ID and slug in `.env` when prompted.
4. Start Redis, Coder, and the local Manager in Docker.
5. Sign you in to Coder with **your GitHub account** (you become the owner) and save an API token you create in the dashboard (one paste).
6. Add your **LLM** provider and model (asks for the API key).
7. Build binaries and push the workspace templates (first run takes about 10 minutes).
8. Connect your repo. The local Manager renews your GitHub App user token when needed and supplies it to the workspaces.

To use your own GitHub App instead, set `OPENFLOWS_GITHUB_AUTH_MODE=self-managed` in `.env` before setup. This keeps the existing Coder external-auth flow; see [manual setup](docs/setup-reference.md).

Maintainers validating the team-owned App can follow the [connected-local live test](docs/testing/connected-local-github-live.md) with a disposable repository.

## Testing the Manager's organization provisioning locally

The Manager needs its own Coder API token in the local development secrets
file. After the local Coder setup has created a Coder credential and
`.env.prod` contains `CODER_URL`,
`OPENFLOWS_CODER_OPERATOR_TOKEN_REF`, and
`OPENFLOWS_DEV_SECRETS_FILE`, run:

```bash
python3 scripts/bootstrap-manager-coder-token.py
cargo run -p openflows-manager
```

With the Docker backend, creating an organization now automatically creates
its Coder provisioner key and starts its provisioner container. Keep Docker
running; no per-organization Coder commands are required. Manager waits for
Coder to report the daemon online before importing templates. Provisioner
containers survive Manager restarts and are reconciled every 30 seconds.
See the [Manager deployment settings](crates/openflows-manager/README.md) for
container networking, persistent key files, and non-Docker backend limitations.

The helper uses `CODER_SESSION_TOKEN` from the process environment or `.env` as
a bootstrap credential, creates a separate Manager API token using the Coder
deployment's configured default lifetime, and stores it under the configured reference in the development secrets JSON file
with mode `0600`. It reuses a valid stored Manager token. If the bootstrap
credential has expired, refresh it through the existing local Coder setup and
run the helper again. The helper never prints token values.

The initial bootstrap credential still has to come from an authorized Coder
administrator or the one-time local Coder setup. For production, provision the
Manager token through the deployment's identity and secret-management process;
do not use this development JSON-file helper.

The script asks how many **FORGE-SENTINEL pairs** (the fleet) to run: one pair works one issue at a time, N pairs work N issues in parallel and use more LLM tokens. Start with `1`. To skip the question, pass it: `./scripts/setup.sh owner/repo --fleet 3`. Other option: `--name TEAM` (default: `owner-repo`).

## Use it

Open an issue in your repo. OpenFlows picks it up, writes code, reviews it and opens a pull request.

- Dashboard: <http://localhost:7080>
- Health check: `./scripts/prod.sh doctor`
- Add another repo: `./scripts/prod.sh tenant owner/repo --name team --fleet 1`

## Something went wrong?

Re-run `./scripts/setup.sh owner/repo` after fixing the problem it printed. Your Coder token lasts 90 days; re-running the script refreshes it. For manual steps, hooks, roles, configuration and troubleshooting, see [docs/setup-reference.md](docs/setup-reference.md).
