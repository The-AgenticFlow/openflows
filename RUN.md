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

```bash
# 1. Configure environment
cp .env.example .env
# Edit .env with your API keys

# 2. Run smoke test (no API keys needed)
cargo run --bin demo

# 3. Run orchestration
cargo run --bin agentflow
```

## Environment Configuration

### Required Environment Variables

| Variable | Description |
|----------|-------------|
| `GITHUB_REPOSITORY` | Target repository in `owner/repo` format |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | GitHub PAT with `repo` scope |
| `DEFAULT_CLI` | CLI backend: `codex` or `claude` |
| Provider key | One of: `ANTHROPIC_API_KEY`, `FIREWORKS_API_KEY`, `OPENAI_API_KEY` |

### Setup

1. **Copy the example file:**
   ```bash
   cp .env.example .env
   ```

2. **Edit `.env` with your credentials:**
   ```bash
   # Required
   GITHUB_REPOSITORY=your-org/your-repo
   GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxx
   DEFAULT_CLI=codex

   # Provider key (depends on mode — see .env.example)
   FIREWORKS_API_KEY=your-key
   OPENAI_API_KEY=your-key
   ```

3. **Verify the CLI backend is installed:**
   ```bash
   # If using Codex
   which codex
   codex --version

   # If using Claude
   which claude
   claude --version
   ```

## Execution Modes

### Production Mode (Recommended)

Run the full orchestration with real GitHub API and CLI backend:

```bash
# Via cargo
cargo run --bin agentflow

# Or directly after build
./target/release/agentflow
```

This mode:
- Connects to GitHub API to fetch issues
- Spawns the CLI backend for code generation
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
cargo run --bin agentflow-dashboard
```

### Setup Wizard

Interactive TUI configuration:

```bash
cargo run --bin agentflow-setup
```

### Diagnostics

Check your environment for common issues:

```bash
cargo run --bin agentflow-doctor
```

## Configuration Options

### LLM Provider Configuration

**Proxy Mode (Recommended):**
```env
PROXY_URL=http://localhost:4000/v1
PROXY_API_KEY=your-proxy-key
```

Route all LLM requests through a LiteLLM proxy for:
- Per-agent model routing
- Cost optimization
- Rate limit management

**Direct Mode:**
```env
ANTHROPIC_API_KEY=sk-ant-xxxxx
FIREWORKS_API_KEY=xxxxx
OPENAI_API_KEY=sk-xxxxx
```

Set individual provider keys. The system falls back through providers on failure.

### Redis Backend (Optional)

For persistent state across runs:

```env
REDIS_URL=redis://localhost:6379
```

Without Redis, the system uses an in-memory store (state lost on restart).

### Logging

Control log verbosity:

```env
RUST_LOG=info,agent_team=debug,pocketflow_core=debug
```

Levels: `error`, `warn`, `info`, `debug`, `trace`

## Directory Structure

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

## Running Examples

### Basic Run

```bash
# Ensure .env is configured
cargo run --bin agentflow
```

Expected output:
```
INFO Starting REAL End-to-End Orchestration...
INFO Target repository workspace ready: /home/user/.agentflow/workspaces/my-repo
INFO Running orchestration loop for repository: owner/repo
```

### With Custom Workspace Root

```env
AGENTFLOW_WORKSPACE_ROOT=/custom/path/workspaces
```

### With Redis Persistence

```bash
# Start Redis
docker run -d -p 6379:6379 redis

# Run with Redis backend
REDIS_URL=redis://localhost:6379 cargo run --bin agentflow
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

### "claude: command not found" or "codex: command not found"

Install the CLI backend or set the path:
```env
CLAUDE_PATH=/path/to/claude
CODEX_PATH=/path/to/codex
```

See [docs/setup-claude-cli.md](docs/setup-claude-cli.md) and [docs/cli-backend-configuration.md](docs/cli-backend-configuration.md) for detailed setup.

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

## Next Steps

- [TUTORIAL.md](TUTORIAL.md) — Step-by-step walkthrough
- [docs/demo.md](docs/demo.md) — Live flow demonstration
- [CONTRIBUTING.md](CONTRIBUTING.md) — Development guidelines
