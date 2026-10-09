# Scripts

## Production Commands

All production operations use `./scripts/prod.sh`:

```bash
./scripts/prod.sh bootstrap                      # One-time: Setup Coder + push templates
./scripts/prod.sh tenant owner/repo --name team --fleet N  # Add a tenant (runs its own in-workspace controller)
```

### `bootstrap` — One-time Setup

```bash
./scripts/prod.sh bootstrap
```

- Creates admin user in Coder (if not exists)
- Pushes Coder templates (nexus, forge, etc.)
- Verifies LLM and GitHub external auth are configured

### `tenant` — Add a Team

```bash
./scripts/prod.sh tenant owner/repo --name my-team --fleet N
```

Binds a GitHub repo to a tenant. Each tenant is scoped to one `owner/repo` and gets its own nexus workspace; that workspace's controller auto-starts with `GITHUB_REPOSITORY`/`OPENFLOWS_TENANT` injected from this command. `--fleet N` is **mandatory** and sets N FORGE-SENTINEL pairs.

### `doctor` — Health Check

```bash
./scripts/prod.sh doctor
```

---

## Development Helpers

### `dev-sync.sh` — Build and Mount Dev Binary

Builds the OpenFlows controller and makes it available to Coder workspaces:

```bash
./scripts/dev-sync.sh
```

This:
1. Incrementally builds `openflows` and `openflows-harness` for Linux amd64.
2. Verifies both artifacts are x86-64 ELF binaries and copies them to `.dev-binaries/`.
3. Hot-deploys the controller into a running Nexus workspace, if present.

**Note:** `./scripts/prod.sh bootstrap` runs this automatically. Use manually if you rebuild the binary and want to hot-deploy into a running workspace.

### macOS hosts (Intel and Apple Silicon)

Run the same `./scripts/prod.sh bootstrap` command on macOS. Install Rust via
rustup and the Xcode command-line tools (`xcode-select --install`), and start
Docker Desktop with Linux containers enabled. Allow Docker access to the repo's
filesystem location. Apple Silicon hosts need amd64 container emulation enabled
for the current Coder workspace templates.

Two binary formats are intentional:

| Consumer | Artifact | Format |
|---|---|---|
| Host bootstrap/tenant/doctor CLI | `target/release/openflows` | Native macOS Mach-O (native ELF on Linux) |
| Coder workspace controller and harness | `target/x86_64-unknown-linux-musl/release/` → `.dev-binaries/` | Linux amd64 ELF |

Do not run the `.dev-binaries/` executables on macOS or copy the native Mac CLI
into a Linux workspace. The workspace templates explicitly select `linux/amd64`
to match their Coder agents, including on Apple Silicon hosts.

`dev-sync.sh` selects an installed Zig toolchain, a musl GCC toolchain, or Docker.
The Docker fallback uses `messense/rust-musl-cross:x86_64-musl` and runs
`cargo build` inside it. The image tag is **not** the Rust target triple
`x86_64-unknown-linux-musl`; confusing them causes Docker's “not found” error.
The builder image supports Intel and ARM hosts; its compiler still targets amd64.
Docker builds do not require a Linux Rust target on the Mac; the image supplies it.
The `.docker-cargo/` cache is separate from the host Cargo cache and ignored by Git.

The first cross-build downloads an image and dependencies and can take time.
Subsequent builds reuse their caches. A native build still needs Rust installed
on the host. The helpers resolve their output paths relative to the repository,
so they can also be invoked from another working directory.

Check script behavior without provisioning infrastructure:

```sh
python3 -m unittest discover -s tests/scripts -v
bash -n scripts/dev-sync.sh scripts/prod.sh
```

### `reset-controller-state.sh` — Clean Redis

Reset Redis to a clean state:

```bash
./scripts/reset-controller-state.sh --confirm
```

Removes all tickets, workers, and orchestration state.

### `install.sh` — CLI Installer

Installs the `openflows` CLI binary:

```bash
curl -fsSL https://get.openflows.dev | bash
```

---

## Production Architecture

The controller runs inside a **Nexus workspace** provisioned by Coder. The workspace auto-starts the controller via startup_script.

```
openflows bootstrap → Coder pushes templates
openflows tenant add → Coder creates nexus workspace
    ↓
Workspace starts (docker container)
    ↓
Startup script runs:
  → Installs openflows binary
  → Sets up orchestration volume
  → Starts heartbeat daemon
  → Executes: openflows run
    ↓
Controller starts inside workspace
```

---

## Quick Reference

| Command | Description |
|---------|-------------|
| `./scripts/dev-sync.sh` | Build and mount dev binary to Coder |
| `./scripts/prod.sh bootstrap` | One-time: Setup Coder + templates (includes dev-sync) |
| `./scripts/prod.sh tenant owner/repo --name team --fleet N` | Add tenant (auto-starts its in-workspace controller) |
| `./scripts/prod.sh doctor` | Health check |
| `./scripts/reset-controller-state.sh --confirm` | Clean Redis state |