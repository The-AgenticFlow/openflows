# Contributing to OpenFlows

> We're building the future of autonomous software development, and we need your help.

Official site: [openflows.dev](https://openflows.dev)

## Table of Contents

- [What is OpenFlows?](#what-is-openflows)
- [Quick Start for Contributors](#quick-start-for-contributors)
- [Project Architecture](#project-architecture)
- [Development Workflow](#development-workflow)
- [Running & Testing](#running--testing)
- [Development Tips](#development-tips)
- [Pre-Submission Checklist](#pre-submission-checklist)
- [Recognition](#recognition)
- [Get Help](#get-help)
- [Additional Resources](#additional-resources)

## What is OpenFlows?

OpenFlows is an autonomous AI development team that turns GitHub issues into working code with pull requests — all without human intervention.

Think of it as having a team of AI agents (NEXUS, FORGE, VESSEL, SENTINEL, LORE) that collaborate to build software just like a human team would.

## Quick Start for Contributors

### Step 1: Fork & Clone

```bash
git clone https://github.com/YOUR_USERNAME/OpenFlows.git
cd OpenFlows
```

### Step 2: Set Up Environment

```bash
# Copy environment template
cp .env.example .env

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install the CLI backend (choose one)
# Option A: Codex CLI (OpenAI-compatible)
npm install -g @openai/codex
codex login

# Option B: Claude Code CLI (Anthropic native)
npm install -g @anthropic-ai/claude-code
claude auth login
```

### Step 3: Configure

Edit `.env` with your API keys. The `.env.example` file describes three setup modes:

- **Mode A (recommended):** Codex + Fireworks — simplest, no proxy needed.
- **Mode B:** Claude + Direct Anthropic Key — if you have an Anthropic API key.
- **Mode C:** Claude + Proxy — for third-party gateways requiring protocol translation.

Minimum required variables:

```bash
# Required: GitHub access
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
GITHUB_REPOSITORY=your-org/your-repo

# Required: CLI backend selection
DEFAULT_CLI=codex   # or "claude"

# Required: Provider keys (depends on mode)
FIREWORKS_API_KEY=your-key-here
OPENAI_API_KEY=your-key-here
# OR for Claude mode:
# ANTHROPIC_API_KEY=sk-ant-api03-your-key-here

# Optional: explicit path to CLI binary
CODEX_PATH=$(which codex)
CLAUDE_PATH=$(which claude)
```

### Step 4: Build & Test

```bash
# Build all workspace crates (debug)
make build

# Run the full test suite
make test

# Run formatting and linting
make fmt
make lint

# Run CI-quality checks (format + lint + build + test)
make check
```

**Smoke test without API keys:**

```bash
# Mocked demo (no API keys required)
cargo run --bin demo
```

**Verify the build against a real target repo:**

```bash
# Ensure .env is configured, then run
cargo run --bin openflows
```

---

## Project Architecture

### Workspace Layout

OpenFlows is a Rust workspace composed of multiple crates:

```
openflows/
├── Cargo.toml              # Workspace root
├── Makefile                # Common build tasks
├── binary/                 # CLI entry points
│   └── src/bin/
│       ├── openflows.rs    # Main orchestration
│       ├── demo.rs         # Mocked demo
│       ├── setup.rs        # TUI setup wizard
│       ├── dashboard.rs    # Live monitoring dashboard
│       └── doctor.rs       # Environment diagnostics
├── crates/
│   ├── pocketflow-core/    # Flow engine, shared store, routing
│   ├── agent-client/       # LLM client + MCP integration
│   ├── agent-nexus/        # Orchestrator node
│   ├── agent-forge/        # Builder node (spawns CLI backend)
│   ├── agent-vessel/       # CI/CD and merge logic
│   ├── agent-lore/         # Documentation generation
│   ├── agent-sentinel/     # Code review and security audit
│   ├── pair-harness/       # Worktree management, process spawning
│   ├── github/             # GitHub API abstraction
│   ├── config/             # Configuration parsing
│   ├── agentflow-tui/      # Terminal UI components
│   └── anthropic-mock/     # Protocol translator proxy
└── orchestration/
    └── agent/
        ├── agents/         # Persona definitions
        └── registry.json   # Agent definitions and routing
```

### Core Components

| Component | Role | Description |
|-----------|------|-------------|
| **NEXUS** | Orchestrator | Assigns work to agents, manages workflow |
| **FORGE** | Developer | Spawns the CLI backend to implement code |
| **VESSEL** | Merger | Merges approved PRs, handles CI |
| **SENTINEL** | Reviewer | Evaluates code quality and security |
| **LORE** | Documenter | Writes docs and ADRs |

### Key Concepts

**SharedStore**: Central key-value store where agents exchange state.
- `tickets` — GitHub issues converted to work items
- `worker_slots` — Agent worker assignments
- `pending_prs` — Pull requests awaiting merge

**PocketFlow**: The engine that executes the agent graph and manages state transitions.

For deep technical details, see:
- [docs/shared-store.md](docs/shared-store.md)
- [docs/architecture/system-behavior.md](docs/architecture/system-behavior.md)
- [docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md)

---

## Development Workflow

### Good First Issues

Look for issues labeled:
- `good first issue` — Simple tasks to get started
- `help wanted` — We need community help
- `documentation` — Improve docs, no code required
- `rust` — Rust-specific improvements

### First Contribution Ideas

1. Fix a typo in documentation
2. Add a test for existing functionality
3. Improve error messages for better UX
4. Add logging to help with debugging
5. Write a tutorial based on your experience

### Contribution Process

> Before you start, read the [Contribution Guidelines](docs/contribution_guidelines.md) for branch naming, commit messages, and PR conventions.

1. **Find or Create an Issue**
   - Check [existing issues](https://github.com/The-AgenticFlow/OpenFlows/issues)
   - Comment on an issue to express interest
   - Or [create a new issue](https://github.com/The-AgenticFlow/OpenFlows/issues/new) for bugs/features

2. **Get Assigned**
   - Wait for a maintainer to assign you
   - This prevents duplicate work

3. **Create a Branch**
   ```bash
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/issue-description
   ```

4. **Make Changes**
   - Follow Rust best practices
   - Add tests for new functionality
   - Update documentation if needed
   - Keep commits focused and atomic

5. **Test Your Changes**
   ```bash
   # Run all tests
   cargo test --workspace

   # Check formatting
   cargo fmt -- --check

   # Run linter
   cargo clippy -- -D warnings

   # Run the demo to verify
   cargo run --bin demo
   ```

6. **Commit & Push**
   ```bash
   git add .
   git commit -m "feat: add feature description"
   git push origin feature/your-feature-name
   ```

   **Commit message format:**
   - `feat:` — New feature
   - `fix:` — Bug fix
   - `docs:` — Documentation changes
   - `test:` — Adding tests
   - `refactor:` — Code refactoring
   - `chore:` — Maintenance tasks

7. **Create a Pull Request**
   - Go to GitHub and create a PR from your branch
   - Fill out the PR template
   - Link to the issue you're fixing
   - Request review from maintainers

---

## Running & Testing

### Unit Tests

```bash
# Run all unit tests
make test
# Or directly with cargo:
cargo test --workspace

# Run tests for a specific crate
cargo test -p agent-nexus
cargo test -p agent-forge
cargo test -p pocketflow-core
```

### End-to-End Tests

```bash
# Test Nexus decision making
cargo test -p agent-nexus --test nexus_e2e

# Test Forge suspension logic
cargo test -p agent-forge --test forge_claude_e2e
```

### Demo Mode (No API Keys Needed)

```bash
# Run the mocked demo
cargo run --bin demo
```

### Running in Different Modes

**Mock Mode** (safe, no API keys):
```bash
cargo run --bin demo
```

**Real Mode** (connects to live LLMs and GitHub):
```bash
# Ensure .env is configured
cargo run --bin openflows

**Dashboard** (monitor workers):
```bash
cargo run --bin openflows-dashboard
```

**Doctor** (check environment):
```bash
cargo run --bin openflows-doctor
```

---

## Development Tips

### Makefile Targets

The `Makefile` provides common tasks:

| Target | Description |
|--------|-------------|
| `make build` | Build all binaries (debug) |
| `make release` | Build all binaries (release) |
| `make install` | Install binaries to `~/.local/bin` |
| `make test` | Run all tests |
| `make lint` | Run `cargo fmt --check` and `cargo clippy` |
| `make fmt` | Format all code with `cargo fmt` |
| `make check` | Full CI pass: fmt + lint + build + test |
| `make clean` | Remove build artifacts |
| `make docker-build` | Build Docker image |
| `make docker-run` | Run via Docker Compose |

### Crate-Level Development

Build or test a specific crate:

```bash
# Build a single crate
cargo build -p agent-forge

# Run tests for a single crate
cargo test -p agent-nexus

# Run the main binary package
cargo build -p openflows
```

### Common Issues & Solutions

**Issue**: `Failed to spawn FORGE process`
**Solution**: Check `CODEX_PATH` or `CLAUDE_PATH` in `.env` points to a valid binary.

**Issue**: Build failures
**Solution**:
```bash
rustup update
cargo clean
make build
```

**Issue**: Test failures
**Solution**: Ensure all prerequisites are installed (Rust, Node.js).

**Issue**: `linker 'cc' not found`
**Solution**: Install a C compiler:
```bash
# Ubuntu/Debian
sudo apt-get install build-essential

# macOS
xcode-select --install
```

**Issue**: OpenSSL errors
**Solution**:
```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev

# macOS
brew install openssl
export OPENSSL_DIR=$(brew --prefix openssl)
```

---

## Pre-Submission Checklist

Before opening a pull request, ensure you have:

- [ ] Run `make check` (or manually: `cargo fmt`, `cargo clippy`, `cargo test --workspace`)
- [ ] Added tests for new functionality
- [ ] Updated documentation if you changed user-facing behavior
- [ ] Verified the demo still runs: `cargo run --bin demo`
- [ ] Linked the related issue in your PR description
- [ ] Written a clear commit message following the format above

---

## Recognition

Contributors will be:
- Listed in CONTRIBUTORS.md
- Mentioned in release notes
- Added to the organization after 3+ quality contributions

---

## Get Help

- **Discord**: [Join our community](https://discord.gg/Zf6PTQAgE)
- **GitHub Discussions**: [Ask questions](https://github.com/The-AgenticFlow/OpenFlows/discussions)
- **Issues**: [Report bugs](https://github.com/The-AgenticFlow/OpenFlows/issues)

---

## Additional Resources

- [Official Website](https://openflows.dev)
- [Contribution Guidelines](docs/contribution_guidelines.md)
- [Tutorial](TUTORIAL.md)
- [Demo Walkthrough](DEMO.md)
- [Packaging Guide](PACKAGING.md)
- [Security Policy](orchestration/agent/standards/SECURITY.md)
- [Build Guide](BUILD.md)
- [Running Guide](RUN.md)

---

Thank you for contributing! Every contribution matters — whether it's fixing a typo, adding a test, or building a major feature. You're helping shape the future of autonomous software development.
