# Local distribution modes

Status: contract for epic #481 (#482). The source and compose modes below are
implemented; the binary mode and the shared GitHub App with compose are planned.

## Goal

A first-time user can run OpenFlows locally without compiling Rust, while
contributors keep the source workflow. Every mode installs one OpenFlows
release, uses the same onboarding (`scripts/setup.sh`) and the same health
check (`openflows-doctor`), and stores its configuration in `.env`.

## Install modes

| Mode | Flag | Programs come from | Host |
| --- | --- | --- | --- |
| Source | `--source` (default) | `cargo build` of this checkout | Linux, macOS |
| Compose | `--compose` | `ghcr.io/the-agenticflow/openflows:<version>` (`linux/amd64`), copied into `.dev-binaries/` by `docker-compose.images.yml` | x86-64 Linux, glibc 2.34+ |
| Binary | `--binary` (planned) | The release's binary archives | Linux, macOS |

The workspace templates are built into the `openflows` program
(`crates/coder-client/build.rs` packs them; bootstrap embeds them), so in every
mode the templates pushed to Coder come from the same build as the programs.
Workspaces read the programs from `.dev-binaries/`, so the workspace startup is
identical across modes. Setup saves the
mode as `OPENFLOWS_SETUP_MODE` and reuses it when run without a flag; the first
run without a flag uses `--source` and prints a hint about `--compose`.

### Why compose needs an x86-64 Linux host

OpenFlows programs run in two places, and each needs a build for its own
operating system:

| Where | Runs | Source install provides | Compose install provides |
| --- | --- | --- | --- |
| Workspaces (Docker containers, always Linux) | `openflows`, `openflows-harness` | Linux build (`dev-sync.sh` cross-compiles in Docker) | Linux build from the image |
| The host, during setup | `openflows bootstrap`, `tenant add`, `doctor` | Native build (`cargo build` for the host) | Nothing native: the image's x86-64 glibc programs |

Workspaces always run `linux/amd64`, so the copy is pinned to that platform. On
an x86-64 Linux host with glibc 2.34+ the same programs also run on the host. On
macOS, ARM64 hosts or older glibc they cannot, so setup stops before starting
anything and suggests `--source`; it also test-runs the copied program before
bootstrap. The open items below list two ways to lift this.

## GitHub App modes

`OPENFLOWS_GITHUB_AUTH_MODE` selects the mode (#466).

| Mode | Value | Who holds the App secrets | Source install | Compose install |
| --- | --- | --- | --- | --- |
| Shared | `connected-local` | The OpenFlows team's App; no App secret on the user's machine | Default | Not yet |
| Custom | `self-managed` | The user: their own App, credentials only in their `.env` | Explicit choice | Default |

The shared mode currently runs the local Manager from source (`cargo run` for
the GitHub login, `docker compose --build` for its container), which needs Rust.
Compose installs therefore default to the custom App, and setup rejects
`connected-local` together with `--compose` with a clear message.

## Versions

- Users can install any published release. `OPENFLOWS_VERSION` selects it; when
  empty, setup resolves the latest release and saves the exact number.
- One version applies to every part: programs, images and templates.
- Compose pins the image by digest (`OPENFLOWS_IMAGE_DIGEST`). Tags can be
  rebuilt and moved; a digest always names the same image. Each release's notes
  record the digests of its images under **Docker images**.
- Upgrade and rollback: change `OPENFLOWS_VERSION`, clear the digest and re-run
  setup. When setup replaced the programs while workspaces were running, it
  offers to restart them (default no, since a restart pauses work in progress).

## Valid and invalid configurations

| Configuration | Result |
| --- | --- |
| `--source` and `--compose` together | Rejected before anything starts |
| `--compose` on a non-Linux, non-x86-64 or pre-2.34 glibc host | Rejected; suggests `--source` |
| `--compose` with `OPENFLOWS_GITHUB_AUTH_MODE=connected-local` | Rejected; suggests `self-managed` or `--source` |
| `OPENFLOWS_VERSION` not in `X.Y.Z` form | Rejected |
| Version with no published image | Rejected, with a link to the available versions |
| `OPENFLOWS_IMAGE_DIGEST` that does not exist | Rejected |
| `OPENFLOWS_IMAGE_DIGEST` from another version | Rejected (checked against the image's version label) |
| Second install on the same machine without its own `COMPOSE_PROJECT_NAME` | Takes over the first install's containers; must be avoided (documented) |

## Open items

- **Compose on macOS and ARM64:** run setup's `openflows` commands inside the image
  (`docker run`), which needs the container to reach Coder and the project
  folder; or use macOS release binaries once #484 publishes them.
- **Shared App with compose:** run the Manager's GitHub login from the published
  `openflows-manager` image and use that image instead of `--build`, so
  `connected-local` needs no Rust.
- **Release binaries (#484):** releases are immutable, so binaries must be
  attached to a draft before publishing. `release-assets.yml` has not run since
  v1.2.0, because tags created by the release workflow's `GITHUB_TOKEN` do not
  trigger other workflows (the Docker workflows are dispatched explicitly).

See also: [setup reference](../setup-reference.md#install-from-published-images---compose),
[shared GitHub App for local runs](shared-github-app-local-runs.md).
