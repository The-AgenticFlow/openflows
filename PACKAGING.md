# Distribution & Packaging

This document describes how OpenFlows is packaged and distributed across platforms.

## Release Channels

OpenFlows ships through two release channels:

| Channel | Tag Pattern | Published When | Stability |
|---------|------------|----------------|-----------|
| **Stable** | `vX.Y.Z` | Version tag is pushed | Production-ready |
| **Edge** | `vX.Y.Z-dev.N.SHA` | Every push to `main` | Bleeding-edge, may be unstable |

### Stable Releases

Triggered by pushing a version tag (e.g., `v0.2.0`):

```bash
git tag v0.2.0
git push origin v0.2.0
```

This creates a full release with:
- Pre-built binaries for all platforms (as GitHub Release assets)
- Docker image tagged with the version + `latest`
- npm package published under the `latest` dist-tag
- Crates published to crates.io
- Homebrew formula updated with the new version

### Edge (Pre-release) Builds

Every push to the `main` branch automatically creates a pre-release build:

- GitHub Release marked as **prerelease** with a generated version like `v0.1.0-dev.42.abc1234`
- Docker image tagged with the dev version (the `latest` tag is also updated)
- npm package published under the `next` dist-tag

Edge builds let you test the latest changes without waiting for a stable release.

## Installation Methods

### 1. One-Line Installer (All Platforms)

```bash
# Stable release (default)
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash

# Edge (pre-release from main)
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash -s -- --edge

# Custom install directory
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash -s -- --dir /usr/local/bin
```

**What it does:**
- Detects OS and architecture automatically
- Checks for and installs prerequisites (Rust, Node.js, Git, Claude Code CLI)
- Downloads pre-built binaries from GitHub Releases (falls back to building from source)
- Installs to `~/.local/bin` (or `$AGENTFLOW_INSTALL_DIR`)
- Adds the install directory to PATH if needed
- Offers to run the setup wizard

**Environment variables:**
| Variable | Default | Description |
|----------|---------|-------------|
| `AGENTFLOW_INSTALL_DIR` | `~/.local/bin` | Installation directory |
| `AGENTFLOW_CHANNEL` | `stable` | Release channel: `stable` or `edge` |

**CLI flags:**
| Flag | Description |
|------|-------------|
| `--edge` | Install the latest pre-release build from main |
| `--stable` | Install the latest stable release (default) |
| `--dir DIR` | Set installation directory |

### 2. npm (Node.js Package Manager)

```bash
# Stable release
npm install -g @the-agenticflow/openflows

# Edge (pre-release)
npm install -g @the-agenticflow/openflows@next
```

The npm package downloads the correct pre-built binary for your platform during installation (with a fallback to building from source if Rust is available). Supports Linux and macOS on x64 and arm64.

### 3. Homebrew (macOS)

```bash
brew tap The-AgenticFlow/openflows
brew install openflows
```

The Homebrew formula is maintained at `packaging/homebrew/openflows.rb` and is automatically updated on stable releases.

### 4. Docker

```bash
# Pull and run
docker pull ghcr.io/the-agenticflow/openflows:latest
docker run -it --rm \
  -v "$HOME/.agentflow:/home/openflows/.agentflow" \
  -v "$(pwd):/workspace" \
  -e ANTHROPIC_API_KEY=your_key \
  -e GITHUB_PERSONAL_ACCESS_TOKEN=your_token \
  ghcr.io/the-agenticflow/openflows:latest setup

# Or use Docker Compose (includes LiteLLM proxy + Redis)
docker compose up -d
```

The Docker image is multi-stage, uses a non-root user, and includes a health check.

### 5. Cargo (Rust Package Manager)

```bash
cargo install openflows
```

All crates are published to crates.io. The `openflows` package includes all binaries. Crates are published only on stable releases (tag pushes).

### 6. Build from Source

```bash
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
make release    # Builds all binaries in release mode
make install    # Copies to ~/.local/bin
```

