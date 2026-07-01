# The Missing Piece: Why AI Coding Agents Need Orchestration *and* Infrastructure

> Most AI coding tools give you intelligence. OpenFlows gives you orchestration. Add Coder for infrastructure governance, and you get both — anywhere you deploy.

---

## The Problem Nobody Talks About

You've seen the demos. Type a prompt, an AI agent writes code, tests pass, PRs open. It looks magical.

Then you put it in production. API keys sit in environment variables. Agents run with unrestricted network access. Multiple agents step on each other's files. One crashes mid-task and leaves the repo broken. The audit log says "bot-account" did something — but not which human authorized it or why.

The code generation works. The architecture around it doesn't.

Most AI coding tools solve one half of this equation. **OpenFlows solves how agents coordinate.** Add **Coder as a deployment target**, and those coordinated agents run inside governed, isolated environments with full audit trails.

Let me explain what happens when you put them together.

---

## What OpenFlows Actually Does

OpenFlows is an **orchestration system** — a team of specialized agents that coordinate like a real engineering team. It runs anywhere: local machines, servers, Coder workspaces, or cloud environments.

- **NEXUS** — the orchestrator. Picks up GitHub issues, assigns workers, routes through a declared pipeline, detects failures, and reroutes.
- **FORGE** — the builder. Reads issues, creates plans, segments work, writes code, and opens PRs.
- **SENTINEL** — the reviewer. Spawns fresh for every evaluation — plan, segment, and final review — with specific, actionable feedback.
- **VESSEL** — the merge gatekeeper. Deterministic (no LLM). Checks CI, handles conflicts, cleans up after merges.
- **LORE** — the documentarian. Records decisions, updates changelogs, and maintains project history.

Agents communicate through a **shared state store**, not chat messages. The system is event-driven, recoverable, and observable. It works standalone right now — just open a terminal and run `openflows`.

But standalone OpenFlows runs on developer machines. API keys live in environment files. There's no network isolation. No central audit trail. No enterprise-grade attribution. That works for small teams and open-source contributors. It's a gap in regulated industries.

**That's not a bug in OpenFlows. It's an infrastructure problem.**

---

## What Coder Actually Does

Coder is **infrastructure for running agents** — and humans — inside controlled, network-isolated workspaces. It doesn't orchestrate multi-agent workflows. It provides the governed environment where any workflow can run safely.

Coder gives you:

- **Terraform-defined workspaces** — reproducible dev environments. Every agent gets an identical, clean workspace.
- **A control plane** — the LLM loop can run centrally. Workspaces get zero API keys, zero agent software. Nothing to steal.
- **Identity & audit** — every action tied to the human who submitted the prompt. Full attribution, no shared bot accounts.
- **Network isolation** — workspaces reach only your git provider and the control plane. The agent works but can't phone home with your source code.
- **Model governance** — admins approve providers and models. No shadow IT.

Coder solves the **_where_** problem. But Coder's built-in agent is single-agent, single-workspace. It doesn't plan before coding, review its own work, or coordinate with other agents in parallel.

---

## Why They Work Together

**OpenFlows governs how agents coordinate intelligently. Coder governs where agents run safely.**

Without Coder, OpenFlows agents run on developer machines with API keys in env files, no network isolation, no central audit. That works for small teams and OSS contributors. It's not enough for finance, healthcare, and government — where every action needs attribution and every connection needs justification.

Without OpenFlows, Coder's agent handles individual tasks well, but it can't decompose a feature into parallel workstreams. Can't have one agent write while another reviews independently. Can't recover from failures by routing around blocked workers. Lacks the architectural intelligence that makes multi-agent teams effective.

Together: OpenFlows provides the orchestration brain. Coder provides the governed environment. The agents are both **well-coordinated** and **well-governed**.

---

## The Integration: Five Layers

### Layer 1: Workspaces as Isolation Boundaries
Standalone OpenFlows uses local git worktrees with file locks. With Coder, each FORGE-SENTINEL pair gets its own kernel-level isolated workspace. Agent A cannot access Agent B's workspace. API keys never enter workspaces — they stay in the control plane.

### Layer 2: Flow Graphs Replace Agent Loops
Coder's built-in agent does prompt → think → respond. OpenFlows replaces this with a **flow graph** — a declared routing table where each node (NEXUS, FORGE-SENTINEL, VESSEL) follows prep-execute-post patterns. One agent holds one conversation. A flow graph runs a pipeline.

### Layer 3: Every Action Has a Name
OpenFlows standalone uses a shared bot token. Coder attributes every agent action to the authenticated human who started it. Required by finance, healthcare, and government. Not negotiable.

### Layer 4: Workspace Management as a Tool
Workspaces become first-class orchestration objects. NEXUS provisions from templates on ticket assignment. VESSEL stops them on merge. Crashes trigger automatic recreation from the same template with shared state resumption. Infrastructure becomes part of the orchestration graph.

### Layer 5: Gradual Adoption
Three modes, each independently valuable:
- **Standalone OpenFlows** — works today with local worktrees
- **Coder + OpenFlows** — enterprise deployment with full governance
- **Coder only** — workspace governance without multi-agent orchestration

Each transition is incremental. No fork, no rewrite.

---

## What Changes

**For developers:** create a GitHub issue, agents pick it up, you get a PR. What doesn't change is what you don't have to worry about: API keys, workspace collisions, stalled agents, audit compliance, or model selection. The system handles all of that.

**For platform teams:** one control plane for all agent activity. Per-template network policies. Cost attribution per user and team. No secrets sprawl. Terraform reproducibility. Coder provides infrastructure governance. OpenFlows provides orchestration intelligence.

## Where It Stands

This isn't theoretical. The codebase already implements:
- `CoderClient` speaking the Chats API
- NEXUS creating Coder workspaces bound to ticket assignments
- VESSEL destroying workspaces on merge instead of leaving dead infrastructure
- SharedStore carrying ticket-scoped state across agents
- Templates installing agent modules and maintaining heartbeats
- Notifier layer escalating `awaiting_human` to Slack, Discord, or WhatsApp

Incremental build-out continues with transport abstraction, shared state migration, full workspace lifecycle management, governance integration, and an MCP bridge for the Coder dashboard. Each phase is independently deployable.

---

## The Deeper Pattern

The history of software engineering is the history of **separation of concerns**. We separated design from implementation. Testing from development. Deployment from building. Each separation created a discipline.

AI coding agents are going through the same evolution. Right now, most tools conflate two things: the intelligence of the agent and the governance of the environment. When you ask "which AI coding tool should I use?" you're really asking two questions: "how smart can the agents be?" and "how safely can they run?"

OpenFlows answers the first question with architectural intelligence — plan before coding, review before merging, recover from failure. Add Coder and you get the second answer — infrastructure governance with identity, audit, isolation, and model control.

The teams that figure out both questions first will ship faster *and* safer. Not one or the other. Both.

**Because code is the output. Orchestration is the brain. Infrastructure is the building.**

---

*OpenFlows is open-source and available at [github.com/The-AgenticFlow/openflows](https://github.com/The-AgenticFlow/openflows). Coder is available at [coder.com](https://coder.com). The architecture design document for the integration is at `docs/architecture/openflows-coder-integration.md` in the OpenFlows repository.*
