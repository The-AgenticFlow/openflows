# OpenFlows setup reference

The [quick start](../quick_start.md) covers the normal installation. Local
OpenFlows uses the shared OpenFlows GitHub App and a local Manager. The
contributor installs the App on the selected repository, authorizes their
GitHub account in the console at <http://localhost:3002>, and selects the
repository. No App private key, client secret, or Coder GitHub external-auth
provider is needed on the contributor's computer.

## Prerequisites

- Docker and Docker Compose, with enough resources for Coder and workspaces.
- A GitHub account that can access the repository. Its owner must approve the
  OpenFlows App installation for that repository.
- An LLM provider key.
- Rust and Cargo for `--source`. Release installs use published artifacts.

Setup checks for the `coder` CLI and guides installation if it is missing.

## Install

From the checkout or release setup kit, run:

```bash
./scripts/setup.sh owner/repo --binary   # release binaries
./scripts/setup.sh owner/repo --compose  # published images
./scripts/setup.sh owner/repo --source   # compile this checkout
```

`--source` is the default in a source checkout. A setup kit defaults to
`--binary`. Setup creates `.env`, starts Redis, PostgreSQL, Coder, and the
local Manager, connects the repository, configures an LLM model, and pushes
five workspace templates. It can be run again after an interrupted step.

The local console opens during setup:

1. Click **Install OpenFlows App** and grant it access to the repository.
2. Click **Connect GitHub**, approve the device code, and return to the console.
3. Select the repository and click **Use selected repository**.
4. Return to the terminal and continue setup.

The Manager stores the App user access and refresh tokens in a private file
under `~/.config/openflows/` and rotates them when needed. GitHub grants
operations only where the signed-in user and App installation both have
access. A workspace obtains credentials from the local Manager. Do not copy
local credential files into a repository or image.

## Configuration

Copy `.env.example` to `.env` only when configuring by hand; setup creates
`.env` for you. The main user inputs are:

| Variable | Purpose |
| --- | --- |
| `CODER_SESSION_TOKEN` | Your local Coder account token; setup helps create it. |
| `OPENFLOWS_VERSION` | Release version for `--binary` or `--compose`; an empty value selects the latest release. |
| `OPENFLOWS_LLM_PROVIDER`, `OPENFLOWS_LLM_API_KEY`, `OPENFLOWS_LLM_MODEL` | Noninteractive LLM configuration. |
| `COMPOSE_PROJECT_NAME` | Isolates a second installation on the same machine. |

The setup script sets the local Manager, credential path, Coder template, and
host UID/GID variables. It also registers a Coder OAuth application after
sign-in, saves its public client ID in `.env`, and stores its secret in the
local Manager secret directory. For source installs, the template release
name is derived from the template contents, so template edits produce a new
immutable release name on the next setup run. Keep `.env` private.
`docker-compose.connected-local.yml` exposes the console only on host loopback.

## Install without compiling (`--compose`, `--binary`)

Both release modes require a version with the shared App Manager image (newer
than 1.8.1). `--compose` pins the program and Manager images by digest and
copies workspace binaries into `.dev-binaries/`. `--binary` verifies release
archive checksums and uses the same Manager image. A source checkout can use
`--source` to build and run its Manager from current code.

For release installation, setup records the selected version and image
digests in `.env`; it does not substitute an older App mode when an image is
missing. Choose a release with a Manager image or use `--source`.

### Upgrade and rollback

Set `OPENFLOWS_VERSION` in `.env`, clear an old image digest, and rerun setup.
Setup updates stopped workspaces and offers to update running ones. Use
`--update-workspaces` for a noninteractive update. A workspace keeps its
current template version until updated; a plain restart does not upgrade the
template. To roll back, select the previous compatible release and rerun.

## Verify it's working

```bash
curl -fsS http://localhost:3002/health
./scripts/prod.sh doctor
```

The console should list the selected repository, tenant, workspaces, and
operations. A Forge startup should register with the Manager, obtain a GitHub
credential, and send a readiness heartbeat. Check Coder workspace startup
logs when a worker stays in `start_error`.

## Troubleshooting

### Repository does not appear

Check that the App is installed on that repository and your GitHub account
can access it. Return to the local console and refresh GitHub access. If the
user authorization expired or was revoked, use **Connect GitHub** again.
If GitHub's installation return lands on the hosted Manager with an
authentication error, the App installation may still have completed. Return
to the local console and refresh. The installation return is not proof of
repository access; the local Manager checks GitHub using the signed-in user's
App token. To change accounts on one computer, select **Switch GitHub account**
and approve the device code while signed in to the intended GitHub account.

### GitHub credential request fails during worker startup

For local workspaces, check the Manager logs for the `/api/v1/local/github-credential` response.
A GitHub availability failure is retryable; a revoked authorization requires
connecting GitHub again. Restart a failed worker through OpenFlows or Coder's
workspace build path. Its bootstrap token is single use, so restarting only
the Docker container cannot register it again.

### Coder templates push fails

Install the `coder` CLI version offered by the local Coder server and rerun
setup. Template names are scoped to the default Coder organization; the push
command selects that organization explicitly.

### Port already in use

Find the process occupying the reported Redis, Coder, or Manager port. Stop
only the conflicting service or adjust your Compose configuration.

### No LLM models configured

Add a provider and model at <http://localhost:7080/ai/settings/providers>,
then rerun setup.
