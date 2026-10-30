# OpenFlows — Quick Start

Turn GitHub issues into reviewed pull requests, on your own machine. For what OpenFlows is, see the [README](README.md).

## You need

- Docker (running), `curl`, `python3`
- Rust (`cargo`), only for the source install below
- A GitHub account with access to the repository and permission to install the OpenFlows App there (or approval from its owner). With `--compose`, an account that can create a GitHub App on the repo's owner instead
- An API key for an LLM provider (Anthropic, OpenAI, OpenRouter or Google)

## Choose how to install

| | `--compose` | `--source` (default) |
| --- | --- | --- |
| How | Uses the published OpenFlows images | Compiles OpenFlows from this checkout |
| Needs Rust | No | Yes |
| First run | A few minutes | About 10 minutes |
| Host | x86-64 Linux, glibc 2.34+ (e.g. Ubuntu 22.04+) | Linux or macOS |
| Version | Any release, latest by default | This checkout |
| GitHub App | Your own App | The team's shared App (or your own) |

`--compose` currently needs an x86-64 Linux host with glibc 2.34 or newer; elsewhere use `--source` ([why](docs/setup-reference.md#host-requirements)).

## Run

```bash
./scripts/setup.sh owner/repo --compose     # published images, no compiling
./scripts/setup.sh owner/repo --source      # compile from this checkout
```

Without a flag, setup uses the mode of your last run, or `--source` the first time. That is the whole setup. The script is safe to re-run: it checks what is already done and continues from there. If something is missing, it stops and tells you exactly what to do.

It will:

1. Check your machine (and install the `coder` CLI if missing).
2. Create `.env` and generate secrets.
3. With `--compose`: pick the OpenFlows version and pin its image digest in `.env`.
4. Set up GitHub access:
   - `--source`: install the team-owned **OpenFlows GitHub App** on the selected repository, then authorize your GitHub account using its device code. No personal App or App secret is needed. Set the App's public client ID and slug in `.env` when prompted.
   - `--compose`: create **your own GitHub App** — your browser opens, you click **Create GitHub App**, then install it on your repo. (The shared App still needs Rust, so it does not work with `--compose` yet.)
5. Start Redis and Coder in Docker (plus the local Manager when using the shared App).
6. Sign you in to Coder with **your GitHub account** (you become the owner) and save an API token you create in the dashboard (one paste).
7. Add your **LLM** provider and model (asks for the API key).
8. Push the workspace templates. With `--source` it builds the binaries first (about 10 minutes the first time); with `--compose` it uses the programs from the image.
9. Connect your repo. With the shared App, the local Manager renews your GitHub App user token when needed and supplies it to the workspaces; with your own App, you click **Authorize** once to link GitHub to your Coder account.

To use your own GitHub App with `--source` too, set `OPENFLOWS_GITHUB_AUTH_MODE=self-managed` in `.env` before setup. This keeps the existing Coder external-auth flow; see [manual setup](docs/setup-reference.md).

## Version, upgrade, rollback (`--compose`)

Set the version in `.env` (empty means the latest release), clear the pinned digest, and re-run setup:

```bash
OPENFLOWS_VERSION=1.8.1
OPENFLOWS_IMAGE_DIGEST=
```

Your stack and workspaces must be running when you re-run setup. At the end, setup offers to restart the running workspaces so they load the new programs (or later: `coder restart <workspace> --yes`). Details: [upgrade and rollback](docs/setup-reference.md#upgrade-and-rollback).

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

Re-run `./scripts/setup.sh owner/repo` after fixing the problem it printed. Messages specific to `--compose` are explained in [troubleshooting](docs/setup-reference.md#troubleshooting). Your Coder token lasts 90 days; re-running the script refreshes it. For manual steps, hooks, roles, configuration and troubleshooting, see [docs/setup-reference.md](docs/setup-reference.md).
