# Building OpenFlows from Source

> Official site: [openflows.dev](https://openflows.dev)

This guide walks you through building OpenFlows from source. Whether you're contributing to the project or want to run the latest development version, follow these steps.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Clone the Repository](#clone-the-repository)
- [Build Commands](#build-commands)
- [Available Binaries](#available-binaries)
- [Install Globally](#install-globally)
- [Build Troubleshooting](#build-troubleshooting)
- [Build Artifacts](#build-artifacts)
- [Next Steps](#next-steps)

## Prerequisites

### Required

| Tool | Version | Purpose |
|------|---------|---------|
| **Rust** | 1.70+ | Core runtime and build system |
| **Node.js** | 18+ | GitHub MCP server dependency |
| **CLI Backend** | Latest | AI agent execution (Claude Code or Codex) |

### Installing Prerequisites

**Rust (via rustup):**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustc --version  # Verify installation
```

**Node.js:**
```bash
# macOS (Homebrew)
brew install node

# Ubuntu/Debian
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# Windows (winget)
winget install OpenJS.NodeJS.LTS
```

**CLI Backend (choose one):**

*Codex CLI (recommended for OpenAI-compatible gateways):*
```bash
npm install -g @openai/codex
codex login --with-api-key
codex login status
```

*Claude Code CLI (for direct Anthropic access):*
```bash
npm install -g @anthropic-ai/claude-code
claude auth login
```

See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) and [docs/cli-backend-configuration.md](docs/cli-backend-configuration.md) for detailed setup.

## Clone the Repository

```bash
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
```

## Build Commands

### Development Build

Fast build with debug symbols:

```bash
# Build all workspace crates
cargo build --workspace

# Or use the Makefile
make build
```

Binary location: `target/debug/`

### Release Build

Optimized build for production use:

```bash
# Build the main package (openflows) in release mode
cargo build --release -p openflows

# Or use the Makefile
make release
```

Binary location: `target/release/`

Release builds are significantly faster at runtime but take longer to compile.

### Build Specific Binary

```bash
# Main orchestration binary
cargo build --bin openflows

# Mocked demonstration (no API keys needed)
cargo build --bin demo

# Interactive setup wizard
cargo build --bin openflows-setup

# Live dashboard monitor
cargo build --bin openflows-dashboard

# Environment diagnostic tool
cargo build --bin openflows-doctor
```

### Makefile Targets

The root `Makefile` wraps common cargo operations:

```bash
# Show all available targets
make help

# Build, test, lint, and format in one command
make check
```

| Target | Description |
|--------|-------------|
| `make build` | Build all binaries (debug) |
| `make release` | Build all binaries (release) |
| `make install` | Copy release binaries to `~/.local/bin` |
| `make test` | Run workspace tests (uses `cargo nextest` if available) |
| `make lint` | Run `cargo fmt --check` and `cargo clippy` |
| `make fmt` | Format all code |
| `make check` | Full CI-quality check (fmt + lint + build + test) |
| `make clean` | Remove build artifacts |
| `make docker-build` | Build Docker image |
| `make docker-run` | Run via Docker Compose |

## Available Binaries

| Binary | Purpose | When to Use |
|--------|---------|-------------|
| `openflows` | Main entrypoint — production orchestration with real GitHub API and CLI backend | Running the autonomous team against real repos |
| `demo` | Mocked demonstration with fake data | Quick smoke test without API keys |
| `openflows-setup` | Interactive TUI setup wizard | First-time configuration |
| `openflows-dashboard` | Live worker status monitor | Watching agents work in real time |
| `openflows-doctor` | Environment diagnostic tool | Troubleshooting setup problems |

## Install Globally

Install `openflows` and companion binaries to `~/.local/bin/`:

```bash
make install
```

Or manually with cargo:

```bash
cargo install --path binary
```

After installation, run from anywhere:
```bash
openflows
openflows-doctor
```

Make sure `~/.local/bin` (or `~/.cargo/bin` if using `cargo install`) is in your `PATH`.

## Build Troubleshooting

### "Cargo not found"

Install Rust via rustup (see Prerequisites above).

### Compilation errors

```bash
# Update Rust toolchain
rustup update

# Clean and rebuild
cargo clean
cargo build --workspace
```

### "linker 'cc' not found"

Install a C compiler:
```bash
# Ubuntu/Debian
sudo apt-get install build-essential

# macOS (Xcode Command Line Tools)
xcode-select --install

# Windows
# Install Visual Studio Build Tools or MSVC
```

### OpenSSL errors

```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev

# macOS
brew install openssl
export OPENSSL_DIR=$(brew --prefix openssl)
```

### "node: command not found" (when running orchestration)

Install Node.js (see Prerequisites above). Required for GitHub MCP server.

## Build Artifacts

After a successful build:

```
target/
├── debug/
│   ├── openflows             # Development binary
│   ├── demo                  # Mocked demo
│   ├── openflows-setup       # Setup wizard
│   ├── openflows-dashboard   # Dashboard
│   └── openflows-doctor      # Diagnostics
└── release/
    ├── openflows             # Production binary (optimized)
    ├── demo                  # Mocked demo
    ├── openflows-setup       # Setup wizard
    ├── openflows-dashboard   # Dashboard
    └── openflows-doctor      # Diagnostics
```

## Next Steps

After building:
1. Copy `.env.example` to `.env` and configure your credentials
2. Run `cargo run --bin demo` for a smoke test
3. See [RUN.md](RUN.md) for configuration and execution instructions
4. See [CONTRIBUTING.md](CONTRIBUTING.md) for development workflow and testing
