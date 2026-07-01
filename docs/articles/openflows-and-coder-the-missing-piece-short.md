# The Missing Piece: Why AI Agents Need @OpenFlows Orchestration + @Coder Infrastructure

> OpenFlows orchestrates agent teams. Coder governs where they run. Together: coordinated, isolated, auditable, enterprise-ready.

---

## The Problem Nobody Talks About

You've seen the AI coding demos: type a prompt, get code, open PRs. It works on your laptop.

It falls apart in production. API keys exposed in env files. Agents with unrestricted network access. Multiple agents colliding on the same codebase. Crash recovery leaving repos broken. Audit trails showing "bot-account" with no human attribution.

The agent works. The system around it doesn't.

**OpenFlows** solves the orchestration — how agents coordinate like a real team. Add **Coder** as the deployment target, and those agents run inside governed, isolated environments.

Neither is the full answer alone. But together?

---

## What OpenFlows Actually Does

OpenFlows is an **orchestration system** — a team of specialized agents that coordinate like a real engineering team:

- **NEXUS** — picks up issues, assigns workers, routes failures, keeps the pipeline moving
- **FORGE** — plans, segments work, writes code, opens PRs
- **SENTINEL** — spawns fresh per evaluation with specific, actionable feedback against contracts
- **VESSEL** — deterministic (no LLM). Checks CI, handles conflicts, merges
- **LORE** — records decisions, updates changelogs, maintains project history

Agents communicate through a **shared state store**, not chat messages. Event-driven, recoverable, observable. Works standalone right now — `openflows` on any machine with git.

But standalone OpenFlows runs on developer machines. API keys in env files. No network isolation. No enterprise audit attribution. Works great for small teams and OSS. A gap for regulated industries.

---

## What Coder Actually Does

Coder is **infrastructure for running agents** — and humans — inside controlled, network-isolated workspaces:

- **Terraform-defined workspaces** — reproducible environments, zero "works on my machine" drift
- **A control plane** — LLM runs centrally, API keys never touch the workspace
- **Identity & audit** — every action attributed to the human who started it
- **Network isolation** — workspaces reach only what they need, full egress control
- **Model governance** — admins approve providers, manage costs, enforce policies

Coder solves the **_where_** problem. But its built-in agent is single-agent, single-workspace. No multi-agent planning, no parallel review, no orchestration intelligence.

---

## Why They Need Each Other

**OpenFlows orchestrates agent workflows. Coder governs where those workflows run.**

Without Coder, OpenFlows agents run on developer machines with exposed API keys and no audit trail. Fine for small teams. Not enough for finance, healthcare, or government — where **every action needs attribution and every connection needs justification**.

Without OpenFlows, Coder's agent does individual tasks well but **can't run a multi-agent pipeline**. Can't decompose features into parallel workstreams. Can't have one agent write while another reviews independently. Can't recover from failures by routing around blocked workers.

Together: OpenFlows provides the orchestration brain. Coder provides the governed environment.

---

## Five Integration Layers

**Layer 1: Kernel-Level Isolation**
Standalone OpenFlows uses git worktrees with file locks. With Coder, each agent gets its own isolated workspace. Agent A literally cannot access Agent B's work. Separation goes from convention to enforcement.

**Layer 2: Flow Graphs Replace Agent Loops**
Coder's agent does prompt → think → respond. OpenFlows replaces this with a declared **flow graph** where each node (NEXUS, FORGE-SENTINEL, VESSEL) follows prep-execute-post patterns. One agent can hold one conversation. A flow graph runs a pipeline.

**Layer 3: Human Attribution**
OpenFlows alone uses a shared bot token. Coder attributes every agent action to the authenticated human who started it. Required by regulated industries. Not negotiable.

**Layer 4: Workspaces as Orchestration Objects**
NEXUS provisions workspaces from templates on ticket assignment. VESSEL stops them on merge. Crashes trigger automatic recreation. Infrastructure becomes part of the orchestration graph.

**Layer 5: Gradual Adoption**
- **Standalone OpenFlows** — works today with local worktrees
- **Coder + OpenFlows** — enterprise deployment with full governance
- **Coder only** — workspace governance without multi-agent orchestration

Each mode independently valuable. Each transition incremental.

---

## What Changes for Developers

Create a GitHub issue. Agents pick it up. Code gets written, reviewed, and merged. You get a PR. **What doesn't change is what you don't have to worry about**:

- No API key exposure (Coder keeps it in the control plane)
- No workspace collisions (kernel-level isolation)
- No stalled agents blocking the team (NEXUS detects and reroutes)
- No audit compliance burden (every action traces back to you)
- No model selection paralysis (admins curate the list)

The developer describes the work. The system handles the rest — safely.

---

## What Changes for Platform Teams

- **One control plane** for models, prompts, and permissions — not scattered across machines
- **Per-template network policies** — workspaces locked down to only what they need
- **Cost attribution** — every LLM call, every workspace minute, tied to a user
- **No secrets sprawl** — API keys exist only in the control plane
- **Terraform reproducibility** — every workspace identical by design

---

## Where It Stands Right Now

This isn't theoretical. The codebase already implements:

- `CoderClient` speaking the Chats API
- NEXUS creating Coder workspaces bound to ticket assignments
- VESSEL destroying workspaces on merge (no dead infrastructure)
- SharedStore carrying ticket-scoped state across agents
- Templates installing modules, writing heartbeats, keeping persistent workspaces alive
- Notifier layer escalating `awaiting_human` to Slack, Discord, or WhatsApp

The incremental roadmap continues: transport abstraction, shared state migration, full lifecycle management, governance integration, and an MCP bridge for the Coder dashboard. Each phase independently deployable.

---

## The Deeper Pattern

The history of software engineering is **separation of concerns**. AI agents are going through the same evolution.

Most tools conflate intelligence and infrastructure governance. You shouldn't have to choose between "smart agents" and "governed agents." OpenFlows answers "how smart can the agents be?" Add Coder and you get "how safely can they run?"

The teams that figure out both first will ship **faster and safer**. Not one or the other. Both.

**Because code is the output. Orchestration is the brain. Infrastructure is the building.**

---

*\@OpenFlows is open-source at [github.com/The-AgenticFlow/openflows](https://github.com/The-AgenticFlow/openflows)*
*\@Coder is at [coder.com](https://coder.com)*
