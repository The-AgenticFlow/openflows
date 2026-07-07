# OpenFlows — Autonomous AI Development Team

> Official site: [openflows.dev](https://openflows.dev)

**OpenFlows is an autonomous software development team that runs itself — architecture-first, by construction.**

Give it a GitHub repo and some issues, and OpenFlows orchestrates a team of coordinated AI agents that plan the work, write the code, review it adversarially, and ship reviewed PRs — without you writing a single line of code. Each agent runs in an ephemeral, governed workspace provisioned by [Coder](https://coder.com), with LLM keys kept in the control plane.

## Why architecture-first

AI can generate code against a spec, but it can't write the spec. As models make boilerplate cheap, the real difficulty shifts *up the stack* — into architectural thinking, product judgment, and security awareness. OpenFlows encodes that discipline: a declared flow graph (PocketFlow), typed SharedStore state contracts, an explicit routing table, and recovery built into every step. **Engineering goes in, software comes out.** See [`docs/articles/architecture-is-the-product.md`](docs/articles/architecture-is-the-product.md).

## Quick Start

OpenFlows runs in two deployment modes. **Coder mode** (default) provisions ephemeral, governed workspaces via Coder with the AI Gateway routing LLM calls and keeping keys out of the agents. **Local mode** runs agents in local git worktrees with direct API calls — no Coder, no Docker required.

### Option 1: Docker Compose — full Coder stack (recommended)

One command brings up Coder + PostgreSQL + LiteLLM (fallback) + Redis + OpenFlows:

```bash
git clone https://github.com/The-AgenticFlow/openflows.git
cd openflows
cp .env.example .env   # edit .env with your GitHub PAT and provider keys

docker compose --profile coder up
```

The `CoderBootstrapper` runs on startup: it provisions the admin user, pushes the five role Terraform workspace templates, and obtains an API token. Agents then run inside Coder workspaces with the [Claude Code Coder Registry module](https://registry.coder.com/coder/claude-code/coder), AI Gateway enabled, and LLM credentials never injected into workspace env.

> No Coder Premium license? Set `USE_AI_GATEWAY=false` in `.env` and OpenFlows falls back to the bundled LiteLLM proxy for model routing.

### Option 2: Standalone binary — local worktrees

For development or when you don't need the governed Coder environment, run agents locally with direct API access:

```bash
# One-line install
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/openflows/main/scripts/install.sh | bash

# Or edge (pre-release from main)
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/openflows/main/scripts/install.sh | bash -s -- --edge
```

Then set up and run:

```bash
openflows-setup   # interactive wizard — configures repo, API keys, CLI backend, provider mode
openflows          # start the autonomous team (local worktrees)
```

To use Coder mode with the standalone binary, set `CODER_URL` and `USE_AI_GATEWAY=true` in `.env` and point `CODER_URL` at your Coder server. OpenFlows detects `CODER_URL` and switches agents to Coder transport automatically.

### Option 3: Install from source

```bash
git clone https://github.com/The-AgenticFlow/openflows.git
cd openflows

# Build and install release binaries
make install   # installs to ~/.local/bin (copies orchestration/ too)

openflows-setup
openflows
```

Or build manually with Cargo:

```bash
cargo build --release -p openflows
# Binaries at target/release/{openflows,openflows-setup,openflows-dashboard,openflows-doctor}
# You also need the orchestration/ directory — copy it to ~/.local/bin/ or set OPENFLOWS_HOME
cp -r orchestration ~/.local/bin/
```

## How It Works

OpenFlows runs a team of AI agents that collaborate just like a real engineering team:

```
You create a GitHub issue → NEXUS picks it up → FORGE writes code → SENTINEL reviews adversarially
→ VESSEL merges green PRs → LORE documents → you get a merged PR
```

You stay in the loop only when needed — security concerns, ambiguous specs, or major decisions. Otherwise, the team runs autonomously, with NEXUS's `reconcile()` detecting orphans, stale workers, and unmerged PRs and recovering automatically.

![OpenFlows Architecture](image.png)

### Coder governs *where* agents run — OpenFlows governs *how* they coordinate

The integration is deliberate and asymmetrical: **Coder provides the governed environment** (ephemeral workspaces, AI Gateway, centrally managed keys, audit logging), while **OpenFlows provides the brain** (the flow graph, typed SharedStore contracts, the Node trait's `prep → exec → post` separation, and the FORGE↔SENTINEL planning cycle). Coder is a service dependency bundled in `docker-compose.yml` alongside PostgreSQL, Redis, and LiteLLM — not a code dependency.

### Model routing

- **Coder AI Gateway (primary, Coder mode)** — Anthropic calls route through the gateway; the Coder session token authenticates, so no `ANTHROPIC_API_KEY` ever enters a workspace. Audit logging and cost tracking are built in.
- **LiteLLM proxy (fallback + Local mode)** — per-agent model routing via `routing_key` dispatch. See [`litellm_config.yaml`](litellm_config.yaml).

## The Team

| Agent | Role | Permission mode | What it does |
|-------|------|-----------------|--------------|
| **NEXUS** | Orchestrator | `plan` | Assigns issues, coordinates the team, owns `reconcile()` failure recovery, notifies you when needed |
| **FORGE** | Builder | `acceptEdits` | Writes code against an agreed `CONTRACT.md`, creates branches, opens PRs |
| **SENTINEL** | Reviewer | `plan` | Adversarially reviews code for security, quality, and test coverage against the contract |
| **VESSEL** | DevOps | `acceptEdits` | Monitors CI, handles merge conflicts, squash-merges green PRs, tears down workspaces on merge |
| **LORE** | Writer | `acceptEdits` | Documents decisions, updates changelogs, maintains project history *(disabled by default — enable in the registry)* |

Each agent is a *pair*: a CLI backend (the muscle — Claude Code, Codex, etc.) plus a configuration harness (the brain — persona, skills, hooks, permissions). Forge and Nexus can share the same CLI yet behave as completely different agents. See [`docs/agentflow-pair-harness.md`](docs/agentflow-pair-harness.md).

## Prerequisites

### System Requirements

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Docker** | 24+ | Required for the Docker Compose stack (Coder mode). Not needed for standalone local mode. |
| **Git** | 2.x+ | Required for repo cloning, worktree management, and branching |
| **Node.js** | 18+ | Required for the GitHub MCP server (`npx -y @modelcontextprotocol/server-github`) |
| **C compiler** | — | `build-essential` (Debian/Ubuntu) or `xcode-select --install` (macOS) |
| **OpenSSL dev headers** | — | `pkg-config` + `libssl-dev` (Debian/Ubuntu) or `brew install openssl` (macOS) |
| **Rust** | 1.70+ | Only required if building from source |

### GitHub

- **A GitHub repository** — the repo OpenFlows will work on
- **A GitHub Personal Access Token** — with `repo` scope (set as `GITHUB_PERSONAL_ACCESS_TOKEN`)

### AI Backend

In **Coder mode**, agents install their CLI inside the workspace via the configured [Coder Registry module](https://registry.coder.com) and route through the AI Gateway — you only need provider keys in the Coder control plane (or, with AI Gateway off, in LiteLLM config).

In **standalone local mode**, install the CLI backend yourself and provide a provider key directly:

| Mode | CLI | Required API Key | Install |
|------|-----|-------------------|---------|
| **Claude + Anthropic** | Claude Code | `ANTHROPIC_API_KEY` | `npm install -g @anthropic-ai/claude-code && claude login` |
| **Codex + OpenAI** | Codex | `OPENAI_API_KEY` | `npm install -g @openai/codex && codex login --with-api-key` |
| **Codex + Fireworks** | Codex | `FIREWORKS_API_KEY` | `npm install -g @openai/codex && codex login --with-api-key` |

Set `DEFAULT_CLI` to `claude` or `codex` to select your backend.

### Bundled services (included in the Docker Compose stack)

| Service | Purpose | Coder mode | Local mode |
|---------|---------|------------|------------|
| **Coder** | Governed ephemeral workspaces for each agent | Required (via `--profile coder`) | Not used |
| **PostgreSQL 16** | Coder database | Used by Coder | Not used |
| **AI Gateway** | Centralized LLM routing, keys stay in control plane | Primary (`USE_AI_GATEWAY=true`) | Not available |
| **LiteLLM proxy** | Per-agent model routing, cost optimization, rate limits | Fallback for non-Anthropic providers | Primary routing path |
| **Redis 7** | SharedStore — persistent state across restarts and agents | Required (agent coordination) | Optional (in-memory fallback) |

### Environment Setup

```bash
cp .env.example .env
# Edit .env with your GitHub PAT, provider keys, and (for Coder mode) CODER_URL / USE_AI_GATEWAY
```

The `openflows-setup` wizard handles configuration interactively, including Coder mode detection. See [.env.example](.env.example) for all available options.

## Documentation

| Guide | What it covers |
|-------|---------------|
| [INSTALL.md](INSTALL.md) | Full installation options, Coder mode, and configuration |
| [RUN.md](RUN.md) | Running and configuration reference |
| [TUTORIAL.md](TUTORIAL.md) | Step-by-step walkthrough with logs |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute to OpenFlows |
| [BUILD.md](BUILD.md) | Building from source |
| [DEMO.md](DEMO.md) | Quick demo (no API keys needed) |
| [docs/ephemeral-coder-workspace-integration.md](docs/ephemeral-coder-workspace-integration.md) | Coder integration architecture and roadmap |
| [docs/coder-compatibility.md](docs/coder-compatibility.md) | Coder version compatibility |

## License

MIT