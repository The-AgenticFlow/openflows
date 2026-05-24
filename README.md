# AgentFlow - Autonomous AI Development Team

> Humans steer. Agents execute.
>
> 🌐 Official site: [openflows.dev](https://openflows.dev)

An autonomous software development team composed of AI agents working in a unified Rust/Tokio flow. The team can take GitHub issues and turn them into working code with pull requests - all autonomously.

![AgentFlow Architecture](image.png)

## Quick Start

### Option A: Codex + Fireworks (Recommended — Simplest Setup)

No proxy required! Codex CLI is OpenAI-compatible and connects directly to any OpenAI-compatible provider (Fireworks, OpenAI, DeepSeek, etc.).

> **Tip:** You can also use Codex with a direct OpenAI API key — just set `OPENAI_API_KEY` and leave `OPENAI_BASE_URL` unset. No Fireworks key needed.

```bash
# 1. Clone and setup
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
cp .env.example .env

# 2. Install and authenticate Codex CLI
npm install -g @openai/codex
echo "your-fireworks-key" | codex login --with-api-key
codex login status   # verify it worked

# 3. Edit .env — uncomment MODE A and set:
#    DEFAULT_CLI=codex
#    FIREWORKS_API_KEY=your-fireworks-key
#    CODEX_PATH=codex     # or absolute path from: which codex
#    OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1
#    OPENAI_API_KEY=your-fireworks-key   # same as FIREWORKS_API_KEY

# 4. Run
cargo run --bin agentflow
```

### Option B: Claude Code + Direct Anthropic Key

If you have a direct Anthropic API key, Claude Code connects natively — no proxy needed.

```bash
# 1. Clone and setup
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
cp .env.example .env

# 2. Install and authenticate Claude Code CLI
npm install -g @anthropic-ai/claude-code
claude login

# 3. Edit .env — uncomment MODE B and set:
#    DEFAULT_CLI=claude
#    ANTHROPIC_API_KEY=sk-ant-...
#    ANTHROPIC_MODEL=claude-haiku-4-5-20251001

# 4. Run
cargo run --bin agentflow
```

### Option C: Claude Code + Third-Party Provider (Requires Proxy)

Claude Code uses the Anthropic Messages API format, which is **NOT OpenAI-compatible**.
If you want to route Claude Code through a third-party provider (Fireworks, etc.),
you MUST run the local protocol translator proxy (`anthropic-mock`).

```bash
# 1. Clone and setup
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
cp .env.example .env

# 2. Install Claude Code CLI (same as Option B)
npm install -g @anthropic-ai/claude-code

# 3. Edit .env — uncomment MODE C and set:
#    DEFAULT_CLI=claude
#    PROXY_URL=http://localhost:8765/v1
#    PROXY_API_KEY=your-gateway-key
#    MODEL_MAP=claude-haiku-4-5-20251001=your-gateway-model,...
#    GATEWAY_URL=https://api.fireworks.ai/inference/v1/
#    GATEWAY_API_KEY=your-gateway-key

# 4. Start the proxy (MUST be running before orchestration)
cargo run -p anthropic-mock &
# Or: ./scripts/start_proxy.sh &

# 5. Run orchestration (in another terminal)
cargo run --bin agentflow
```

## Why Codex Doesn't Need a Proxy

| | Codex CLI | Claude Code CLI |
|---|---|---|
| **API Format** | OpenAI Chat Completions | Anthropic Messages |
| **Third-party providers** | Direct connection | Requires proxy |
| **Env vars** | `OPENAI_API_KEY` + `OPENAI_BASE_URL` | `ANTHROPIC_API_KEY` + `ANTHROPIC_BASE_URL` |
| **Fireworks** | Set `OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1` | Must run `anthropic-mock` proxy |

**Codex CLI speaks the OpenAI Chat Completions format** (`/v1/chat/completions`), which is the industry-standard interchange format. Nearly every third-party provider (Fireworks, DeepSeek, Together, Groq, etc.) supports this format natively. This means Codex can connect directly to any of them with just `OPENAI_BASE_URL` and `OPENAI_API_KEY`.

**Claude Code CLI speaks the Anthropic Messages format** (`/v1/messages`), which only Anthropic's own API supports natively. When you want to route Claude Code through a non-Anthropic provider, you need the `anthropic-mock` proxy running locally to translate Anthropic → OpenAI format in real-time.

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
| **FORGE** | Builder | Senior Engineer. Writes code, tests, opens PRs via Claude Code or Codex CLI. |
| **SENTINEL** | Reviewer | Security auditor. Reviews PRs, ensures all logic is tested. |
| **VESSEL** | DevOps | Deployment expert. Manages CI/CD and rollbacks. |
| **LORE** | Writer | Documenter. Writes ADRs, maintains project history. |

## Registry System

The registry at [`orchestration/agent/registry.json`](orchestration/agent/registry.json) is the **single source of truth** for team membership, worker scaling, and per-agent routing. NEXUS reloads it on every poll cycle, so changes take effect without restarting the orchestration.

### Structure

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

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Agent name. Used in logs, worktree names (`forge-1`, `forge-2`), and branch names. |
| `cli` | string | CLI tool to spawn. `"claude"` (Claude Code) or `"codex"` (Codex CLI). |
| `active` | bool | When `false`, the agent is excluded from orchestration entirely. |
| `instances` | int | Number of parallel worker slots. FORGE uses this directly (`forge-1`, `forge-2`, ...). Other agents with `instances > 1` get numbered slots (`vessel-1`, `vessel-2`). Agents with `instances == 1` use their bare ID (`nexus`, `sentinel`). |
| `model_backend` | string | Model identifier passed to the LLM client. Can be a direct provider path (`anthropic/claude-sonnet-4-5`) or a gateway path (`accounts/fireworks/models/glm-5`). |
| `routing_key` | string | LiteLLM proxy routing key. When `PROXY_URL` is set, this key is used to route requests to the correct backend model. When unset, the agent falls back to direct API access. |
| `github_token_env` | string | Environment variable name that holds the GitHub PAT for this agent. Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN` if not set. Enables per-agent token rotation and scoping. |

### Worker Slots

The registry generates worker slot names that appear throughout the system (worktrees, branches, logs, `STATUS.json`):

```json
{
  "team": [
    { "id": "nexus",    "instances": 1 },   // → slot: "nexus"
    { "id": "forge",    "instances": 2 },   // → slots: "forge-1", "forge-2"
    { "id": "sentinel", "instances": 1 },   // → slot: "sentinel"
    { "id": "vessel",   "instances": 1 },   // → slot: "vessel"
    { "id": "lore",     "instances": 1 }    // → slot: "lore"
  ]
}
```

### Common Operations

**Scale FORGE workers** — change `instances` on the forge entry:
```json
{ "id": "forge", "instances": 4 }  // → forge-1, forge-2, forge-3, forge-4
```

**Disable an agent** — set `active` to `false`:
```json
{ "id": "lore", "active": false }  // LORE will not be invoked
```

**Rotate a GitHub token** — update the env var referenced by `github_token_env`:
```bash
export AGENT_FORGE_GITHUB_TOKEN=ghp_new_token_here
```

**Switch a model backend** — update `model_backend`:
```json
{ "id": "forge", "model_backend": "anthropic/claude-sonnet-4-5" }
```

### Per-Agent GitHub Tokens

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

## Architecture

```
AgentFlow/
|-- orchestration/agent/agents/           # Agent personas (nexus.agent.md, forge.agent.md)
|-- crates/
|   |-- agent-nexus/         # Orchestrator node
|   |-- agent-forge/         # Builder node (spawns Claude Code or Codex CLI)
|   |-- agent-client/        # LLM client + MCP integration
|   |-- pair-harness/        # Worktree management, process spawning
|   |-- pocketflow-core/     # Flow engine, shared store, routing
|
|-- binary/src/bin/
    |-- real_test.rs          # Live orchestration entry point
    |-- demo.rs               # Mocked demonstration
```

## How It Works

```
                    GitHub Issues
                         |
                         v
                    +---------+
                    |  NEXUS  |  ← Orchestrator: discovers issues, assigns work
                    +---------+
                         |
              ACTION_WORK_ASSIGNED
                         |
                         v
              +--------------------+
              |   FORGE-SENTINEL   |  ← Builder + Reviewer pair
              |       PAIR         |
              +--------------------+
                |                |
                |  PLAN.md       |  CONTRACT.md
                |  CODE          |  segment-N-eval.md
                |  STATUS.json   |  final-review.md
                |                |
                v                v
              PR Opened     CI Checks
                         |
                         v
                    +---------+
                    | VESSEL  |  ← DevOps: polls CI, resolves conflicts, merges
                    +---------+
                         |
              ACTION_DEPLOYED
                         |
                         v
                    +---------+
                    |  LORE   |  ← Writer: ADRs, changelogs, documentation
                    +---------+
                         |
              ACTION_DOCS_COMPLETE
                         |
                         v
                    +---------+
                    |  NEXUS  |  ← Loop: assigns next ticket or halts
                    +---------+
```

### The Orchestration Cycle

1. **NEXUS** fetches open GitHub issues and assigns them to available FORGE workers
2. **FORGE** creates an isolated worktree, writes PLAN.md, then implements code via Claude Code or Codex CLI
3. **SENTINEL** reviews the plan (CONTRACT.md), evaluates each code segment, and performs final review
4. **FORGE** opens a PR once SENTINEL approves
5. **VESSEL** polls CI status, detects merge conflicts, attempts resolution, and squash-merges green PRs
6. **LORE** generates documentation: ADRs, changelogs, and project history updates
7. **NEXUS** loops back to assign the next ticket or halts when no work remains

### Shared State

All agents communicate through a **SharedStore** (in-memory or Redis):

| Key | Purpose |
|-----|---------|
| `tickets` | GitHub issues converted to internal work items |
| `worker_slots` | Available FORGE workers and their status |
| `pending_prs` | PRs awaiting CI completion |

### File Artifacts

Each FORGE-SENTINEL pair produces artifacts in two locations:

**Shared directory** (`~/.agentflow/workspaces/<repo>/orchestration/pairs/forge-<N>/shared/`):

| File | Written By | Purpose |
|------|-----------|---------|
| `TICKET.md` | NEXUS | GitHub issue details assigned to this pair |
| `TASK.md` | NEXUS | Task instructions and acceptance criteria |
| `PLAN.md` | FORGE | Implementation plan with segment breakdown |
| `CONTRACT.md` | SENTINEL | Plan review verdict (AGREED or CHANGES_REQUESTED) |
| `WORKLOG.md` | FORGE | Running log of segment implementation progress |
| `segment-N-eval.md` | SENTINEL | Evaluation result for segment N (APPROVED / CHANGES_REQUESTED) |
| `final-review.md` | SENTINEL | Final overall review verdict |
| `HANDOFF.md` | FORGE | Context reset request when context window is full |
| `STATUS.json` | FORGE | Terminal status: `PR_OPENED`, `BLOCKED`, or `FUEL_EXHAUSTED` |

**Worktree** (`~/.agentflow/workspaces/<repo>/worktrees/forge-<N>/`):

```
worktrees/forge-1/
├── src/                     # Code changes on isolated branch
├── tests/                   # Test files added/modified by FORGE
├── PLAN.md                  # Copy of implementation plan
├── WORKLOG.md               # Copy of progress log
├── CONTRACT.md              # Copy of SENTINEL-approved contract
├── segment-1-eval.md        # Copy of segment evaluation
├── final-review.md          # Copy of final review
└── STATUS.json              # Copy of completion status
```

The `STATUS.json` structure:

```json
{
  "status": "PR_OPENED",
  "ticket_id": "T-001",
  "pr_url": "https://github.com/owner/repo/pull/42",
  "pr_number": 42,
  "branch": "forge-1/T-001",
  "files_changed": 5,
  "segments_completed": [
    {"segment": 1, "status": "APPROVED", "eval_file": "segment-1-eval.md"},
    {"segment": 2, "status": "APPROVED", "eval_file": "segment-2-eval.md"}
  ],
  "test_results": {"passed": 12, "failed": 0, "skipped": 0},
  "sentinel_approved": true,
  "context_resets": 0
}
```

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
- **[docs/cli-backend-configuration.md](docs/cli-backend-configuration.md)** - CLI backend configuration (Claude & Codex)
- **[CONTRIBUTING.md](CONTRIBUTING.md)** - Development guidelines
- **[docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md)** - Architecture details

## CLI Backend Configuration

AgentFlow supports two CLI backends for agent execution: **Claude Code** and **Codex CLI**.

### Supported Backends

| Backend | Description | Provider |
|---------|-------------|----------|
| `claude` | Claude Code CLI (default) | Anthropic |
| `codex` | Codex CLI | OpenAI |

### Configuration Hierarchy

The CLI backend is determined using a priority-based fallback chain:

1. **Agent-specific `cli` field** in `registry.json` (highest priority)
2. **`DEFAULT_CLI` environment variable**
3. **`default_cli` field** in `registry.json`
4. **Hardcoded `"claude"` fallback** (lowest priority)

### Example: Mixed Backend Configuration

```json
{
  "default_cli": "claude",
  "team": [
    { "id": "nexus", "cli": "claude", ... },
    { "id": "forge", "cli": "codex", ... },
    { "id": "sentinel", "cli": "claude", ... }
  ]
}
```

### Codex-Specific Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CODEX_PATH` | Path to Codex CLI binary | `"codex"` |
| `OPENAI_API_KEY` | OpenAI API key for Codex | (required for Codex) |
| `OPENAI_BASE_URL` | OpenAI-compatible API endpoint | (optional, for proxies) |
| `CODEX_HOME` | Codex config directory per worktree | (auto-generated) |

### Codex Harness Primitives

When using Codex, AgentFlow maps these Codex concepts to its architecture:

| Codex Primitive | AgentFlow Equivalent |
|----------------|---------------------|
| `codex exec --full-auto` | `spawn_forge_with_backend(Codex)` |
| Hooks (SessionStart, PreToolUse, etc.) | `orchestration/plugin/hooks/` shell scripts |
| Skills (SKILL.md) | `orchestration/plugin/skills/` (symlinked) |
| Subagents (`.codex/agents/`) | `orchestration/agent/agents/*.agent.md` personas |
| AGENTS.md | Generated from agent.md files at provision time |
| Permission profiles | Generated in `.codex/config.toml` |
| MCP servers | Configured in `.codex/config.toml` |

For complete details, see **[docs/cli-backend-configuration.md](docs/cli-backend-configuration.md)**.

## LLM Provider Routing

AgentFlow routes LLM requests based on your `.env` configuration:

```
┌─────────────────────────────────────────────────────────┐
│  Registry model_backend (e.g. "accounts/fireworks/...") │
│                         │                                │
│                         v                                │
│              ┌── Auto-detected? ──┐                      │
│              │                    │                      │
│         Yes (prefix)         No (use MODEL_PROVIDER_MAP)│
│              │                    │                      │
│              v                    v                      │
│     FireworksClient        Resolved provider            │
│     (direct, no proxy)     (direct, no proxy)           │
└─────────────────────────────────────────────────────────┘

Fallback chain (when no model_override):
  PROXY_URL set? → AnthropicClient (proxy)
  FIREWORKS_API_KEY? → FireworksClient (direct)
  Otherwise → Direct API keys (ANTHROPIC_API_KEY, GEMINI_API_KEY, OPENAI_API_KEY)
```

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `PROXY_URL` | Optional | Proxy URL. When set, agents route through the proxy. When unset, agents use direct API access. |
| `PROXY_API_KEY` | Optional | API key for the proxy. Falls back to `ANTHROPIC_API_KEY`. |
| `FIREWORKS_API_KEY` | Optional | Fireworks API key. When set, FireworksClient is used directly (no proxy needed). |
| `OPENAI_BASE_URL` | Optional | OpenAI-compatible API endpoint. Critical for Codex+Fireworks. |
| `OPENAI_API_KEY` | Optional | OpenAI API key. When using Fireworks, set to same as `FIREWORKS_API_KEY`. |
| `ANTHROPIC_API_KEY` | Optional | Anthropic API key (used by FORGE, NEXUS, and as fallback) |
| `GEMINI_API_KEY` | Optional | Google Gemini API key (used by SENTINEL via direct key) |
| `GATEWAY_URL` | Optional | Remote OpenAI-compatible gateway URL. Used by anthropic-mock proxy. |
| `GATEWAY_API_KEY` | Optional | API key for the remote gateway. Falls back to `PROXY_API_KEY`. |

### OpenAI-Only Gateways (Local anthropic-mock Proxy)

If your LLM gateway only supports the OpenAI Chat Completions format (`/v1/chat/completions`), Claude CLI will fail because it speaks Anthropic Messages API (`/v1/messages`). AgentFlow includes a local protocol translator:

```bash
# Terminal 1: Start the proxy (reads .env automatically)
cargo run -p anthropic-mock

# Terminal 2: Run orchestration
cargo run --bin agentflow
```

Configure `.env`:
```env
# Claude CLI and Nexus send Anthropic requests to the LOCAL proxy
PROXY_URL=http://localhost:8765/v1
PROXY_API_KEY=your-gateway-api-key

# The LOCAL proxy forwards OpenAI-format requests to the REMOTE gateway
GATEWAY_URL=https://api.fireworks.ai/inference/v1/
GATEWAY_API_KEY=your-gateway-api-key
```

When your provider adds native Anthropic support, change `PROXY_URL` to point directly to the gateway and remove `GATEWAY_*`.

### Disabling Proxy (Direct API Access)

If `PROXY_URL` is not set, all agents use direct API access with `ANTHROPIC_API_KEY` — this is the default behavior and requires no proxy setup.

## Requirements

- Rust 1.70+
- Node.js 18+ (for GitHub MCP server)
- **Codex CLI** (`npm install -g @openai/codex`) or **Claude Code CLI** (`npm install -g @anthropic-ai/claude-code`)
- API keys (choose one):
  - **Codex + Fireworks**: `FIREWORKS_API_KEY` + `OPENAI_BASE_URL` (recommended — no proxy needed)
  - **Codex + OpenAI**: `OPENAI_API_KEY` only (no `OPENAI_BASE_URL` needed — direct connection)
  - **Claude direct**: `ANTHROPIC_API_KEY`
  - **Claude + third-party gateway**: `PROXY_API_KEY` + `GATEWAY_API_KEY` (proxy required)
- `GITHUB_PERSONAL_ACCESS_TOKEN` (required for all modes)

Before first use, authenticate your CLI:
  Codex:  echo "your-key" | codex login --with-api-key
  Claude: claude login

## License

MIT
