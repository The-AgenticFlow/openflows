# OpenFlows - Autonomous AI Development Team

> Official site: [openflows.dev](https://openflows.dev)

**OpenFlows is an autonomous software development team that runs itself.**

Imagine having a complete engineering team — Scrum Master, Senior Developer, Security Auditor, DevOps Engineer, and Technical Writer — that works 24/7 to turn your GitHub issues into production-ready code and pull requests. All without you writing a single line of code.

## Installation

See **[INSTALL.md](INSTALL.md)** for complete installation and configuration instructions.

Quick start:

```bash
# One-line install (recommended)
curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash

# Or via cargo
cargo install openflows
openflows-setup
openflows
```

## The Big Idea: You Stay the Product Owner

**Each AI agent gets their own account and identity.** They create branches, open PRs, review code, run CI/CD, and deploy — just like human developers.

**You stay as the client/product owner.** NEXUS (the orchestrator) notifies you via your preferred communication channel (WhatsApp, Discord, Email, etc.) only when necessary:
- Specification discrepancies need clarification
- Credits/API limits are exhausted
- Security concerns require human approval
- Final approval for major architectural decisions

**Otherwise, the team runs autonomously.** You wake up to completed features, reviewed PRs, and updated documentation.

![AgentFlow Architecture](image.png)

## The Team

| Agent | Role | Description |
|-------|------|-------------|
| **NEXUS** | Orchestrator | Scrum Master & Tech Lead. Assigns tickets, approves dangerous commands. |
| **FORGE** | Builder | Senior Engineer. Writes code, tests, opens PRs via Claude Code. |
| **SENTINEL** | Reviewer | Security auditor. Reviews PRs, ensures all logic is tested. |
| **VESSEL** | DevOps | Deployment expert. Manages CI/CD and rollbacks. |
| **LORE** | Writer | Documenter. Writes ADRs, maintains project history. |

### Human-in-the-Loop (Only When Needed)

NEXUS reaches out to you via your configured channels when:
- **Security concerns** — SENTINEL flags potential vulnerabilities
- **Resource limits** — API credits exhausted, need approval to continue
- **Spec ambiguity** — Issue description unclear, needs clarification
- **Architecture decisions** — Major design choices need product owner input
- **CI failures** — Tests failing repeatedly, needs human debugging

**Default mode:** Autonomous execution. You only hear from the team when they need you.

## Architecture

```
openflows/
├── orchestration/agent/
│   ├── agents/              # Agent personas (nexus.agent.md, forge.agent.md)
│   ├── registry.json        # Agent definitions and model routing
│   └── standards/           # Coding standards
├── crates/
│   ├── agent-nexus/         # Orchestrator node
│   ├── agent-forge/         # Builder node (spawns Claude Code)
│   ├── agent-client/        # LLM client + MCP integration
│   ├── pair-harness/        # Worktree management, process spawning
│   └── pocketflow-core/     # Flow engine, shared store, routing
└── binary/src/bin/
    ├── agentflow.rs         # Main entry point
    └── demo.rs              # Mocked demonstration
```

## How It Works

```
GitHub Issue → NEXUS assigns → FORGE codes → SENTINEL reviews → VESSEL merges → You get notified
```

### The Lifecycle of an Issue

1. **Discovery** — NEXUS polls GitHub and discovers new issues
2. **Assignment** — NEXUS assigns tickets to available FORGE workers
3. **Implementation** — FORGE spawns Claude Code, writes code, opens PRs
4. **Review** — SENTINEL reviews PRs for security, quality, and test coverage
5. **Iteration** — If issues found, FORGE fixes them; loop continues
6. **Merge** — VESSEL merges approved PRs, handles CI/CD
7. **Documentation** — LORE writes ADRs and updates docs
8. **Notification** — NEXUS notifies you only when human input needed

### The Orchestration Cycle

1. **NEXUS** fetches open GitHub issues and assigns them to available FORGE workers
2. **FORGE** creates an isolated worktree, writes PLAN.md, then implements code via Claude Code
3. **SENTINEL** reviews the plan (CONTRACT.md), evaluates each code segment, and performs final review
4. **FORGE** opens a PR once SENTINEL approves
5. **VESSEL** polls CI status, detects merge conflicts, attempts resolution, and squash-merges green PRs
6. **LORE** generates documentation: ADRs, changelogs, and project history updates
7. **NEXUS** loops back to assign the next ticket or halts when no work remains

### Agent Accounts & Identity

Each agent operates with their own identity:
- **Separate GitHub tokens** — Each agent can have their own PAT
- **Named branches** — `forge-1/feature-xyz`, `sentinel/review-123`
- **Attribution in commits** — Know which agent made changes
- **Individual worktrees** — Agents work in isolated directories

### Shared State

All agents communicate through a **SharedStore** (in-memory or Redis):

| Key | Purpose |
|-----|---------|
| `tickets` | GitHub issues converted to internal work items |
| `worker_slots` | Available FORGE workers and their status |
| `pending_prs` | PRs awaiting CI completion |

## Key Files

| File | Purpose |
|------|---------|
| [`orchestration/agent/agents/nexus.agent.md`](orchestration/agent/agents/nexus.agent.md) | Orchestrator persona and workflow |
| [`orchestration/agent/agents/forge.agent.md`](orchestration/agent/agents/forge.agent.md) | Builder persona and instructions |
| [`orchestration/agent/registry.json`](orchestration/agent/registry.json) | Worker slot definitions |
| [`binary/src/bin/agentflow.rs`](binary/src/bin/agentflow.rs) | Main entry point |
| [`crates/agent-forge/src/lib.rs`](crates/agent-forge/src/lib.rs) | Forge node implementation |

## Documentation

| Document | Description |
|----------|-------------|
| [INSTALL.md](INSTALL.md) | Complete installation and configuration guide |
| [TUTORIAL.md](TUTORIAL.md) | Step-by-step tutorial with logs and troubleshooting |
| [RUN.md](RUN.md) | Running and configuration reference |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Development guidelines |
| [docs/demo.md](docs/demo.md) | Live flow walkthrough |
| [docs/setup-claude-cli.md](docs/setup-claude-cli.md) | Claude CLI setup |
| [docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md) | Architecture deep dive |

## License

MIT