# The Missing Piece: Why AI Coding Agents Need Both a Brain and a Building

> Most AI coding tools give you either intelligence or infrastructure. @OpenFlows and @Coder give you both — and that changes everything.

---

## The Problem Nobody Talks About

You've seen the demos. Type a prompt, an AI agent writes code, tests pass, PRs open. It looks magical.

Then you put it in production. API keys sit in environment variables. Agents run with unrestricted network access. Multiple agents step on each other's files. One crashes mid-task and leaves the repo broken. The audit log says "bot-account" did something — but not which human authorized it or why.

The code generation works. The architecture around it doesn't.

Two projects have been building solutions from opposite sides. **Coder** makes it safe to run agents by governing the infrastructure. **OpenFlows** makes agents effective by governing how they coordinate with each other. They've been solving complementary problems this whole time.

Let me explain what happens when you put them together.

---

## What Coder Actually Does

Coder is not an AI agent. It's **infrastructure for running agents** — and humans — inside controlled, network-isolated workspaces.

Coder gives you:

- **Terraform-defined workspaces** — reproducible dev environments. Every agent gets an identical, clean workspace.
- **A control plane** — the LLM loop runs centrally. Workspaces get zero API keys, zero agent software. Nothing to steal.
- **Identity & audit** — every action tied to the human who submitted the prompt. Full attribution, no shared bot accounts.
- **Network isolation** — workspaces reach only your git provider and the control plane. The agent works but can't phone home with your source code.
- **Model governance** — admins approve providers and models. No shadow IT.

This solves the **_where_** problem. But Coder's built-in agent is single-agent, single-workspace. It doesn't plan before coding, review its own work, or coordinate with other agents in parallel.

That's the **_how_** problem. And that's where OpenFlows comes in.

---

## What OpenFlows Actually Does

OpenFlows is not infrastructure. It's an **orchestration system** — a team of specialized agents that coordinate like a real engineering team.

- **NEXUS** — the orchestrator. Picks up GitHub issues, assigns workers, routes through pipeline, detects failures, and reroutes.
- **FORGE** — the builder. Reads issues, creates plans, segments work, writes code, and opens PRs.
- **SENTINEL** — the reviewer. Spawned fresh for every evaluation — plan, segment, and final review — with specific, actionable feedback.
- **VESSEL** — the merge gatekeeper. Deterministic (no LLM). Checks CI, handles conflicts, cleans up after merges.
- **LORE** — the documentarian. Records decisions, updates changelogs, and maintains project history.

Agents communicate through a **shared state store**, not chat messages. The system is event-driven, recoverable, and observable.

But OpenFlows doesn't govern where agents run. It doesn't control API key distribution. It doesn't enforce network boundaries. It doesn't provide enterprise audit logs.

**It needs a kitchen.**

---

## Why They Need Each Other

**Coder governs where agents run safely. OpenFlows governs how agents coordinate intelligently.**

Without Coder, OpenFlows agents run on developer machines with API keys in env files, no network isolation, no central audit. That works for small teams. It breaks down in finance, healthcare, and government — where every action needs attribution and every connection needs justification.

Without OpenFlows, Coder's agent does individual tasks well, but it can't decompose a feature into parallel workstreams, have one agent write while another reviews independently, or recover from failures by routing around blocked workers.

Together: an enterprise-grade, AI-native development platform where agents are both **well-governed** and **well-coordinated**.

---

## The Integration: Five Layers

### Layer 1: Workspaces as Isolation Boundaries
Standalone OpenFlows uses local git worktrees with file locks. With Coder, each FORGE-SENTINEL pair gets its own kernel-level isolated workspace. Agent A **literally cannot access** Agent B's workspace. API keys never enter workspaces — they stay in the control plane.

### Layer 2: Orchestration Replaces the Agent Loop
Coder's built-in agent does prompt → think → respond. OpenFlows replaces this with a **flow graph** — a declared routing table where each node (NEXUS, FORGE-SENTINEL, VESSEL) follows prep-execute-post patterns. One agent can hold one conversation. A flow graph runs a pipeline.

### Layer 3: Every Action Has a Name
OpenFlows alone uses a shared bot token. Coder attributes every agent action to the authenticated human who started it. Required by finance, healthcare, and government. Not negotiable.

### Layer 3.5: Template Selection
NEXUS picks the right Coder template for the right task — locked-down egress for backend security work, full toolchain for frontend projects. Automatic.

### Layer 4: Workspace Management as a Tool
Workspaces become first-class orchestration objects. NEXUS provisions from templates on ticket assignment. VESSEL stops them on merge. Crashes trigger automatic recreation from the same template with shared state resumption.

### Layer 5: Gradual Adoption
Three modes, each independently valuable:
- **Standalone OpenFlows** — works today with local worktrees
- **Coder + OpenFlows** — enterprise deployment with governance
- **Coder only** — workspace governance without multi-agent orchestration

## What Changes

**For developers:** create a GitHub issue, agents pick it up, you get a PR. What *doesn't* change: you don't worry about API keys, workspace collisions, stalled agents, audit compliance, or model selection. The system handles all of that.

**For platform teams:** one control plane for all agent activity. Per-template network policies. Cost attribution per user and team. No secrets sprawl. Terraform reproducibility. Coder provides the infrastructure governance. OpenFlows provides the AI orchestration.

## Where It Stands

This isn't theoretical. The codebase already implements:
- `CoderClient` using the Chats API
- Nexus creating Coder workspaces bound to ticket assignments
- VESSEL destroying workspaces on merge
- SharedStore carrying ticket-scoped state across agents
- Templates installing agent modules and maintaining heartbeats
- Notifier layer escalating `awaiting_human` to Slack/Discord/WhatsApp

Incremental build-out continues with transport abstraction, shared state migration, full workspace lifecycle management, governance integration, and an MCP bridge for the Coder dashboard.

---

*OpenFlows is open-source at [github.com/The-AgenticFlow/openflows](https://github.com/The-AgenticFlow/openflows). Coder is at [coder.com](https://coder.com). The architecture design for the integration is at `docs/architecture/openflows-coder-integration.md` in the OpenFlows repository.*
