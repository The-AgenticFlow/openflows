# Local distribution modes

Status: contract for epic #481 (#482), implemented for all three modes.

## Goal

A first-time user can run OpenFlows locally without compiling Rust, while
contributors keep the source workflow. Every mode installs one OpenFlows
release, uses the same onboarding (`scripts/setup.sh`) and the same health
check (`openflows-doctor`), and stores its configuration in `.env`.

## Install modes

| Mode | Flag | Programs come from | Host |
| --- | --- | --- | --- |
| Source | `--source` (default) | `cargo build` of this checkout | Linux, macOS |
| Compose | `--compose` | `ghcr.io/the-agenticflow/openflows:<version>` (`linux/amd64`), copied into `.dev-binaries/` by `docker-compose.images.yml` | Linux, macOS; x86-64, ARM64 |
| Binary | `--binary` (default in a setup kit) | The release's `openflows-<version>-<target>.tar.gz`, checksum verified | Linux, macOS; x86-64, ARM64 |

The workspace templates are built into the `openflows` program
(`crates/coder-client/build.rs` packs them; bootstrap embeds them), so in every
mode the templates pushed to Coder come from the same build as the programs.
Workspaces read the programs from `.dev-binaries/`, so the workspace startup is
identical across modes. Setup saves the
mode as `OPENFLOWS_SETUP_MODE` and reuses it when run without a flag; the first
run without a flag uses `--source` and prints a hint about `--compose`.

### Programs for the workspaces and for the host

OpenFlows programs run in two places, and each needs a build for its own
operating system:

| Where | Runs | Source | Compose | Binary |
| --- | --- | --- | --- | --- |
| Workspaces (`linux/amd64` containers) | `openflows`, `openflows-harness` | `dev-sync.sh` cross-compiles | From the image | `x86_64-unknown-linux-gnu` archive |
| The host, during setup | `openflows bootstrap`, `tenant add`, `doctor` | Native `cargo build` | The image's copy on x86-64 Linux with glibc 2.34+; otherwise the release's native archive | The same archive on x86-64 Linux with glibc; otherwise the release's native archive |

Native copies live in `.dev-binaries/host/`. Docker emulates `linux/amd64` on ARM64.

## GitHub App modes

`OPENFLOWS_GITHUB_AUTH_MODE` selects the mode (#466). The epic's `shared` and
`custom` are named `connected-local` and `self-managed`.

| Mode | Value | Who holds the App secrets | GitHub handled by | Manager comes from |
| --- | --- | --- | --- | --- |
| Shared | `connected-local` (default) | The OpenFlows team's App; no App secret on the user's machine | Local Manager | Source: compiled. Compose, binary: the published `openflows-manager` image |
| Custom | `self-managed` | The user: their own App, credentials only in their `.env` | Coder external auth | Not used |

The Manager is a separate program, not packaged in the `openflows` archives. Its
image has the same version as the release and is pinned by digest. The first
usable version is the one after 1.8.1; setup rejects `connected-local` on older
versions without compiling. When the mode is unset, setup picks `self-managed` if
App credentials are present or the version has no shared App.

## Versions

- Users can install any published release. `OPENFLOWS_VERSION` selects it; when
  empty, setup resolves the latest release and saves the exact number.
- One version applies to every part: programs, images and templates.
- Binary installs verify each archive against its `.sha256`. Release files exist
  from the version after 1.8.1, since published releases are immutable.
- Compose pins the image by digest (`OPENFLOWS_IMAGE_DIGEST`). Tags can be
  rebuilt and moved; a digest always names the same image. Each release's notes
  record the digests of its images under **Docker images**.
- Upgrade and rollback: change `OPENFLOWS_VERSION`, clear any digest and re-run
  setup. Workspaces keep their template version until updated. Setup moves
  stopped workspaces to the new templates right away (they stay stopped) and
  offers `coder update` for running workspaces with new templates and a restart
  for the others (default no, since it pauses work in progress;
  `--update-workspaces` skips the question). Setup waits for each build. Until
  every workspace is updated, setup reports the upgrade as not complete, exits
  with status 3 and offers the rest again on each run.

## Valid and invalid configurations

| Configuration | Result |
| --- | --- |
| More than one of `--source`, `--compose`, `--binary` | Rejected before anything starts |
| `--binary` or `--compose` on an unsupported OS or CPU | Rejected; suggests `--source` |
| `--source` in a setup kit | Rejected; suggests `--binary` |
| `--binary` or `--compose` with `connected-local` on 1.8.1 or older | Rejected; suggests a newer version or `self-managed` |
| Native programs needed, and the release has no files | Rejected; suggests a newer version or `--source` |
| Archive that fails checksum verification | Rejected; nothing installed |
| `OPENFLOWS_VERSION` not in `X.Y.Z` form | Rejected |
| Version with no published image | Rejected, with a link to the available versions |
| `OPENFLOWS_IMAGE_DIGEST` that does not exist | Rejected |
| `OPENFLOWS_IMAGE_DIGEST` from another version | Rejected (checked against the image's version label) |
| Second install on the same machine without its own `COMPOSE_PROJECT_NAME` | Takes over the first install's containers; must be avoided (documented) |

See also: [release checklist and rollback](../operations/release-checklist.md),
[setup reference](../setup-reference.md#install-without-compiling---compose---binary),
[shared GitHub App for local runs](shared-github-app-local-runs.md).
