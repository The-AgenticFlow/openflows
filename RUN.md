# Running OpenFlows

> Official site: [openflows.dev](https://openflows.dev)

This guide covers configuration and execution of OpenFlows after building. See [BUILD.md](BUILD.md) for build instructions and [INSTALL.md](INSTALL.md) for first-time installation.

## Table of Contents

- [Quick Start](#quick-start)
- [Environment Configuration](#environment-configuration)
- [Execution Modes](#execution-modes)
- [Configuration Options](#configuration-options)
- [Directory Structure](#directory-structure)
- [Running Examples](#running-examples)
- [Troubleshooting](#troubleshooting)
- [Next Steps](#next-steps)

## Quick Start

### Coder mode (recommended) — full stack via Docker Compose

```bash
# 1. Configure environment
cp .env.example .env
# Edit .env — at minimum GITHUB_REPOSITORY, GITHUB_PERSONAL_ACCESS_TOKEN
# For Coder mode, set CODER_URL and USE_AI_GATEWAY=true (defaults are sensible in docker-compose.yml)

# 2. Bring up the full stack (Coder + Postgres + LiteLLM + Redis + OpenFlows)
docker compose --profile coder up
```

### Local mode — standalone binary

```bash
# 1. Configure environment
cp .env.example .env
# Edit .env with your API keys

# 2. Run smoke test (no API keys needed)
cargo run --bin demo

# 3. Run orchestration
cargo run --bin openflows
```

## Environment Configuration

### Required Environment Variables

| Variable | Description |
|----------|-------------|
| `GITHUB_REPOSITORY` | Target repository in `owner/repo` format |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | GitHub PAT with `repo` scope |
| `DEFAULT_CLI` | CLI backend: `codex` or `claude` (ignored in Coder mode — module selection comes from `registry.json`) |
| Provider key | One of: `ANTHROPIC_API_KEY`, `FIREWORKS_API_KEY`, `OPENAI_API_KEY` (Local mode only — in Coder mode these live in the gateway/proxy config) |

### Setup

1. **Copy the example file:**
   ```bash
   cp .env.example .env
   ```

2. **Edit `.env` with your credentials:**
   ```bash
   # Required for all modes
   GITHUB_REPOSITORY=your-org/your-repo
   GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxx

   # Coder mode (Docker Compose) — set these to enable ephemeral Coder workspaces
   CODER_URL=http://localhost:7080
   USE_AI_GATEWAY=true

   # Local mode only — provider key depends on mode (see .env.example)
   DEFAULT_CLI=codex
   FIREWORKS_API_KEY=your-key
   OPENAI_API_KEY=your-key
   ```

3. **Verify the CLI backend is installed (Local mode only):**
   ```bash
   # If using Codex
   which codex
   codex --version

   # If using Claude
   which claude
   claude --version
   ```
   In **Coder mode**, the CLI is installed inside each ephemeral workspace by the [Coder Registry module](https://registry.coder.com) — no host-side install required.

## Execution Modes

### Coder Mode (Recommended)

Run the full stack with ephemeral, governed Coder workspaces:

```bash
docker compose --profile coder up
```

This mode:
- Brings up Coder + PostgreSQL + LiteLLM + Redis + OpenFlows
- Runs `CoderBootstrapper` on startup (admin user, templates, API token)
- Provisions an ephemeral Coder workspace per agent per ticket, with AI Gateway routing LLM calls
- Connects to GitHub API, creates PRs, polls CI, merges green PRs, tears workspaces down on merge

To run Coder mode from a standalone binary pointing at your own Coder server, set `CODER_URL` in `.env` and run:

```bash
cargo run --bin openflows
```

OpenFlows auto-detects `CODER_URL` and switches agents to Coder transport.

### Local Mode

Run the full orchestration with real GitHub API and the CLI backend in local git worktrees:

```bash
# Via cargo
cargo run --bin openflows

# Or directly after build
./target/release/openflows
```

This mode:
- Connects to GitHub API to fetch issues
- Spawns the CLI backend in local git worktrees (no Coder, no Docker)
- Creates real pull requests
- Polls CI status and merges PRs

### Mocked Demo

Pre-configured demonstration with fake data (no API keys required):

```bash
cargo run --bin demo
```

Uses in-memory implementations without external API calls.

### Dashboard

Live worker status monitor:

```bash
cargo run --bin openflows-dashboard
```

### Setup Wizard

Interactive TUI configuration (detects Coder mode):

```bash
cargo run --bin openflows-setup
```

### Diagnostics

Check your environment for common issues:

```bash
cargo run --bin openflows-doctor
```

## Configuration Options

### LLM Provider Configuration

**Coder AI Gateway (primary — Coder mode):**
```env
USE_AI_GATEWAY=true
# Anthropic calls route through the gateway; the Coder session token authenticates.
# Provider keys live in the Coder control plane, not in .env.
```

**LiteLLM proxy (fallback + Local mode):**
```env
PROXY_URL=http://localhost:4000/v1      # Local mode
LITELLM_PROXY_URL=http://proxy:4000     # inside Coder workspaces (Docker service name)
```

Route LLM requests through LiteLLM for:
- Per-agent model routing via `routing_key` dispatch
- Cost optimization
- Rate limit management
- Providers the AI Gateway doesn't yet proxy

**Direct mode (Local, no proxy):**
```env
ANTHROPIC_API_KEY=sk-ant-xxxxx
FIREWORKS_API_KEY=xxxxx
OPENAI_API_KEY=sk-xxxxx
```

Set individual provider keys. The system falls back through providers on failure.

### Redis Backend (SharedStore — agent coordination)

```env
REDIS_URL=redis://localhost:6379
```

In **Coder mode**, Redis is part of the Compose stack and required for typed agent coordination via SharedStore. In **Local mode**, without Redis the system uses an in-memory store (state lost on restart).

### Coder Mode Configuration

| Variable | Description |
|----------|-------------|
| `CODER_URL` | Coder server URL — when set, agents switch to Coder transport automatically |
| `CODER_ADMIN_PASSWORD` | Password for the admin user created by `CoderBootstrapper` (default: `Op3nFl0ws!`) |
| `USE_AI_GATEWAY` | `true` routes Anthropic calls through Coder's AI Gateway; `false` falls back to LiteLLM (default: `true` in Coder mode) |
| `LITELLM_PROXY_URL` | LiteLLM fallback URL used inside Coder workspaces for non-Anthropic providers |
| `HOST_CLAUDE_BINARY` | Path to host-side Claude CLI — bind-mounted read-only into each workspace so the module can skip its startup download |
| `HOST_CODEX_BINARY` | Same, for the Codex CLI |

### Logging

Control log verbosity:

```env
RUST_LOG=info,agent_team=debug,pocketflow_core=debug
```

Levels: `error`, `warn`, `info`, `debug`, `trace`

## Directory Structure

### Local mode

OpenFlows creates workspaces in `~/.agentflow/`:

```
~/.agentflow/
└── workspaces/
    └── your-repo/           # Cloned target repository
        ├── .git/
        ├── worktrees/       # Agent worktrees
        │   ├── forge-1/
        │   └── forge-2/
        └── orchestration/   # Status files, logs
```

### Coder mode

Agents run inside ephemeral Coder workspaces (managed by the Coder server). The host `~/.agentflow/` workspace root is not used; workspace state is governed by Coder + the persistent volume. See [`docs/ephemeral-coder-workspace-integration.md`](docs/ephemeral-coder-workspace-integration.md).

## Running Examples

### Basic run (Local mode)

```bash
# Ensure .env is configured
cargo run --bin openflows
```

Expected output:
```
INFO Starting REAL End-to-End Orchestration...
INFO Target repository workspace ready: /home/user/.agentflow/workspaces/my-repo
INFO Running orchestration loop for repository: owner/repo
```

### Basic run (Coder mode)

```bash
docker compose --profile coder up
# CoderBootstrapper provisions admin + templates on startup
# Agents run inside ephemeral Coder workspaces with AI Gateway
```

### With custom workspace root (Local mode)

```env
AGENTFLOW_WORKSPACE_ROOT=/custom/path/workspaces
```

### With Redis persistence

```bash
# Coder mode — Redis is part of the Compose stack
docker compose --profile coder up

# Local mode — start Redis manually
docker run -d -p 6379:6379 redis
REDIS_URL=redis://localhost:6379 cargo run --bin openflows
```

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

### "claude: command not found" or "codex: command not found" (Local mode)

Install the CLI backend or set the path:
```env
CLAUDE_PATH=/path/to/claude
CODEX_PATH=/path/to/codex
```

In **Coder mode**, if the workspace module download is slow, set `HOST_CLAUDE_BINARY`/`HOST_CODEX_BINARY` to bind-mount a host-side binary.

See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) and [docs/cli-backend-configuration.md](docs/cli-backend-configuration.md) for detailed setup.

### "Connection refused" (Redis)

In **Coder mode**, ensure the Redis service is healthy (`docker compose --profile coder ps redis`). In **Local mode**, start Redis:
```bash
docker run -d -p 6379:6379 redis
```

Or remove `REDIS_URL` to use in-memory store.

### "Coder server unreachable" / workspaces stuck in provisioning (Coder mode)

- Verify `CODER_URL` points at the Coder server
- Check the Coder service health: `docker compose --profile coder ps coder`
- Ensure the Docker socket is mounted for the Coder provisioner (see `docker-compose.yml`)
- `CoderBootstrapper` logs should show templates pushed on startup

### API rate limit exceeded
- Use LiteLLM proxy (or AI Gateway in Coder mode) for rate limit management
- Reduce concurrent workers in `orchestration/agent/registry.json` (Local mode only — Coder mode uses one workspace per ticket)
- Add fallback providers in `LLM_FALLBACK`

## Next Steps

- [TUTORIAL.md](TUTORIAL.md) — Step-by-step walkthrough
- [docs/demo.md](docs/demo.md) — Live flow demonstration
- [docs/ephemeral-coder-workspace-integration.md](docs/ephemeral-coder-workspace-integration.md) — Coder integration architecture
- [CONTRIBUTING.md](CONTRIBUTING.md) — Development guidelines
