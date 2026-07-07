# Installation Guide

> Complete guide for installing, configuring, and running OpenFlows.

## Table of Contents

- [Requirements](#requirements)
- [Choose Your Deployment Mode](#choose-your-deployment-mode)
- [End User Installation](#end-user-installation)
  - [Option 1: Docker Compose — Full Coder Stack (Recommended)](#option-1-docker-compose--full-coder-stack-recommended)
  - [Option 2: One-Line Binary Install (Local Mode)](#option-2-one-line-binary-install-local-mode)
  - [Option 3: Homebrew (macOS)](#option-3-homebrew-macos)
  - [Option 4: npm](#option-4-npm)
  - [Option 5: Cargo](#option-5-cargo)
- [Developer Installation (From Source)](#developer-installation-from-source)
  - [Step 1: Clone & Prerequisites](#step-1-clone--prerequisites)
  - [Step 2: Build](#step-2-build)
  - [Step 3: Configure Environment](#step-3-configure-environment)
  - [Step 4: Verify](#step-4-verify)
- [After Installation](#after-installation)
- [Environment Setup](#environment-setup)
- [Coder Mode](#coder-mode)
- [AI Gateway & Model Routing](#ai-gateway--model-routing)
- [Proxy Configuration (LiteLLM Routing)](#proxy-configuration-litellm-routing)
- [Per-Agent Configuration](#per-agent-configuration)
- [Running OpenFlows](#running-openflows)
- [Troubleshooting](#troubleshooting)

## Requirements

- **Rust 1.70+** — Required for building from source. Pre-built binaries do not need Rust.
- **Node.js 18+** — Required for the GitHub MCP server.
- **Docker 24+** — Required for the Docker Compose Coder stack (Coder mode only). Not needed for local-mode standalone runs.
- **Claude Code CLI** or **Codex CLI** — The AI agent execution backend.
  - In **Coder mode**, the CLI is installed inside each ephemeral workspace via the configured [Coder Registry module](https://registry.coder.com) — you do not need to install it on the host.
  - In **Local mode**, install it yourself. See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) and [docs/cli-backend-configuration.md](docs/cli-backend-configuration.md).
- **API Keys:**
  - `GITHUB_PERSONAL_ACCESS_TOKEN` (required)
  - Provider key depending on mode: `ANTHROPIC_API_KEY`, `FIREWORKS_API_KEY`, `OPENAI_API_KEY`, etc.
  - In **Coder mode** with AI Gateway enabled, provider keys live in the Coder control plane (or LiteLLM config) — not in `.env`.

## Choose Your Deployment Mode

OpenFlows supports two deployment modes. The registry's `workspace_provider` field controls which one each agent uses (`"coder"` or `"local"`).

| Mode | Workspaces | LLM routing | Keys injected into agents? | Requires Docker? |
|------|------------|-------------|----------------------------|------------------|
| **Coder** (default) | Ephemeral Coder workspaces per agent | AI Gateway (primary) / LiteLLM (fallback) | No — keys stay in the control plane | Yes (`docker compose --profile coder`) |
| **Local** | Local git worktrees on the host | LiteLLM proxy or direct API keys | Yes — keys passed via env vars | No |

You can mix modes per agent in `registry.json`, but production defaults to Coder mode for all agents.

## End User Installation

Choose one of these if you only want to run OpenFlows and do not plan to modify the source code.

### Option 1: Docker Compose — Full Coder Stack (Recommended)

One command brings up Coder + PostgreSQL + LiteLLM (fallback) + Redis + OpenFlows:

```bash
git clone https://github.com/The-AgenticFlow/openflows.git
cd openflows
cp .env.example .env   # edit .env — at minimum GITHUB_REPOSITORY, GITHUB_PERSONAL_ACCESS_TOKEN
docker compose --profile coder up
```

On startup the `CoderBootstrapper` automatically:
1. Waits for the Coder server to become healthy
2. Provisions the admin user and obtains an API token
3. Pushes the role Terraform workspace templates (`openflows-forge`, `openflows-sentinel`, …)

Agents then run inside ephemeral Coder workspaces with the [Claude Code Coder Registry module](https://registry.coder.com/coder/claude-code/coder) and AI Gateway enabled. Set `CODER_URL` and `USE_AI_GATEWAY=true` in `.env` (both have sensible defaults in the Compose file).

> **No Coder Premium license?** Set `USE_AI_GATEWAY=false` in `.env` and OpenFlows falls back to the bundled LiteLLM proxy at `http://proxy:4000` for model routing. Provider keys then go in `litellm_config.yaml`.

### Option 2: One-Line Binary Install (Local Mode)

For development or when you don't need the governed Coder environment — agents run in local git worktrees with direct API access, no Docker required:

```bash
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/openflows/main/scripts/install.sh | bash
```

This installs all binaries to `~/.local/bin` and offers to run the setup wizard. To use Coder mode with the standalone binary, set `CODER_URL` in `.env` and point it at your own Coder server.

### Option 3: Homebrew (macOS)

```bash
brew tap The-AgenticFlow/openflows
brew install openflows
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
cd openflows

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

> **Coder feature flag:** The `coder-client` and `pair-harness` crates are feature-gated behind the `coder` Cargo feature. It is enabled by default in builds intended for Coder-mode deployments. To build Local-only binaries, use `cargo build --workspace --no-default-features` (see [docs/coder-compatibility.md](docs/coder-compatibility.md)).

**Available binaries built:**

| Binary | Purpose |
|--------|---------|
| `openflows` | Main orchestration — connects to GitHub, spawns AI agents in local worktrees or Coder workspaces, creates real PRs |
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

The `.env.example` file documents three LLM modes for standalone local runs:

- **Mode A:** Claude + Anthropic — Claude Code speaks the Anthropic Messages API natively, no proxy needed.
- **Mode B:** Codex + OpenAI — Codex CLI authenticates with OpenAI directly.
- **Mode C (recommended for cost):** Codex + Fireworks — OpenAI-compatible endpoints at lower cost.

For **Coder-mode** deployments, the LLM keys instead live in the Coder control plane (AI Gateway) or `litellm_config.yaml` (fallback) — not in `.env`. See [Coder Mode](#coder-mode) below.

At minimum you must set:
- `GITHUB_REPOSITORY` — target repo in `owner/repo` format
- `GITHUB_PERSONAL_ACCESS_TOKEN` — GitHub PAT with `repo` scope
- One LLM provider key (Local mode) **or** `CODER_URL` + `USE_AI_GATEWAY=true` (Coder mode)

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
| `ANTHROPIC_API_KEY` | Anthropic API key — required in direct Claude (Local) mode; in Coder mode it lives in LiteLLM config or the AI Gateway, not `.env` |
| `FIREWORKS_API_KEY` | Fireworks API key (required in Codex + Fireworks Local mode) |

### All Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GITHUB_REPOSITORY` | Yes | Target repository (`owner/repo`) |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | Yes | GitHub PAT with `repo` scope |
| `DEFAULT_CLI` | Yes* | CLI backend: `codex` or `claude` (ignored in Coder mode — module selection comes from `registry.json`) |
| `ANTHROPIC_API_KEY` | Yes* | Anthropic API key (Local mode only — in Coder mode lives in the gateway/proxy config) |
| `FIREWORKS_API_KEY` | Yes* | Fireworks API key (required in Codex + Fireworks Local mode) |
| `OPENAI_API_KEY` | Yes* | OpenAI/Fireworks key (required in Codex Local mode) |
| `PROXY_URL` | No | LiteLLM proxy URL for routing (Local mode primary, Coder mode fallback — overrides below) |
| `PROXY_API_KEY` | No | API key for hosted LiteLLM proxy |
| `GATEWAY_URL` | No | OpenAI-compatible gateway URL |
| `GATEWAY_API_KEY` | No | Gateway API key |
| `CLAUDE_PATH` | No | Path to Claude CLI binary (default: `claude`) — Local mode only; in Coder mode the module installs it inside the workspace |
| `CODEX_PATH` | No | Path to Codex CLI binary (default: `codex`) — Local mode only |
| `REDIS_URL` | No | Redis URL for SharedStore (agent coordination). Required in Coder mode. In-memory fallback in Local mode |
| `CODER_URL` | No | Coder server URL — when set, agents default to Coder transport automatically |
| `CODER_ADMIN_PASSWORD` | No | Password for the Coder admin user created by `CoderBootstrapper` (default: `Op3nFl0ws!`) |
| `USE_AI_GATEWAY` | No | `true` routes Anthropic calls through Coder's AI Gateway — keys never enter workspace env. `false` falls back to LiteLLM (default: `true` in Coder mode) |
| `LITELLM_PROXY_URL` | No | LiteLLM fallback URL used inside Coder workspaces for non-Anthropic providers (default: `http://proxy:4000`) |
| `HOST_CLAUDE_BINARY` | No | Path to a host-side Claude CLI ELF — bind-mounted read-only into each Coder workspace so the module can skip its startup download (essential when the endpoint is slow). Auto-detected if unset |
| `HOST_CODEX_BINARY` | No | Same as above, for the Codex CLI |
| `SLACK_WEBHOOK_URL` | No | Slack webhook for `awaiting_human` escalation alerts (channel-based) |
| `DISCORD_WEBHOOK_URL` | No | Discord webhook for `awaiting_human` escalations |
| `AGENTFLOW_DOMAIN_MODE` | No | `manual` or `all` — `all` allows unrestricted internet, `manual` restricts to `AGENTFLOW_ALLOWED_DOMAINS` |
| `AGENTFLOW_ALLOWED_DOMAINS` | No | Comma-separated list of allowed domains (e.g. `api.github.com,*.github.com,pypi.org`). Only used when `AGENTFLOW_DOMAIN_MODE=manual` |
| `RUST_LOG` | No | Log level (default: `info`) |
| `AGENTFLOW_WORKSPACE_ROOT` | No | Workspace root directory (Local mode only) |

---

## Coder Mode

When `CODER_URL` is set (or `docker compose --profile coder up` is used), OpenFlows switches to **Coder transport**. Each agent runs inside an ephemeral Coder workspace created from a role-specific Terraform template. Workspaces are created when a ticket enters the pipeline and torn down on merge.

### How it works

1. **`CoderBootstrapper`** runs on OpenFlows startup — idempotently creates the Coder admin user, pushes the role templates (`openflows-forge`, `openflows-sentinel`, …), and stores the API token.
2. **NEXUS** detects a new GitHub issue and provisions an ephemeral workspace from the role template for the assigned agent.
3. The workspace boots the [Coder Registry agent module](https://registry.coder.com) (e.g. `claude-code`), which installs the CLI, sets permissions, and wires up AI Gateway routing.
4. The OpenFlows harness binary runs inside the workspace and enforces typed **SharedStore** contracts — agents coordinate via Redis, not by guessing key formats.
5. **VESSEL** tears the workspace down (stops → waits → deletes) when the PR is merged.

> **Coder governs *where* agents run. OpenFlows governs *how* they coordinate.** See [`docs/ephemeral-coder-workspace-integration.md`](docs/ephemeral-coder-workspace-integration.md) for the full integration architecture.

### Registry module mapping

Each agent's `coder_module` in `registry.json` points to a Coder Registry Terraform module. The default is `claude-code` v5.2.0:

```json
{
  "id": "forge",
  "cli": "claude",
  "workspace_provider": "coder",
  "coder_module": {
    "source": "registry.coder.com/coder/claude-code/coder",
    "version": "5.2.0",
    "params": {
      "enable_ai_gateway": true,
      "permission_mode": "acceptEdits",
      "workdir": "/home/coder/workspace"
    }
  }
}
```

Other CLI modules (Codex, Aider, Goose, Amazon Q, Gemini, Copilot, Cursor CLI) can be swapped in by changing `coder_module.source`. See the [module list](docs/ephemeral-coder-workspace-integration.md#coder-registry-modules-extensible-agent-module-system). When `coder_module` is absent, the Provisioner falls back to a default mapping based on the `cli` field.

### Role-specific permissions

| Role | `permission_mode` | Rationale |
|------|-------------------|-----------|
| `forge` | `acceptEdits` | Builds code, needs write access |
| `sentinel` | `plan` | Reviews code, should not auto-edit |
| `nexus` | `plan` | Orchestrates, should not auto-edit |
| `vessel` | `acceptEdits` | Merges PRs, needs write access |
| `lore` | `acceptEdits` | Writes documentation, needs write access |

---

## AI Gateway & Model Routing

OpenFlows routes LLM calls through a layered fallback:

1. **Coder AI Gateway (primary, Coder mode)** — when `USE_AI_GATEWAY=true`, Anthropic calls go to `${workspace_access_url}/api/v2/aibridge/anthropic` and authenticate via the Coder session token. No `ANTHROPIC_API_KEY` is injected into the workspace. Provides built-in audit logging, token tracking, and cost management.
2. **LiteLLM proxy (fallback + Local mode)** — per-agent model routing via `routing_key` dispatch. Used in Local mode and for providers the AI Gateway doesn't yet proxy. See [Proxy Configuration](#proxy-configuration-litellm-routing) below.
3. **Direct API access** — when no proxy is configured, agents use provider keys directly (Local mode default).

---

## Proxy Configuration (LiteLLM Routing)

Each agent has different workload requirements. Instead of routing all agents through the same expensive model, OpenFlows supports per-agent model routing via a **LiteLLM proxy**. In Coder mode this is the fallback path; in Local mode it is the primary.

### Default Model Assignments (LiteLLM fallback aliases)

| Alias | Model | Used by |
|-------|-------|---------|
| `openflows-forge` | `anthropic/claude-sonnet-4-5` | FORGE — primary coding agent, needs top-tier reasoning |
| `openflows-nexus` | `anthropic/claude-sonnet-4-5` | NEXUS — orchestrator, needs reliable decision-making |
| `openflows-sentinel` | `gemini/gemini-2.5-pro` | SENTINEL — code review, strong reasoning at lower cost |
| `openflows-vessel` | `groq/llama-3.3-70b-versatile` | VESSEL — CI/CD scripting, fast and cheap |
| `openflows-lore` | `openai/gpt-4o-mini` | LORE — documentation, lightweight task |

### How It Works

1. Each agent is spawned with its own `routing_key` (e.g., `forge-key`, `sentinel-key`)
2. A LiteLLM proxy receives all requests and dispatches based on the routing key
3. The proxy maps each routing key to the correct backend model via `litellm_config.yaml`
4. Fallback is configured — any provider failure falls back to `anthropic/claude-sonnet-4-5`

### Self-Hosted LiteLLM (Docker Compose)

```bash
# Start the proxy, Redis, and agent-team (Coder mode adds Coder + Postgres)
docker compose --profile coder up

# Or in Local mode — just the proxy + Redis for agent coordination
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

The registry at [`orchestration/agent/registry.json`](orchestration/agent/registry.json) is the **single source of truth** for team membership, worker scaling, per-agent routing, and workspace provider selection.

#### Structure

```json
{
  "default_cli": "claude",
  "team": [
    {
      "id": "forge",
      "cli": "claude",
      "active": true,
      "instances": 2,
      "model_backend": "anthropic/claude-haiku-4-5-20251001",
      "routing_key": "forge-key",
      "github_token_env": "AGENT_FORGE_GITHUB_TOKEN",
      "workspace_provider": "coder",
      "coder_module": {
        "source": "registry.coder.com/coder/claude-code/coder",
        "version": "5.2.0",
        "params": {
          "enable_ai_gateway": true,
          "permission_mode": "acceptEdits",
          "workdir": "/home/coder/workspace"
        }
      }
    }
  ]
}
```

#### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Agent name. Used in logs, worktree names (`forge-1`, `forge-2`), and branch names. |
| `cli` | string | CLI tool to spawn — `claude`, `codex`, `aider`, `goose`, etc. In Coder mode this selects the default Registry module. |
| `active` | bool | When `false`, the agent is excluded from orchestration entirely. (LORE ships disabled by default.) |
| `instances` | int | Number of parallel worker slots. FORGE uses this directly (`forge-1`, `forge-2`, ...). Other agents with `instances > 1` get numbered slots. Ignored in Coder mode (one workspace per ticket per role). |
| `model_backend` | string | Model identifier for the LLM client. In Coder mode with AI Gateway, routes through the gateway; otherwise routes through LiteLLM alias or direct provider path. |
| `routing_key` | string | LiteLLM proxy routing key (fallback). When `PROXY_URL`/`LITELLM_PROXY_URL` is set, dispatches to the correct backend model. |
| `github_token_env` | string | Env var holding this agent's GitHub PAT. Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN`. |
| `allowed_domains` | array or null | Network domains this agent can access. Falls back to the registry-level `allowed_domains`. Use `["*"]` for unrestricted access. |
| `workspace_provider` | `"coder"` or `"local"` | Where the agent runs. `"coder"` → ephemeral Coder workspace; `"local"` → local git worktree. Defaults to `"coder"` when `CODER_URL` is set, else `"local"`. |
| `coder_module` | object | (Coder mode) Coder Registry Terraform module config. `source` = module path, `version` = module version, `params` = module parameters (`enable_ai_gateway`, `permission_mode`, `workdir`). When absent, falls back to a default mapping based on `cli`. |

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

### Coder Mode (Docker Compose — recommended)

```bash
docker compose --profile coder up
```

This mode:
- Brings up Coder + PostgreSQL + LiteLLM + Redis + OpenFlows
- Runs the `CoderBootstrapper` on startup (admin user, templates, API token)
- Provisions ephemeral Coder workspaces per agent, AI Gateway enabled
- Connects to GitHub API, creates PRs, polls CI, merges green PRs, tears workspaces down on merge

### Local Mode (standalone binary)

```bash
# Via cargo
cargo run --bin openflows

# Or directly after build
./target/release/openflows
```

This mode:
- Connects to GitHub API to fetch issues
- Spawns the CLI backend in local git worktrees
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

In **Local mode**, install the CLI backend or set the path:

```env
CLAUDE_PATH=/path/to/claude
CODEX_PATH=/path/to/codex
```

In **Coder mode** this error won't appear on the host — the CLI is installed inside each workspace by the Coder Registry module. If the workspace's module download is slow, set `HOST_CLAUDE_BINARY` / `HOST_CODEX_BINARY` in `.env` to bind-mount a host-side binary.

See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) for detailed setup.

### "Connection refused" (Redis)

In **Coder mode**, Redis is part of the Compose stack — ensure it's healthy (`docker compose ps redis`). In **Local mode**, start Redis:

```bash
docker run -d -p 6379:6379 redis
```

Or remove `REDIS_URL` to use in-memory store.

### "Coder server unreachable" / agent workspaces stuck in provisioning

- Verify the Coder service is healthy: `docker compose --profile coder ps coder`
- Check `CODER_URL` in `.env` points at the Coder server
- Ensure the Docker socket is mounted (the Coder provisioner needs it) — see `docker-compose.yml`
- `CoderBootstrapper` logs should show "templates pushed" on startup

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
