# OpenFlows — Quick Start

Turn GitHub issues into reviewed pull requests, on your own machine. For what OpenFlows is, see the [README](README.md).

## Choose the GitHub authorization mode

The default is `connected-local`: OpenFlows supplies the team-owned GitHub App,
and the local Manager guides you through installing it, approving your GitHub
account, and selecting a repository. You do not create an App or enter a client
ID, App slug, client secret, or private key.

If you want to use your own GitHub App, set this before running setup:

```bash
OPENFLOWS_GITHUB_AUTH_MODE=self-managed ./scripts/setup.sh owner/repo
```

That keeps the existing Coder external-auth flow. Follow the [self-managed App
setup instructions](docs/setup-reference.md) for its credentials and
installation.

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
3. Open the local OpenFlows console, install the team-owned **OpenFlows GitHub App** on a repository, authorize your GitHub account using its device code, then choose that repository. No personal App, App secret, client ID, or App slug is needed.
4. Start Redis, Coder, and the local Manager in Docker.
5. Sign you in to Coder with **your GitHub account** (you become the owner) and save an API token you create in the dashboard (one paste).
6. Add your **LLM** provider and model (asks for the API key).
7. Build binaries and push the workspace templates (first run takes about 10 minutes).
8. Connect your repo. The local Manager renews your GitHub App user token when needed and supplies it to the workspaces.

For connected-local mode, open the Manager console at <http://localhost:3002> after setup. The console handles App installation, GitHub approval, and repository selection.

Maintainers validating the team-owned App can follow the [connected-local live test](docs/testing/connected-local-github-live.md) with a disposable repository.

## Testing the Manager's organization provisioning locally

This is only needed when testing the Manager's organization provisioning
directly. Normal `./scripts/setup.sh owner/repo` setup does this work through
the local stack.

The Manager uses a separate Coder API token so it can provision organizations
without reusing your personal token. You choose the local secrets-file path
and the label under which that token is stored; the helper creates the token.
It never prints the token value.

1. Run `./scripts/setup.sh owner/repo` first. It starts Coder and stores your
   Coder session token in `.env`.
2. Create a private local secrets file. This file is ignored by Git:

   ```bash
   mkdir -p .openflows/local
   printf '{}\n' > .openflows/local/manager-secrets.json
   chmod 600 .openflows/local/manager-secrets.json
   ```

3. Copy the example and set these three values in `.env.prod`:

   ```bash
   cp .env.prod.example .env.prod
   ```

   ```dotenv
   # The local Coder instance started by setup.
   CODER_URL=http://127.0.0.1:7080

   # A name we choose for the Manager's token inside the JSON file.
   OPENFLOWS_CODER_OPERATOR_TOKEN_REF=coder-operator-token

   # An absolute path to the file created in step 2.
   OPENFLOWS_DEV_SECRETS_FILE=/absolute/path/to/openflows/.openflows/local/manager-secrets.json
   ```

   `CODER_URL` is the address of your local Coder instance. The token reference
   is only a JSON key, so keep the default value. Replace the last value with
   the real absolute path on your machine; it is not a token.

4. Create the Manager token and start the Manager:

   ```bash
   python3 scripts/bootstrap-manager-coder-token.py
   cargo run -p openflows-manager
   ```

If `.env` has no valid `CODER_SESSION_TOKEN`, sign in at
<http://localhost:7080>, create a token at **Settings → Tokens**, save it in
`.env`, and run the helper again. The helper mints a separate token for the
Manager and writes it to the secrets file with `0600` permissions.

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
