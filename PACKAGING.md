# Distribution & Packaging

This document describes how OpenFlows is packaged and distributed across platforms.

## Installation Methods

### 1. One-Line Installer (All Platforms)

```bash
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash
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

### 2. Homebrew (macOS)

```bash
brew tap The-AgenticFlow/openflows
brew install openflows
```

The Homebrew formula is maintained at `packaging/homebrew/openflows.rb`.

### 3. Docker

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

### 4. Cargo (Rust Package Manager)

```bash
cargo install openflows
```

All crates are published to crates.io. The `openflows` package includes all binaries.

### 5. npm (Node.js Package Manager)

```bash
npm install -g openflows
```

The npm package downloads the correct pre-built binary for your platform during installation (with a fallback to building from source if Rust is available). Supports Linux and macOS on x64 and arm64.

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

Tagging a release triggers the full release pipeline:

```bash
git tag v0.2.0
git push origin v0.2.0
```

This triggers `.github/workflows/release.yml` which:

1. **Builds binaries** for 4 platforms:
   - `x86_64-unknown-linux-musl` (Linux x86_64, static)
   - `aarch64-unknown-linux-gnu` (Linux ARM64)
   - `x86_64-apple-darwin` (macOS Intel)
   - `aarch64-apple-darwin` (macOS Apple Silicon)

2. **Creates tarballs** with all binaries, orchestration config, and README

3. **Generates SHA256 checksums** for each tarball

4. **Builds and pushes Docker image** to GHCR

5. **Creates GitHub Release** with:
   - Auto-generated changelog from git history
   - All tarballs and checksums as assets
   - Installation instructions in the release body

6. **Publishes crates** to crates.io (for stable releases only)

### Manual Release Dispatch

You can also trigger a release manually via GitHub Actions:

1. Go to Actions → Release → Run workflow
2. Enter version (e.g., `v0.2.0`)
3. Click "Run workflow"

## Binary Contents

Each release tarball contains:

```
openflows-v0.1.0-x86_64-unknown-linux-musl/
├── agentflow              # Main orchestration binary
├── agentflow-setup        # Setup wizard TUI
├── agentflow-dashboard    # Live monitoring TUI
├── agentflow-doctor       # Diagnostic tool
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
