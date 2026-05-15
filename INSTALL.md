# Installation Guide

> Complete guide for installing, configuring, and running OpenFlows.

## Requirements

- **Rust 1.70+**
- **Node.js 18+** (for GitHub MCP server)
- **Claude Code CLI** - [Setup Guide](docs/setup-claude-cli.md)
- **API Keys:**
  - `ANTHROPIC_API_KEY` (required)
  - `GITHUB_PERSONAL_ACCESS_TOKEN` (required)
  - Optional provider keys: `GEMINI_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`

## Quick Start

### Option 1: One-Line Install (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash
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

### Option 4: npm (Node.js Package Manager)

Install globally via npm for easy updates and cross-platform support:

```bash
# Install the package globally (@the-agenticflow scope)
npm install -g @the-agenticflow/openflows

# Verify installation
openflows --version

# Run the interactive setup wizard
openflows-setup

# Start the autonomous orchestration
openflows

# Monitor with the dashboard (optional, separate terminal)
openflows-dashboard

# Diagnose issues
openflows-doctor
```

**Updating via npm:**

```bash
npm update -g @the-agenticflow/openflows
```

**Uninstalling:**

```bash
npm uninstall -g @the-agenticflow/openflows
```

**Note:** The npm package includes platform-specific native binaries as optional dependencies. The correct binary for your platform (Linux x86_64/aarch64, macOS x86_64/Apple Silicon) is automatically downloaded during installation via the `postinstall` script.

### Option 5: Build from Source

```bash
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
make release          # or: cargo build --release -p openflows
make install          # installs to ~/.local/bin
openflows-setup       # Guided setup wizard
openflows             # Start orchestration
```

### Option 6: Cargo Install

```bash
cargo install openflows
openflows-setup
openflows
```

## After Installation

### Standard Commands (All Install Methods)

1. **Configure** — Run `openflows-setup` (or `agentflow-setup`) for the guided TUI wizard
2. **Verify** — Run `openflows-doctor` (or `agentflow-doctor`) to check your environment
3. **Run** — Run `openflows` (or `agentflow`) to start the autonomous team
4. **Monitor** — Run `openflows-dashboard` (or `agentflow-dashboard`) for live worker status

### npm-Specific Workflow

If you installed via npm, you can also use npx without global installation:

```bash
# Run setup wizard without installing (uses @the-agenticflow scope)
npx @the-agenticflow/openflows-setup

# Start orchestration directly
npx @the-agenticflow/openflows

# Check status
npx @the-agenticflow/openflows-doctor
```

**Using npx with specific versions:**

```bash
# Run a specific version
npx @the-agenticflow/openflows@0.1.2

# Run the latest version
npx @the-agenticflow/openflows@latest
```

**Package Scripts (if integrating into a Node.js project):**

Add to your `package.json`:

```json
{
  "scripts": {
    "agent:setup": "openflows-setup",
    "agent:start": "openflows",
    "agent:doctor": "openflows-doctor",
    "agent:dashboard": "openflows-dashboard"
  },
  "devDependencies": {
    "@the-agenticflow/openflows": "^0.1.2"
  }
}
```

Or install as a dev dependency:

```bash
npm install --save-dev @the-agenticflow/openflows
```

Then run:

```bash
npm run agent:setup      # Configure the system
npm run agent:start      # Start the orchestration
npm run agent:doctor     # Check environment
npm run agent:dashboard  # Monitor workers
```

**Programmatic API (Node.js):**

```javascript
const { spawn } = require('child_process');
const path = require('path');

// Run openflows commands programmatically
const openflows = spawn('openflows', ['--version'], {
  stdio: 'inherit',
  env: {
    ...process.env,
    ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY,
    GITHUB_PERSONAL_ACCESS_TOKEN: process.env.GITHUB_PERSONAL_ACCESS_TOKEN
  }
});

openflows.on('exit', (code) => {
  console.log(`OpenFlows exited with code ${code}`);
});
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
| `ANTHROPIC_API_KEY` | Anthropic API key (or use proxy mode) |

### All Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GITHUB_REPOSITORY` | Yes | Target repository (`owner/repo`) |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | Yes | GitHub PAT with `repo` scope |
| `ANTHROPIC_API_KEY` | Yes* | Anthropic API key (required in direct mode) |
| `PROXY_URL` | No | LiteLLM proxy URL for routing |
| `PROXY_API_KEY` | No | API key for hosted LiteLLM proxy |
| `GATEWAY_URL` | No | OpenAI-compatible gateway URL |
| `GATEWAY_API_KEY` | No | Gateway API key |
| `OPENAI_API_KEY` | No | OpenAI API key |
| `GEMINI_API_KEY` | No | Google Gemini API key |
| `GROQ_API_KEY` | No | Groq API key |
| `FIREWORKS_API_KEY` | No | Fireworks AI API key |
| `CLAUDE_PATH` | No | Path to Claude CLI binary (default: `claude`) |
| `REDIS_URL` | No | Redis URL for persistent state |
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
cargo run --bin agentflow
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
| `cli` | string | CLI tool to spawn. Currently only `"claude"` (Claude Code) is supported. |
| `active` | bool | When `false`, the agent is excluded from orchestration entirely. |
| `instances` | int | Number of parallel worker slots. FORGE uses this directly (`forge-1`, `forge-2`, ...). Other agents with `instances > 1` get numbered slots (`vessel-1`, `vessel-2`). Agents with `instances == 1` use their bare ID (`nexus`, `sentinel`). |
| `model_backend` | string | Model identifier passed to the LLM client. Can be a direct provider path (`anthropic/claude-sonnet-4-5`) or a gateway path (`accounts/fireworks/models/glm-5`). |
| `routing_key` | string | LiteLLM proxy routing key. When `PROXY_URL` is set, this key is used to route requests to the correct backend model. When unset, the agent falls back to direct API access. |
| `github_token_env` | string | Environment variable name that holds the GitHub PAT for this agent. Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN` if not set. Enables per-agent token rotation and scoping. |

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
cargo run --bin agentflow

# Or directly after build
./target/release/agentflow
```

This mode:
- Connects to GitHub API to fetch issues
- Spawns Claude CLI for code generation
- Creates real pull requests
- Polls CI status and merges PRs

### Development Mode

Dry-run with local node implementations:

```bash
cargo run --bin agentflow-demo
```

Uses in-memory implementations without external API calls.

### Mocked Demo

Pre-configured demonstration with fake data:

```bash
cargo run --bin demo
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

### "claude: command not found"

Install Claude CLI or set the path:

```env
CLAUDE_PATH=/path/to/claude
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

---

## Additional Resources

- **[TUTORIAL.md](TUTORIAL.md)** - Complete tutorial with logs, file structure, and troubleshooting
- **[RUN.md](RUN.md)** - Running and configuration guide
- **[docs/demo.md](docs/demo.md)** - Live flow walkthrough
- **[docs/setup-claude-cli.md](docs/setup-claude-cli.md)** - Claude CLI setup
- **[CONTRIBUTING.md](CONTRIBUTING.md)** - Development guidelines