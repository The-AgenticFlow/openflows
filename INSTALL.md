# Installation Guide

> Complete guide for installing, configuring, and running OpenFlows.

## Table of Contents

- [Requirements](#requirements)
- [End User Installation](#end-user-installation)
  - [Option 1: One-Line Install (Recommended)](#option-1-one-line-install-recommended)
  - [Option 2: Homebrew (macOS)](#option-2-homebrew-macos)
  - [Option 3: Docker](#option-3-docker)
  - [Option 4: npm](#option-4-npm)
  - [Option 5: Cargo](#option-5-cargo)
- [Developer Installation (From Source)](#developer-installation-from-source)
  - [Step 1: Clone & Prerequisites](#step-1-clone--prerequisites)
  - [Step 2: Build](#step-2-build)
  - [Step 3: Configure Environment](#step-3-configure-environment)
  - [Step 4: Verify](#step-4-verify)
- [After Installation](#after-installation)
- [Environment Setup](#environment-setup)
- [Proxy Configuration (LiteLLM Routing)](#proxy-configuration-litellm-routing)
- [Per-Agent Configuration](#per-agent-configuration)
- [Running OpenFlows](#running-openflows)
- [Troubleshooting](#troubleshooting)

## Requirements

- **Rust 1.70+** — Required for building from source. Pre-built binaries do not need Rust.
- **Node.js 18+** — Required for the GitHub MCP server.
- **Claude Code CLI** or **Codex CLI** — The AI agent execution backend. See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) and [docs/cli-backend-configuration.md](docs/cli-backend-configuration.md).
- **API Keys:**
  - `GITHUB_PERSONAL_ACCESS_TOKEN` (required)
  - Provider key depending on mode: `ANTHROPIC_API_KEY`, `FIREWORKS_API_KEY`, `OPENAI_API_KEY`, etc.

## End User Installation

Choose one of these if you only want to run OpenFlows and do not plan to modify the source code.

### Option 1: One-Line Install (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/openflows/main/scripts/install.sh | bash
```

This installs all binaries to `~/.local/bin` and offers to run the setup wizard.

### Option 2: Homebrew (macOS)

```bash
brew tap The-AgenticFlow/openflows
brew install openflows
```

### Option 3: Docker

```bash
docker run -it --rm \
  -v "$HOME/.agentflow:/home/openflows/.agentflow" \
  -v "$(pwd):/workspace" \
  -e ANTHROPIC_API_KEY=your_key \
  -e GITHUB_PERSONAL_ACCESS_TOKEN=your_token \
  ghcr.io/the-agenticflow/openflows:latest setup
```

### Option 4: npm

```bash
# Install globally
npm install -g @the-agenticflow/openflows

# Verify installation
openflows --version

# Run setup wizard
openflows-setup
```

**Update:** `npm update -g @the-agenticflow/openflows`

**Uninstall:** `npm uninstall -g @the-agenticflow/openflows`

### Option 5: Cargo

```bash
cargo install openflows
openflows-setup
openflows
```

---

## Developer Installation (From Source)

Use this path if you want to contribute code, debug issues, or run the latest unreleased changes.

### Step 1: Clone & Prerequisites

```bash
# Clone the repository
git clone https://github.com/The-AgenticFlow/openflows.git
cd AgentFlow

# Verify Rust is installed (need 1.70+)
rustc --version
# If not installed: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Verify Node.js is installed (need 18+)
node --version

# Verify Git is installed
git --version
```

### Step 2: Build

OpenFlows provides a Makefile with common tasks. You can use it or run cargo directly:

**Using Make (recommended):**

```bash
# Build all binaries in debug mode (fastest compile)
make build

# Build all binaries in release mode (optimized)
make release

# Install locally to ~/.local/bin
make install
```

**Using Cargo directly:**

```bash
# Development build
cargo build --workspace

# Release build
cargo build --release -p openflows
```

**Available binaries built:**

| Binary | Purpose |
|--------|---------|
| `openflows` | Main orchestration — connects to GitHub, spawns AI agents, creates real PRs |
| `demo` | Mocked demonstration with fake data (no API keys required) |
| `openflows-setup` | Interactive TUI setup wizard |
| `openflows-dashboard` | Live worker status monitor |
| `openflows-doctor` | Environment diagnostic tool |

After a successful build, binaries are located in `target/debug/` or `target/release/`.

### Step 3: Configure Environment

```bash
# Copy the example environment file
cp .env.example .env

# Edit .env with your settings
nano .env   # or your preferred editor
```

The `.env.example` file documents three setup modes:

- **Mode A (Recommended):** Codex + Fireworks — simplest, no proxy needed.
- **Mode B:** Claude + Direct Anthropic Key — if you have an Anthropic API key.
- **Mode C:** Claude + Proxy — for third-party gateways that require protocol translation.

At minimum you must set:
- `GITHUB_REPOSITORY` — target repo in `owner/repo` format
- `GITHUB_PERSONAL_ACCESS_TOKEN` — GitHub PAT with `repo` scope
- One LLM provider key (see `.env.example` for which key your mode needs)

### Step 4: Verify

```bash
# Run diagnostics
./target/debug/openflows-doctor

# Run the mocked demo (no API keys needed)
cargo run --bin demo

# Run the full test suite
cargo test --workspace
```

---

## After Installation

### Standard Commands (All Install Methods)

1. **Configure** — `openflows-setup` runs the interactive TUI wizard
2. **Verify** — `openflows-doctor` checks your environment
3. **Run** — `openflows` starts the autonomous team
4. **Monitor** — `openflows-dashboard` shows live worker status

### npm-Specific Workflow

If you installed via npm, you can also use npx without global installation:

```bash
# Run setup wizard without installing
npx @the-agenticflow/openflows-setup

# Start orchestration directly
npx @the-agenticflow/openflows

# Check status
npx @the-agenticflow/openflows-doctor
```

---

## Environment Setup

### Configuration File

Copy the example file and configure your credentials:

```bash
cp .env.example .env
```

Edit `.env` with your API keys and settings.

### Required Environment Variables

| Variable | Description |
|----------|-------------|
| `GITHUB_REPOSITORY` | Target repository in `owner/repo` format |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | GitHub PAT with `repo` scope |
| `ANTHROPIC_API_KEY` | Anthropic API key (required in direct Claude mode) |
| `FIREWORKS_API_KEY` | Fireworks API key (required in Codex + Fireworks mode) |

### All Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GITHUB_REPOSITORY` | Yes | Target repository (`owner/repo`) |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | Yes | GitHub PAT with `repo` scope |
| `DEFAULT_CLI` | Yes* | CLI backend: `codex` or `claude` |
| `ANTHROPIC_API_KEY` | Yes* | Anthropic API key (required in direct Claude mode) |
| `FIREWORKS_API_KEY` | Yes* | Fireworks API key (required in Codex + Fireworks mode) |
| `OPENAI_API_KEY` | Yes* | OpenAI/Fireworks key (required in Codex mode) |
| `PROXY_URL` | No | LiteLLM proxy URL for routing |
| `PROXY_API_KEY` | No | API key for hosted LiteLLM proxy |
| `GATEWAY_URL` | No | OpenAI-compatible gateway URL |
| `GATEWAY_API_KEY` | No | Gateway API key |
| `CLAUDE_PATH` | No | Path to Claude CLI binary (default: `claude`) |
| `CODEX_PATH` | No | Path to Codex CLI binary (default: `codex`) |
| `REDIS_URL` | No | Redis URL for persistent state |
| `AGENTFLOW_DOMAIN_MODE` | No | `manual` or `all` — `all` allows unrestricted internet, `manual` restricts to `AGENTFLOW_ALLOWED_DOMAINS` |
| `AGENTFLOW_ALLOWED_DOMAINS` | No | Comma-separated list of allowed domains (e.g. `api.github.com,*.github.com,pypi.org`). Only used when `AGENTFLOW_DOMAIN_MODE=manual` |
| `RUST_LOG` | No | Log level (default: `info`) |
| `AGENTFLOW_WORKSPACE_ROOT` | No | Workspace root directory |

---

## Proxy Configuration (LiteLLM Routing)

Each agent has different workload requirements. Instead of routing all agents through the same expensive model, OpenFlows supports per-agent model routing via a **LiteLLM proxy**.

### Default Model Assignments

| Agent | Model | Why |
|-------|-------|-----|
| **FORGE** | `anthropic/claude-sonnet-4-5` | Primary coding agent, needs top-tier reasoning |
| **NEXUS** | `anthropic/claude-sonnet-4-5` | Orchestrator, needs reliable decision-making |
| **SENTINEL** | `gemini/gemini-2.5-pro` | Code review, strong reasoning at lower cost |
| **VESSEL** | `groq/llama-3.3-70b-versatile` | CI/CD scripting, fast and cheap (free tier) |
| **LORE** | `openai/gpt-4o-mini` | Documentation, lightweight task |

### How It Works

1. Claude Code supports `ANTHROPIC_BASE_URL` and `ANTHROPIC_API_KEY` env vars
2. A LiteLLM proxy receives all requests and routes based on the API key (routing key)
3. Each agent is spawned with its own routing key (e.g., `forge-key`, `sentinel-key`)
4. The proxy maps each routing key to the correct backend model via `litellm_config.yaml`
5. Fallback is configured — any provider failure falls back to `anthropic/claude-sonnet-4-5`

### Self-Hosted LiteLLM (Docker Compose)

```bash
# Start the proxy, Redis, and agent-team
docker compose up

# Or just the proxy for local dev
docker compose up proxy redis
```

The proxy runs on port 4000 with a health check. See `docker-compose.yml` and `litellm_config.yaml` for configuration.

### Hosted LiteLLM (e.g., LiteLLM Cloud)

```bash
# .env
PROXY_URL=https://your-litellm-instance.example.com
PROXY_API_KEY=sk-your-hosted-litellm-key
```

When using a hosted proxy, set `PROXY_API_KEY` to your proxy authentication key. The provider API keys (`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, etc.) are configured on the proxy side, not in the OpenFlows `.env`.

### OpenAI-Only Gateways (Local Anthropic Proxy)

If your LLM gateway only supports the OpenAI Chat Completions format (`/v1/chat/completions`), Claude CLI will fail because it speaks Anthropic Messages API (`/v1/messages`). OpenFlows includes a local protocol translator:

```bash
# Terminal 1: Start the proxy (reads .env automatically)
./scripts/start_proxy.sh

# Terminal 2: Run orchestration
cargo run --bin openflows
```

Configure `.env`:

```env
# Claude CLI and Nexus send Anthropic requests to the LOCAL proxy
PROXY_URL=http://localhost:8080/v1
PROXY_API_KEY=your-gateway-api-key

# The LOCAL proxy forwards OpenAI-format requests to the REMOTE gateway
GATEWAY_URL=https://api.ai.camer.digital/v1/
GATEWAY_API_KEY=your-gateway-api-key
```

When your provider adds native Anthropic support, change `PROXY_URL` to point directly to the gateway and remove `GATEWAY_*`.

### Disabling Proxy (Direct API Access)

If `PROXY_URL` is not set, all agents use direct API access with `ANTHROPIC_API_KEY` — this is the default behavior and requires no proxy setup.

```env
# Direct mode (no proxy)
LLM_FALLBACK=anthropic,gemini,openai
ANTHROPIC_API_KEY=sk-ant-xxxxx
GEMINI_API_KEY=AIzaSyxxxxx
OPENAI_API_KEY=sk-proj-xxxxx
```

---

## Per-Agent Configuration

### GitHub Tokens

Each agent can use a separate GitHub PAT. This is useful for:

- **Rate limit isolation** — one agent hitting rate limits doesn't block others
- **Scope restriction** — give VESSEL only `repo` scope, give FORGE `repo` + `workflow`
- **Token rotation** — rotate one agent's token without affecting the rest of the team

Set the env vars referenced in `github_token_env` before running:

```bash
export AGENT_NEXUS_GITHUB_TOKEN=ghp_nexus_token
export AGENT_FORGE_GITHUB_TOKEN=ghp_forge_token
export AGENT_SENTINEL_GITHUB_TOKEN=ghp_sentinel_token
export AGENT_VESSEL_GITHUB_TOKEN=ghp_vessel_token
export AGENT_LORE_GITHUB_TOKEN=ghp_lore_token
```

If any `github_token_env` variable is unset, the system falls back to `GITHUB_PERSONAL_ACCESS_TOKEN`.

### Registry Configuration

The registry at [`orchestration/agent/registry.json`](orchestration/agent/registry.json) is the **single source of truth** for team membership, worker scaling, and per-agent routing.

#### Structure

```json
{
  "team": [
    {
      "id": "forge",
      "cli": "claude",
      "active": true,
      "instances": 2,
      "model_backend": "accounts/fireworks/models/glm-5",
      "routing_key": "forge-key",
      "github_token_env": "AGENT_FORGE_GITHUB_TOKEN"
    }
  ]
}
```

#### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Agent name. Used in logs, worktree names (`forge-1`, `forge-2`), and branch names. |
| `cli` | string | CLI tool to spawn. Currently only `"claude"` or `"codex"` is supported. |
| `active` | bool | When `false`, the agent is excluded from orchestration entirely. |
| `instances` | int | Number of parallel worker slots. FORGE uses this directly (`forge-1`, `forge-2`, ...). Other agents with `instances > 1` get numbered slots (`vessel-1`, `vessel-2`). Agents with `instances == 1` use their bare ID (`nexus`, `sentinel`). |
| `model_backend` | string | Model identifier passed to the LLM client. Can be a direct provider path (`anthropic/claude-sonnet-4-5`) or a gateway path (`accounts/fireworks/models/glm-5`). |
| `routing_key` | string | LiteLLM proxy routing key. When `PROXY_URL` is set, this key is used to route requests to the correct backend model. When unset, the agent falls back to direct API access. |
| `github_token_env` | string | Environment variable name that holds the GitHub PAT for this agent. Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN` if not set. Enables per-agent token rotation and scoping. |
| `allowed_domains` | array or null | Network domains this agent can access (for sandbox configuration). Falls back to the registry-level `allowed_domains` if not set. Use `["*"]` to allow unrestricted internet access. Examples: `["api.github.com", "*.github.com", "pypi.org"]` |

#### Common Operations

**Scale FORGE workers:**

```json
{ "id": "forge", "instances": 4 }  // → forge-1, forge-2, forge-3, forge-4
```

**Disable an agent:**

```json
{ "id": "lore", "active": false }  // LORE will not be invoked
```

**Rotate a GitHub token:**

```bash
export AGENT_FORGE_GITHUB_TOKEN=ghp_new_token_here
```

**Switch a model backend:**

```json
{ "id": "forge", "model_backend": "anthropic/claude-sonnet-4-5" }
```

---

## Running OpenFlows

### Production Mode (Recommended)

Run the full orchestration with real GitHub API and Claude CLI:

```bash
# Via cargo
cargo run --bin openflows

# Or directly after build
./target/release/openflows
```

This mode:
- Connects to GitHub API to fetch issues
- Spawns Claude CLI for code generation
- Creates real pull requests
- Polls CI status and merges PRs

### Development Mode

Test with mocked data (no API keys required):

```bash
cargo run --bin demo
```

Uses in-memory implementations without external API calls.

### Dashboard

Monitor worker status in real time:

```bash
cargo run --bin openflows-dashboard
```

---

## Troubleshooting

### "GITHUB_REPOSITORY must be set"

Set the target repository in `.env`:

```env
GITHUB_REPOSITORY=owner/repo
```

### "GitHub token must be set"

Set your GitHub PAT:

```env
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxx
```

### "claude: command not found" or "codex: command not found"

Install the CLI backend or set the path:

```env
CLAUDE_PATH=/path/to/claude
CODEX_PATH=/path/to/codex
```

See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) for detailed setup.

### Connection refused (Redis)

Ensure Redis is running:

```bash
docker run -d -p 6379:6379 redis
```

Or remove `REDIS_URL` to use in-memory store.

### API rate limit exceeded

- Use LiteLLM proxy for rate limit management
- Reduce concurrent workers in `orchestration/agent/registry.json`
- Add fallback providers in `LLM_FALLBACK`

### Build errors

```bash
# Update Rust toolchain
rustup update

# Clean and rebuild
cargo clean
make build
```

---

## Additional Resources

- **[BUILD.md](BUILD.md)** — Detailed build instructions and platform-specific notes
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — Contributor workflow, testing, and style guide
- **[TUTORIAL.md](TUTORIAL.md)** — Complete tutorial with logs and troubleshooting
- **[RUN.md](RUN.md)** — Day-to-day running and configuration reference
- **[docs/demo.md](docs/demo.md)** — Live flow walkthrough
- **[docs/setup-claude-cli.md](docs/setup-claude-cli.md)** — CLI backend setup (Claude Code and Codex)