**Available Make targets:**
| Target | Description |
|--------|-------------|
| `make build` | Debug build of all crates |
| `make release` | Release build of binaries |
| `make install` | Install binaries to `~/.local/bin` |
| `make clean` | Remove build artifacts |
| `make test` | Run all tests |
| `make lint` | Run clippy + fmt check |
| `make fmt` | Format code |
| `make check` | Full CI check (fmt + clippy + build + test) |
| `make docker-build` | Build Docker image |
| `make docker-run` | Run via Docker Compose |
| `make cross-linux` | Cross-compile for Linux (x86_64 + aarch64) |
| `make cross-mac` | Cross-compile for macOS (x86_64 + aarch64) |
| `make dist` | Create release tarballs |

## Release Process

### Automated Releases (GitHub Actions)

#### Every push to `main`

Triggers `.github/workflows/release.yml` which:

1. **Resolves version** — auto-generates `v{last_version}-dev.{commit_count}.{short_sha}`
2. **Builds binaries** for 4 platforms:
   - `x86_64-unknown-linux-musl` (Linux x86_64, static)
   - `aarch64-unknown-linux-gnu` (Linux ARM64)
   - `x86_64-apple-darwin` (macOS Intel)
   - `aarch64-apple-darwin` (macOS Apple Silicon)
3. **Creates tarballs** with all binaries, orchestration config, and README
4. **Generates SHA256 checksums** for each tarball
5. **Builds and pushes Docker image** to GHCR (tagged with dev version + `latest`)
6. **Creates GitHub Release** (marked as prerelease) with binaries and changelog
7. **Publishes npm package** under `next` dist-tag

#### Version tag push (`v*`)

Triggers `.github/workflows/release.yml` which:

1. **Uses the tag as version** (e.g., `v0.2.0`)
2. **Builds binaries** for all 4 platforms
3. **Creates tarballs** with all binaries, orchestration config, and README
4. **Generates SHA256 checksums** for each tarball
5. **Builds and pushes Docker image** to GHCR
6. **Creates GitHub Release** (stable, not prerelease) with binaries and changelog
7. **Publishes npm package** under `latest` dist-tag
8. **Publishes crates** to crates.io
9. **Updates Homebrew formula** with new version and SHA

### Manual Release Dispatch

You can also trigger a release manually via GitHub Actions:

1. Go to Actions → Release → Run workflow
2. Enter version (e.g., `v0.2.0`)
3. Click "Run workflow"

### Version Scheme

| Trigger | Version Format | Release Type | npm dist-tag |
|---------|---------------|--------------|--------------|
| Tag push (`v0.2.0`) | `v0.2.0` | Stable | `latest` |
| Main push | `v0.1.0-dev.42.abc1234` | Pre-release | `next` |
| Manual dispatch | User-specified | Depends on version string | Depends |

## Binary Contents

Each release tarball contains:

```
openflows-v0.1.0-x86_64-unknown-linux-musl/
├── openflows              # Main orchestration binary
├── openflows-setup        # Setup wizard TUI
├── openflows-dashboard    # Live monitoring TUI
├── openflows-doctor       # Diagnostic tool
├── orchestration/         # Agent personas, registry, hooks, skills
├── README.md
└── LICENSE
```

## Platform Support

| Platform | Architecture | Binary | Docker | Homebrew |
|----------|-------------|--------|--------|----------|
| Linux | x86_64 | ✅ Static (musl) | ✅ | — |
| Linux | aarch64 | ✅ Dynamic (glibc) | ✅ | — |
| macOS | x86_64 (Intel) | ✅ | ✅ | ✅ |
| macOS | aarch64 (Apple Silicon) | ✅ | ✅ | ✅ |

## Prerequisites

All installation methods require:

| Dependency | Version | Required By |
|------------|---------|-------------|
| Rust | 1.70+ | Build from source, cargo install |
| Node.js | 18+ | GitHub MCP server, Claude Code CLI |
| Git | Any | All methods |
| Claude Code CLI | Latest | Agent execution |

The one-line installer handles all of these automatically.