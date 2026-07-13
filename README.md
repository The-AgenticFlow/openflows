# OpenFlows — Autonomous AI Development Team on Coder

> Official site: [openflows.dev](https://openflows.dev)

**OpenFlows is an autonomous software development team orchestrator that runs on your self-hosted Coder deployment.**

Give it a GitHub repo and some issues, and OpenFlows orchestrates a team of coordinated AI agents that plan the work, write the code, review it adversarially, and ship reviewed PRs — without you writing a single line of code. Each agent runs as a **Coder Agent** (control-plane AI loop) operating on an ephemeral, governed Coder workspace, with LLM keys kept in the Coder control plane and every action tied to your identity.

## Why architecture-first

AI can generate code against a spec, but it can't write the spec. As models make boilerplate cheap, the real difficulty shifts *up the stack* — into architectural thinking, product judgment, and security awareness. OpenFlows encodes that discipline: a declared flow graph (PocketFlow), typed SharedStore state contracts, an explicit routing table, and recovery built into every step. **Engineering goes in, software comes out.** See [`docs/articles/architecture-is-the-product.md`](docs/articles/architecture-is-the-product.md).

## Quick Start

```bash
git clone https://github.com/The-AgenticFlow/openflows.git
cd openflows
cp .env.example .env   # edit .env with your GitHub OAuth app credentials

# 1. Start the infrastructure stack
docker compose up -d

# 2. Bootstrap OpenFlows (creates admin, pushes templates, verifies config)
cargo run -p openflows --bin openflows -- bootstrap

# 3. Add a tenant (links GitHub via Coder external auth, creates nexus workspace)
cargo run -p openflows --bin openflows -- tenant add owner/repo --name my-team

# 4. Create a GitHub issue in your repo — OpenFlows picks it up automatically
```

**Prerequisites:** Docker 24+, Git 2.x+, a GitHub OAuth App (for external auth), and at least one LLM provider configured in the Coder dashboard (AI Settings → Coder Agents → Models).

## How It Works

OpenFlows runs a team of AI agents that collaborate just like a real engineering team:

```
You create a GitHub issue → NEXUS picks it up → FORGE writes code → SENTINEL reviews adversarially
→ VESSEL merges green PRs → LORE documents → you get a merged PR
```

You stay in the loop only when needed — security concerns, ambiguous specs, or major decisions. Otherwise, the team runs autonomously, with NEXUS's `reconcile()` detecting orphans, stale workers, and unmerged PRs and recovering automatically.

### Coder governs *where* agents run — OpenFlows governs *how* they coordinate

The integration is deliberate and asymmetrical:
- **Coder** provides the governed environment: ephemeral workspaces, control-plane AI agents, model governance, identity, audit logging, cost tracking. The workspace has zero AI software and zero LLM keys.
- **OpenFlows** provides the brain: the flow graph, typed SharedStore contracts, the Node trait's `prep → exec → post` separation, and the FORGE↔SENTINEL planning cycle.

Coder Agents run in the **control plane** (not in workspaces). They execute tool calls by connecting to workspaces over the same secure tunnel as IDEs. You watch agents coding live in the Coder Agents chat UI with diffs, status, and message streaming.

### The `openflows-harness` CLI

Each worker workspace gets a small `openflows-harness` binary. The Coder Agent invokes it via shell (guided by skills) to read/write the Redis SharedStore with typed, validated schemas. Agents never run `redis-cli` directly — the harness is the only Redis client in a workspace.

## The Team

| Agent | Role | Plan mode | What it does |
|-------|------|-----------|--------------|
| **NEXUS** | Orchestrator | yes | Assigns issues, coordinates the team, owns `reconcile()` failure recovery, notifies you when needed |
| **FORGE** | Builder | no | Writes code against an agreed `CONTRACT.md`, creates branches, opens PRs |
| **SENTINEL** | Reviewer | yes | Adversarially reviews code for security, quality, and test coverage against the contract |
| **VESSEL** | DevOps | no | Monitors CI, handles merge conflicts, squash-merges green PRs, tears down workspaces on merge |
| **LORE** | Writer | no | Documents decisions, updates changelogs, maintains project history *(disabled by default — enable in the registry)* |

## Multi-Tenancy

One Coder server serves many teams. Each tenant = a real Coder user + a repo binding + an `openflows-nexus` workspace. Tenants are isolated by Coder RBAC and per-tenant Redis keyspace prefixes (`ns:{tenant}:...`).

```bash
# Add a new tenant
openflows tenant add another-org/another-repo --name team-b
```

## Plug-and-Play Extension

- **Add a skill**: Drop a directory in `orchestration/plugin/skills/` with a `SKILL.md`, list it in `registry.json` under the role's `skills` array. No code change.
- **Add an MCP server**: Add it to the role's `mcp` object in `registry.json`, or register it centrally in the Coder dashboard (AI Settings → MCP Servers). Both coexist.
- **Enable a new model**: Configure it in the Coder dashboard (AI Settings → Coder Agents → Models). Reference it in `registry.json` via the `model` field.

See [`docs/extending.md`](docs/extending.md) for details.

## Documentation

| Guide | What it covers |
|-------|---------------|
| [INSTALL.md](INSTALL.md) | Full installation and configuration |
| [RUN.md](RUN.md) | Running and configuration reference |
| [TUTORIAL.md](TUTORIAL.md) | Step-by-step walkthrough |
| [DEMO.md](DEMO.md) | Quick demo walkthrough (requires a real LLM key) |
| [BUILD.md](BUILD.md) | Building from source |
| [docs/coder-compatibility.md](docs/coder-compatibility.md) | Coder version compatibility and verification |
| [docs/tenancy.md](docs/tenancy.md) | Multi-tenant model and Redis namespacing |
| [docs/governance.md](docs/governance.md) | AI governance controls and network policy |
| [docs/extending.md](docs/extending.md) | Adding skills, MCP servers, and models |

## License

MIT
