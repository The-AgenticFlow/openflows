# AgentFlow - Autonomous AI Development Team

An autonomous software development team composed of AI agents working in a unified Rust/Tokio flow. The team can take GitHub issues and turn them into working code with pull requests - all autonomously.

## Quick Start

```bash
# 1. Clone and setup
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
cp .env.example .env
# Edit .env with your API keys

# 2. Start the local proxy (required when gateway doesn't support Anthropic format)
source .env && ./scripts/start_proxy.sh &
# Or if your provider supports Anthropic directly, skip this step

# 3. Verify setup (optional but recommended)
./scripts/check_setup.sh

# 4. Run the orchestration
cargo run --bin real_test
```

## Getting Started

### 📖 Complete Tutorial
**NEW: [TUTORIAL.md](TUTORIAL.md)** - Detailed walkthrough with:
- ✅ Step-by-step setup from zero
- ✅ Expected logs and outputs at each step
- ✅ File structure and locations explained
- ✅ Troubleshooting common issues
- ✅ How to inspect generated code and PRs

### 🚀 Live Flow Walkthrough
**[docs/demo.md](docs/demo.md)** - Step-by-step walkthrough of a live orchestration run with:
- What each log line means as NEXUS discovers issues and assigns work
- How the FORGE-SENTINEL pair communicates through the shared directory
- Where to find generated plans, evaluations, and code changes on disk
- Troubleshooting table for common failures

## The Team

| Agent | Role | Description |
|-------|------|-------------|
| **NEXUS** | Orchestrator | Scrum Master & Tech Lead. Assigns tickets, approves dangerous commands. |
| **FORGE** | Builder | Senior Engineer. Writes code, tests, opens PRs via Claude Code. |
| **SENTINEL** | Reviewer | Security auditor. Reviews PRs, ensures all logic is tested. |
| **VESSEL** | DevOps | Deployment expert. Manages CI/CD and rollbacks. |
| **LORE** | Writer | Documenter. Writes ADRs, maintains project history. |

## Architecture

```
AgentFlow/
|-- orchestration/agent/agents/           # Agent personas (nexus.agent.md, forge.agent.md)
|-- crates/
|   |-- agent-nexus/         # Orchestrator node
|   |-- agent-forge/         # Builder node (spawns Claude Code)
|   |-- agent-client/        # LLM client + MCP integration
|   |-- pair-harness/        # Worktree management, process spawning
|   |-- pocketflow-core/     # Flow engine, shared store, routing
|
|-- binary/src/bin/
    |-- real_test.rs         # Live orchestration entry point
    |-- demo.rs              # Mocked demonstration
```

## How It Works

```
GitHub Issues
     |
     v
  +-------+     +-------+     +-------+
  | NEXUS |---->| FORGE |---->|  PR   |
  +-------+     +-------+     +-------+
     |               |
     |               v
     |          Claude Code
     |               |
     v               v
  Routing        STATUS.json
  Logic
```

1. **NEXUS** discovers open GitHub issues and assigns them to available workers
2. **FORGE** spawns Claude Code to implement the solution in an isolated worktree
3. Claude Code writes code, tests, and creates `STATUS.json` with the result
4. **NEXUS** reviews results and assigns more work or handles blocked workers

## Key Files

| File | Purpose |
|------|---------|
| [`orchestration/agent/agents/nexus.agent.md`](orchestration/agent/agents/nexus.agent.md) | Orchestrator persona and workflow |
| [`orchestration/agent/agents/forge.agent.md`](orchestration/agent/agents/forge.agent.md) | Builder persona and instructions |
| [`orchestration/agent/registry.json`](orchestration/agent/registry.json) | Worker slot definitions |
| [`binary/src/bin/real_test.rs`](binary/src/bin/real_test.rs) | Main entry point |
| [`crates/agent-forge/src/lib.rs`](crates/agent-forge/src/lib.rs) | Forge node implementation |

## Documentation

- **[TUTORIAL.md](TUTORIAL.md)** - Complete tutorial with logs, file structure, and troubleshooting
- **[docs/demo.md](docs/demo.md)** - Live flow walkthrough: logs, file locations, and troubleshooting
- **[docs/setup-claude-cli.md](docs/setup-claude-cli.md)** - Claude CLI setup and troubleshooting
- **[CONTRIBUTING.md](CONTRIBUTING.md)** - Development guidelines
- **[docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md)** - Architecture details

## Per-Agent LLM Routing (LiteLLM Proxy)

Each agent has different workload requirements. Instead of routing all agents through the same expensive model, AgentFlow supports per-agent model routing via a **LiteLLM proxy** — each agent uses the most cost-effective model for its task.

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

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `PROXY_URL` | Optional | LiteLLM proxy URL. When set, agents route through the proxy. When unset, agents use direct API access. |
| `PROXY_API_KEY` | Optional | API key for a **hosted** LiteLLM proxy. When set, `ANTHROPIC_API_KEY` is set to this value for auth. When unset, the routing key is used as `ANTHROPIC_API_KEY` (for self-hosted LiteLLM). |
| `ANTHROPIC_API_KEY` | Required* | Anthropic API key (used by FORGE, NEXUS, and as fallback) |
| `GEMINI_API_KEY` | Optional | Google Gemini API key (used by SENTINEL via proxy) |
| `OPENAI_API_KEY` | Optional | OpenAI API key (used by LORE via proxy) |
| `GROQ_API_KEY` | Optional | Groq API key (used by VESSEL via proxy, free tier available) |
| `GATEWAY_URL` | Optional | Remote OpenAI-compatible gateway URL. Used by the local Anthropic proxy to forward requests. Required only when the gateway doesn't support Anthropic protocol. |
| `GATEWAY_API_KEY` | Optional | API key for the remote gateway. Falls back to `PROXY_API_KEY` if unset. |

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

When using a hosted proxy, set `PROXY_API_KEY` to your proxy authentication key. The provider API keys (`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, etc.) are configured on the proxy side, not in the AgentFlow `.env`.

### OpenAI-Only Gateways (Local Anthropic Proxy)

If your LLM gateway only supports the OpenAI Chat Completions format (`/v1/chat/completions`), Claude CLI will fail because it speaks Anthropic Messages API (`/v1/messages`). AgentFlow includes a local protocol translator:

```bash
# Terminal 1: Start the proxy (reads .env automatically)
./scripts/start_proxy.sh

# Terminal 2: Run orchestration
cargo run --bin real_test
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

## Requirements

- Rust 1.70+
- Node.js 18+ (for GitHub MCP server)
- **Claude Code CLI** - [Setup Guide](docs/setup-claude-cli.md)
- API keys: `ANTHROPIC_API_KEY` (required), `GITHUB_PERSONAL_ACCESS_TOKEN` (required), plus optional provider keys for proxy routing (`GEMINI_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`)

## License

MIT
