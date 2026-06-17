# OpenFlows - Autonomous AI Development Team

> Official site: [openflows.dev](https://openflows.dev)

**OpenFlows is an autonomous software development team that runs itself.**

Give it a GitHub repo and some issues, and OpenFlows handles everything — writing code, opening PRs, reviewing changes, merging, and documenting — without you writing a single line of code.

## Quick Start

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash

# Set up your environment (interactive wizard)
openflows-setup

# Run
openflows
```

That's it. The setup wizard walks you through configuring your GitHub repo, API keys, and CLI backend.

<details>
<summary>Other install options</summary>

- **Homebrew (macOS):** `brew tap The-AgenticFlow/openflows && brew install openflows`
- **Cargo:** `cargo install openflows`
- **Docker:** See [INSTALL.md](INSTALL.md#option-3-docker)
- **npm:** `npm install -g @the-agenticflow/openflows`
- **From source:** See [BUILD.md](BUILD.md)

</details>

## How It Works

OpenFlows runs a team of AI agents that collaborate just like a real engineering team:

```
You create a GitHub issue → The team picks it up → Code is written, reviewed, and merged → You get a PR
```

You stay in the loop only when needed — security concerns, ambiguous specs, or major decisions. Otherwise, the team runs autonomously.

![OpenFlows Architecture](image.png)

## The Team

| Agent | Role | What it does |
|-------|------|-------------|
| **NEXUS** | Orchestrator | Assigns issues, coordinates the team, notifies you when needed |
| **FORGE** | Builder | Writes code, creates branches, opens PRs |
| **SENTINEL** | Reviewer | Reviews code for security, quality, and test coverage |
| **VESSEL** | DevOps | Monitors CI, handles merge conflicts, squash-merges green PRs |
| **LORE** | Writer | Documents decisions, updates changelogs, maintains project history |

## Prerequisites

### System Requirements

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Git** | 2.x+ | Required for repo cloning, worktree management, and branching |
| **Node.js** | 18+ | Required for the GitHub MCP server (`npx -y @modelcontextprotocol/server-github`) |
| **C compiler** | — | `build-essential` (Debian/Ubuntu) or `xcode-select --install` (macOS) |
| **OpenSSL dev headers** | — | `pkg-config` + `libssl-dev` (Debian/Ubuntu) or `brew install openssl` (macOS) |
| **Rust** | 1.70+ | Only required if building from source (`cargo install openflows`) |

### GitHub

- **A GitHub repository** — the repo OpenFlows will work on
- **A GitHub Personal Access Token** — with `repo` scope (set as `GITHUB_PERSONAL_ACCESS_TOKEN`)

### AI Backend (choose one)

| Mode | CLI | Required API Key | Install |
|------|-----|-------------------|---------|
| **Claude + Anthropic** | Claude Code | `ANTHROPIC_API_KEY` | `npm install -g @anthropic-ai/claude-code && claude login` |
| **Codex + OpenAI** | Codex | `OPENAI_API_KEY` | `npm install -g @openai/codex && codex login --with-api-key` |
| **Codex + Fireworks** | Codex | `FIREWORKS_API_KEY` | `npm install -g @openai/codex && codex login --with-api-key` |

Set `DEFAULT_CLI` to `claude` or `codex` to select your backend.

### Optional (Recommended for Production)

| Service | Purpose | Default |
|---------|---------|---------|
| **Redis 7** | Persistent state across restarts | In-memory (state lost on restart) |
| **LiteLLM proxy** | Per-agent model routing, cost optimization, rate limits | Direct API calls |

Both are included in the Docker Compose stack (`docker compose up`).

### Environment Setup

```bash
cp .env.example .env
# Edit .env with your tokens and API keys
```

The `openflows-setup` wizard handles configuration interactively. See [.env.example](.env.example) for all available options.

## Documentation

| Guide | What it covers |
|-------|---------------|
| [INSTALL.md](INSTALL.md) | Full installation options and configuration |
| [RUN.md](RUN.md) | Running and configuration reference |
| [TUTORIAL.md](TUTORIAL.md) | Step-by-step walkthrough with logs |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute to OpenFlows |
| [BUILD.md](BUILD.md) | Building from source |
| [DEMO.md](DEMO.md) | Quick demo (no API keys needed) |

## For Contributors

```bash
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
make install   # build release binaries & install to ~/.local/bin
openflows-setup
openflows
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full contributor workflow and [BUILD.md](BUILD.md) for build details.

## License

MIT