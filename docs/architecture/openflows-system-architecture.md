# OpenFlows — Complete System Architecture

**Version:** 1.0
**Status:** Authoritative reference (mirrors the shipped v1.2.x system)
**Scope:** Whole-system, end-to-end — from a GitHub issue to a merged PR to deployment, including every subsystem and extension point.

---

## 1. System Thesis

OpenFlows is an **autonomous AI software team** that turns GitHub issues into reviewed, production-ready pull requests, running entirely on governed, ephemeral [Coder](https://coder.com) workspaces.

The entire design rests on one asymmetry:

> **Coder governs WHERE agents run. OpenFlows governs HOW agents coordinate.**

- **Coder** provides the execution substrate: identity (SSO), administration, audit, network isolation, model governance, and per-workspace lifecycle. It never decides *what* the team should do.
- **OpenFlows** provides the intelligence: a declared flow graph, typed state contracts, a gated planning cycle, adversarial review, self-healing reconciliation, and human-in-the-loop escalation. It never decides *where* work executes.

Neither duplicates the other's core competency. Everything in this document flows from that split.

---

## 2. Big-Picture Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      CONTROL PLANE  (long-lived, trusted)                    │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                    CODER CONTROL PLANE (server)                       │  │
│  │   · Identity / SSO          · Audit log                                │  │
│  │   · Model governance        · AI Gateway (LLM routing)                 │  │
│  │   · MCP server registry     · Template registry                        │  │
│  │   · Chat API                · Provisioner daemon                       │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                              │  HTTPS / API                                 │
│  ┌───────────────────────────▼───────────────────────────────────────────┐  │
│  │                 OPENFLOWS CONTROLLER (NEXUS workspace)               │  │
│  │                                                                      │  │
│  │   ┌──────────────────────────────────────────────────────────────┐   │  │
│  │   │ Orchestration Engine (PocketFlow flow graph)                │   │  │
│  │   │  nexus → forge_pair → sentinel → vessel → lore              │   │  │
│  │   │  reconcile()  · planning gate  · routing table               │   │  │
│  │   └──────────────────────────────────────────────────────────────┘   │  │
│  │   ┌───────────────────────┐   ┌──────────────────────────────────┐   │  │
│  │   │ SharedStore client    │   │ A2A Relay (Axum HTTP :3000)      │   │  │
│  │   │ (pocketflow-core)     │   │ · route · allowlist · mirror     │   │  │
│  │   └───────────┬───────────┘   └────────────────┬─────────────────┘   │  │
│  └───────────────┼───────────────────────────────┼──────────────────────┘  │
│                  │                               │                          │
│  ┌───────────────▼───────────────────────────────▼──────────────────────┐  │
│  │                          SHARED INFRASTRUCTURE                      │  │
│  │   Redis (SharedStore) — single source of truth for all durable state│  │
│  │   GitHub (VCS + issue/PR) · Notification channels (Slack/Discord/WA)│  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│                                                                             │
│                     WORKLOAD PLANE  (ephemeral, untrusted)                  │
│  ┌───────────────────────────────┬───────────────────────────────────────┐  │
│  │  NEXUS workspace             │  WORKER workspaces (per tenant)       │  │
│  │  · Controller process        │   forge / sentinel / vessel / lore    │  │
│  │  · A2A relay                 │   · Coder Agent chat session          │  │
│  └───────────────────────────────┘   · openflows worker surface         │  │
│                                     · hooks · skills · .mcp.json        │  │
│                                     · git checkout · NO keys · NO LLM   │  │
│                                     · A2A executor daemon (forge)       │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**The "Big Four" subsystems** and their responsibilities are detailed below, followed by the deep zoom-ins.

---

## 3. Subsystem 01 — OpenFlows Controller (+ Web UI)

> **Deep-dive:** this section is a summary. The full internal architecture of the Controller is documented in `docs/architecture/openflows-controller.md`.

### 3.1 Role

The Controller is the **brain** of the fleet. It runs as a single long-lived process inside the `openflows-nexus` Coder workspace and drives the entire coordination loop. It is the only OpenFlows component that calls the Coder control-plane API to create chats and provision workspaces.

Entry point: `binary/src/bin/agentflow.rs` (`openflows` binary, `run` subcommand).

### 3.2 Required Environment (fail-fast)

The Controller requires all of these (injected by the Coder template — no fallback):

| Variable | Purpose |
|----------|---------|
| `CODER_URL` | Coder server base URL |
| `CODER_SESSION_TOKEN` | Scoped tenant-owner token (chat + workspace CRUD only, never admin) |
| `REDIS_URL` | SharedStore connection |
| `OPENFLOWS_TENANT` | Tenant identifier (namespaces every Redis key) |
| `GITHUB_REPOSITORY` | Target repo in `owner/repo` form |

### 3.3 The Orchestration Engine

On startup the Controller:

1. Validates environment.
2. Opens a **tenant-scoped** `SharedStore` (Redis) — all keys prefixed `ns:{tenant}:`.
3. Starts the **A2A relay** as a background Axum HTTP server (see Subsystem 04).
4. Resolves the orchestration directory and loads the **agent registry**, exposing it to workers via `OPENFLOWS_REGISTRY_*` env vars and a `registry_json` SharedStore key.
5. Builds the PocketFlow **flow graph** and all agent nodes (NEXUS loader, FORGE pair, SENTINEL, VESSEL, optional LORE).
6. Enters a **paced poll loop** — every 15 seconds it runs one flow pass, then sleeps.

The controller is deliberately **self-healing**: a flow pass that errors is logged and retried on the next poll; a flow error never kills the controller. The flow is bounded with `max_steps(1000)` and `max_visits_per_node(20)` to guard against routing cycles.

### 3.4 Controller CLI (admin surface)

The same binary exposes an operational surface beyond `run`. **Decision (Q2):** the **web UI is now the primary interactive control surface** (see 3.5), while the CLI remains the **programmatic / scripting surface** and the fallback for headless operation — every web action maps 1:1 to a CLI command, so both stay in sync and automation is preserved.

| Subcommand | Purpose |
|------------|---------|
| `run` | Start the Controller loop (default) |
| `bootstrap` | Create Coder admin, templates, verify LLM + external (GitHub) auth |
| `tenant add/list/clean/remove` | Manage multi-tenancy |
| `status` | Read-only snapshot of tickets/slots/PRs from Redis (table or JSON) |
| `control pause\|resume\|drain\|target …` | Halt / target / continue the fleet via the control plane |
| `control set-registry <json>` | Update the agent registry live (no restart) |
| `doctor` | Diagnose Coder integration health |
| `gate approve/status` | Record / inspect gate approvals |
| `reset-orchestration` | Restore orchestration files to bundled defaults |

### 3.5 Web UI (control surface)

**Decision (Q2):** OpenFlows ships a **dedicated OpenFlows web UI (control panel) "like Coder"** as the primary operator surface, replacing CLI-only operation for day-to-day control. It is a thin client over the same control-plane state and commands the CLI uses — it **reads and writes Redis-backed control state and the live registry**, and it never talks to Redis directly (the Controller/harness remain the only Redis writers).

The control panel provides:

- **Fleet live status** — tickets, worker slots, heartbeats, PRs, `awaiting_human` escalations (the machine-readable form of `openflows status`).
- **Agent registry editor** — view and edit the live team fleet (roles, `max_instances`, model, plan mode, skills, MCP) and apply changes with **no Controller restart** (see 3.6).
- **Halt / target / continue** — pause, drain, or target the fleet (repo / issue / label) through the `control` state (see 3.7).
- **Human intervention** — attend to `awaiting_human` escalations and link GitHub OAuth per tenant.
- **Coder-provided surfaces** (embedded/intra-linked) — models, MCP servers, templates, spend limits, model governance, external (GitHub) auth live in the Coder dashboard.

> The `openflows-dashboard` binary (scaffolded in the image build via the Dockerfile) is the delivery vehicle for this web UI; it exposes the same endpoints the CLI wraps, so the CLI and UI can never drift.

### 3.6 Agent Registry (control-plane defined, no file)

**Decision (Q1):** the agent registry is **defined and controlled entirely through the control plane**. The bundled `orchestration/agent/registry.json` is **eliminated** — it is no longer part of the system, not even as a seed.

The architectural rule is now:

> **The control plane (Redis `SharedStore` + a control surface) is the only source of truth for the agent registry. There is no registry file — the fleet is created, read, and updated entirely through the control plane.**

How this works:

1. **Control-plane defined** — the Controller builds the team fleet from the **live `registry_json` SharedStore key**; it does not read a file, and there is nothing to mount or seed from.
2. **Runtime source of truth** — the Controller (and every role node) resolves the registry from the `registry_json` SharedStore key, so a control-plane update takes effect **on the next poll pass** — no restart, no redeploy.
3. **Slot reconciliation is automatic** — the Controller rebuilds `worker_slots` from the live registry on every pass (via `all_worker_slots()` / `effective_instances()`), so changing `max_instances` (or adding/removing a role) dynamically rescales the fleet.
4. **Write path** — an authorized operator defines/updates the registry through a control surface: `openflows control set-registry <json>` on the CLI and/or the same endpoint exposed by the web UI (see 3.7). The default team fleet is a **control-plane-defined baseline**, not a shipped file. Writes are tenant-namespaced and validated against the registry schema; a malformed registry is rejected and never applied.
5. **Guarding against the zero-worker trap** — runtime overrides must be resolved through the same `effective_instances()` semantics (v1 `instances` vs v2 `max_instances`), so a partial override can never accidentally drive a role to zero workers.

This replaces the prior model where every tunable (concurrency, model, plan mode, skills, MCP, CLI backend) lived in a static, restart-gated file.

### 3.7 Addressed Design Gaps

Two control-plane gaps were resolved as part of this architecture decision:

- **Per-run / per-repo budgets (was Q1 gap):** a per-trigger / per-repo `dispatch_budget` can now be stored in Redis (`ns:{tenant}:control:*`) and consulted at assignment time, alongside the template-level `max_instances` ceiling. `max_instances` remains the hard resource cap for a role/template; the budget is a dynamic, per-run override injected at dispatch.
- **Halt / target / continue (was Q2 gap):** the control plane carries a `control` state (`paused | drained | targeted | auto`) with a `targets` set (repo, issue, label). NEXUS consults it in its `prep()` phase: `paused` → return `no_work` (graceful halt, in-flight work continues); `drained` → stop picking up new tickets; `targeted` → filter GitHub issue sync to the target set. These are exposed through both the CLI (`openflows control pause|resume|drain|target …`) and the web UI.

---

## 4. Subsystem 02 — Coder Infrastructure

### 4.1 What Coder Provides

Coder is the **execution substrate**. It contributes:

- **Identity & SSO** — every agent action inherits a real Coder user identity; no shared GitHub PATs.
- **Workspace templates** (Terraform) — define the VM/container, startup script, and network policy for each role (`openflows-forge`, `openflows-sentinel`, `openflows-nexus`, etc.).
- **Provisioner daemon** — materializes workspaces (docker provider in the shipped compose file).
- **Coder Agents & Chats API** — the LLM loop: each agent is a chat bound to a workspace.
- **AI Gateway** — routes model inference centrally; the worker workspace never talks to an LLM provider.
- **Model governance, MCP registry, spend limits, audit logging** — centralized, admin-configured.
- **Tailnet / DERP** — workspace daemon connectivity (file I/O, shell) and outbound reachability.

### 4.2 Control-plane ↔ Controller wiring

The Controller talks to Coder exclusively through the **Chats API** and workspace CRUD:

- Create/stop/delete workspaces from templates.
- Create chats with a bound workspace and (optionally) a model hint matched against the org-scoped `GET /api/v2/organizations/{organization}/chats/models`.
- Chats are created with an **empty content vector** — the agent's initial context comes from the `SessionStart` hook (Section 10), not a hardcoded prompt.

### 4.3 Network policy

Worker workspaces have heavily restricted egress:

```
ALLOW tcp/443 → coder-control-plane   (workspace daemon + AI Gateway)
ALLOW tcp/443 → github.com            (git push/pull, issue/PR API)
ALLOW           redis                  (SharedStore coordination)
DENY  everything else
```

Critically: **worker workspaces contain no LLM API keys and no agent framework software.** The AI loop runs in the control plane, eliminating entire classes of key-exfiltration and data-exfiltration risk.

---

## 5. Subsystem 03 — Agent Workspaces

### 5.1 Anatomy of a worker workspace

Each role (FORGE, SENTINEL, VESSEL, LORE) runs in its own ephemeral workspace, built from a role template. On boot the **startup script**:

1. **Installs `openflows`** (mandatory — startup fails if missing).
2. **Copies hooks** from the orchestration volume into `~/.openflows/hooks/`.
3. **Wires Claude settings** (`settings.json`) mapping hook events → scripts.
4. **Clones the repository** and checks out the pair branch.
5. **Starts the heartbeat** daemon.
6. **Materializes skills** into `.agents/skills/<name>/` and writes `.mcp.json` (from the registry + Coder dashboard).
7. For FORGE only: starts the **A2A executor daemon** (`openflows worker verify serve`).

### 5.2 The agent-side stack inside a workspace

```
┌──────────────────────────  WORKSPACE  ──────────────────────────┐
│  Coder Agent (LLM chat session)                                 │
│    · reads SessionStart hook output as initial context          │
│    · loads skills via `read_skill`                              │
│    · calls MCP tools                                            │
│                                                                 │
│  openflows worker surface  (the ONLY Redis client)                │
│    · dispatch read        · status get/set                      │
│    · gate approve/status  · pr opened/merged                    │
│    · handoff write        · heartbeat start/stop                │
│    · verify request/list/serve  (A2A surface)                   │
│                                                                 │
│  hooks (role-specific policy & bootstrap)                       │
│  skills (SKILL.md in .agents/skills/)                           │
│  .mcp.json (merged per-role + central server)                   │
│                                                                 │
│  git (clone/checkout/push) + repo checkout                      │
│  NO API keys · NO LLM keys · restricted egress                  │
└─────────────────────────────────────────────────────────────────┘
```

### 5.3 Why workspaces are isolated

Every FORGE–SENTINEL pair gets complete workspace isolation. No shared filesystem, no shared credentials. This is what makes adversarial review trustworthy: SENTINEL can reason about FORGE's work and even ask it to run commands, but **never touches FORGE's filesystem directly** (the A2A bridge in Subsystem 04).

### 5.4 Multi-tenancy

One Coder server serves many teams. Each tenant = a real Coder user + a repo binding + an `openflows-nexus` workspace. Isolation is enforced two ways simultaneously:

- **Coder RBAC** — separate users, separate workspace fleets.
- **Redis keyspace prefixes** — every SharedStore key is `ns:{tenant}:...`.

The Controller's tenant subcommands (`add`, `list`, `clean`, `remove`) manage the tenant fleet; `clean` resets `awaiting_human`/`failed` tickets back to `Open` and clears stale worker/recovery state.

---

## 6. Subsystem 04 — The Communication Plane (Coder Control Plane + Redis + A2A)

There are **three distinct channels** of communication, each solving a different problem.

### 6.1 Channel A — Controller ↔ Coder control plane (HTTPS)

The Controller provisions workspaces and creates/streams agent chats over the Coder API. This is how the Controller hands work to an agent (create chat → SessionStart hook → agent reads dispatch) and how it reads agent progress.

### 6.2 Channel B — Redis SharedStore (durable state, the single source of truth)

All **durable artifacts** live in Redis, tenant-namespaced, and are written **only** through `openflows worker` (in workspaces) or the Controller (pocketflow-core).

| Key pattern | Type | Purpose |
|-------------|------|---------|
| `ns:{t}:tickets` | `Vec<Ticket>` | Known tickets |
| `ns:{t}:worker_slots` | `HashMap<String, WorkerSlot>` | Worker availability + workspace IDs |
| `ns:{t}:pending_prs` | `Vec<Value>` | PRs awaiting merge |
| `ns:{t}:ticket:{id}:status` | `{phase, role, ts}` | Harness phase |
| `ns:{t}:ticket:{id}:gate:{phase}` | `GateApproval` | Single-use gate token |
| `ns:{t}:ticket:{id}:chat:{role}` | `String` | Coder chat ID |
| `ns:{t}:ticket:{id}:dispatch:{role}` | `DispatchPayload` | Task assignment |
| `ns:{t}:ticket:{id}:review:{role}` | `ReviewPayload` | Review verdict |
| `ns:{t}:ticket:{id}:recovery_attempts` | int | Recovery counter |
| `ns:{t}:heartbeat:{worker}` | ts | Liveness |

**Design rule:** the harness is the only Redis client inside workspaces. Every write is validated against serde schemas; malformed writes exit non-zero and are never silently accepted. `redis-cli` is disallowed.

### 6.3 Channel C — A2A protocol (live task exchange via a relay)

The A2A channel is used **only** for live, request/response task exchange — specifically delegated verification. The full protocol is specified in `openflows-a2a-relay.md`.

**Why A2A and not more Redis?** Redis expresses durable *facts*; a `verify` request is a live *task* (running, streaming, cancelling, resubscribing). A2A standardizes that lifecycle. The two are complementary, with a hard rule:

> **Redis remains the single source of truth for durable artifacts. A2A is used only for live task exchange, and every terminal A2A result is mirrored into Redis before the task is acknowledged complete.**

**Why a relay and not peer-to-peer?** Coder workspaces are good at initiating outbound connections and bad at accepting inbound ones. So the relay lives inside NEXUS, and both SENTINEL and FORGE dial **outbound** to it. Nexus routes by `(pair_id, role)`. This makes NEXUS the enforcement chokepoint:

- **Authorization** — only NEXUS knows the pair's role map.
- **Command allowlisting** — only safe prefixes pass (`cargo test`, `npm test`, `make test`, `bun test`). Rejections → `audit:a2a:rejected`.
- **Audit** — every accepted request and result is durably logged.
- **Single kill switch** — one relay to disable.

**The flow (SENTINEL verifying FORGE's code):**

```
SENTINEL workspace              NEXUS A2A relay              FORGE workspace
    │ verify request                 │                           │
    │  argv, timeout, expect         │                           │
    └───────────────────────────────▶│  validate allowlist       │
                                     │  route by pair_id         │
                                     │  dedup (pair, hash)       │
                                     └──────────────────────────▶│ verify serve
                                                                │  spawn process
                                                                │  enforce timeout
                                                                │  capture output
                                     ◀────────── result ─────────┘
    result ◀────────────────────────┤  mirror to Redis, ACK
```

**Failure semantics — "when in doubt, don't approve":**
- FORGE workspace offline → `executor_unavailable`, SENTINEL records a `blocked` verdict (unknown ≠ pass).
- Timeout → result carries `timed_out: true`; a timeout never satisfies expectations.
- SENTINEL disconnects → task continues, results buffered and resubscribable, still mirrored to Redis.
- Duplicate request → deduped on `(pair_id, sha256(body))`.
- Redis down during mirror → `completed_unpersisted`; an unpersisted result cannot approve a gate.

The companion principle: **SENTINEL must hard-fail, never approve, when a required artifact (PLAN.md, a diff, a persisted verify result) is missing or unreadable.**

---

## 7. The Orchestration Cycle (zoomed in)

### 7.1 Flow graph

The Controller's flow graph (defined in `agentflow.rs`) is the routing table. Each node implements the PocketFlow `Node` trait: `prep()` (read store) → `exec()` (do work) → `post()` (write store + return `Action`).

```
nexus ──work_assigned──▶ forge_pair ──pr_opened──▶ sentinel
  ▲                        │   │                     │
  │                        │   └──planning_gate──▶ nexus   (spawn SENTINEL)
  │                        │   └──review_ready───▶ nexus   (spawn SENTINEL)
  │                        └──failed──┐                     │
  │                                   ▼                     │
  │                   review_approve ──▶ vessel ──deployed──▶ nexus (or lore)
  │                                   │                        │
  └─────────no_work / merge_blocked ◀─┘    ci_fix / conflicts ─▶ forge_pair
```

### 7.2 The paced poll loop

```
loop {
    match flow.run(&store) {
        Ok(action)  → log; 
        Err(e)      → log error (self-healing; never kill controller)
    }
    sleep(15s)
}
```

Idle and in-progress states pause the pass; the poll timer drives resumption.

### 7.3 Full lifecycle of a ticket

1. **Ingest** — NEXUS syncs GitHub issues into tickets.
2. **Dispatch** — assign to an idle FORGE slot; provision workspace; create empty chat.
3. **SessionStart bootstrap** — FORGE's chat fires the hook, which reads dispatch + phase and injects context.
4. **Planning** — FORGE writes `PLAN.md`, sets phase `planning`, halts.
5. **Planning gate** — the flow routes `planning_gate` to NEXUS, which spawns SENTINEL. SENTINEL reviews the plan and either `gate approve` (single-use GETDEL token) or reports issues.
6. **Building** — FORGE's `status set building` consumes the gate token; no token → transition rejected. FORGE implements.
7. **Testing** — FORGE runs tests.
8. **Review_ready** — FORGE opens the PR, records it, sets `review_ready`.
9. **PR review** — NEXUS spawns SENTINEL; SENTINEL reviews the diff, may delegate `verify` commands via A2A, and approves/rejects with inline comments.
10. **Merge** — approved → VESSEL watches CI, handles conflicts, squash-merges.
11. **Docs** — (optional) LORE updates changelog/ADRs.
12. **Teardown** — workspace deleted on merge.

### 7.4 Phase state machine (enforced by the harness)

```
planning ──[gate]──▶ building ──▶ testing ──▶ review_ready
    │                                  │            │
    ▼                                  ▼            ▼
 blocked (stuck)                   blocked      awaiting_human (review)
                                                  │
                                                  ▼
                                              merged (VESSEL)
```

Validation rules:
- First phase must be `planning` or `blocked` (can't skip the gate).
- Transitioning *from* a gated phase (`planning`) requires consuming a gate token via Redis `GETDEL` (single-use).
- Only the `sentinel` role may approve a gate (`authorize_gate_approver`).

### 7.5 Reconciliation & failure recovery

NEXUS `reconcile()` runs on every loop and detects & repairs:

1. Unmerged PRs not processed by VESSEL.
2. Orphaned tickets (assigned/in-progress but worker idle/missing).
3. Stale workers referencing dead tickets.
4. Completed-without-PR tickets.
5. Crashed workspaces (heartbeat stale > 90s).
6. Crashed chats (status `Error`).
7. Tickets stuck in `planning` without a SENTINEL chat.

Recovery is bounded: max 3 recovery attempts per ticket, then `awaiting_human` escalation.

---

## 8. Agent Harness & Orchestration System (zoomed in)

### 8.1 `openflows worker` — the coordination contract

The harness is the **single interface** between an LLM agent and the shared store. The agent never talks to Redis or Coder directly. Core commands:

```bash
openflows worker dispatch read            # task payload
openflows worker status get|set <phase>   # phase machine
openflows worker gate approve/status      # SENTINEL gate (single-use)
openflows worker pr opened|merged         # record PR
openflows worker handoff write            # CONTRACT.md handoff
openflows worker heartbeat start|stop     # liveness
openflows worker verify request|list|serve # A2A surface
```

Why the harness exists:
- **Typed validation** — malformed writes exit non-zero; agents read stderr and retry.
- **Gate enforcement** — impossible for an agent to skip the planning gate.
- **Loose coupling** — agents know the harness interface, not Coder/Redis internals.
- **Audit** — every state change is a typed, tenant-namespaced write.

### 8.2 PocketFlow core

`pocketflow-core` provides `SharedStore` (Redis), the `Node` trait, and the `Flow` graph runtime. The tenant-scoped store (`new_redis_with_tenant`) prefixes every key with `ns:{tenant}:`. The un-namespaced store + `raw_keys`/`raw_del` are used by admin commands to enumerate/purge tenants.

### 8.3 The agent team (registry-driven)

Roles are declared in the **agent registry** (schema v2). Render: FORGE (builder), SENTINEL (adversarial reviewer, plan mode), VESSEL (devops/merge), LORE (docs, optional), NEXUS (orchestrator). The registry is **defined and controlled entirely through the control plane** (see 3.6) — there is no registry file; the live `registry_json` is the sole source of truth, so `max_instances`, model, plan mode, skills, and MCP can be defined and retuned through the control panel with no restart.

Each role's **persona** lives in `orchestration/agent/agents/{role}.agent.md` and is embedded into the session via hooks, giving the agent complete identity and capability context.

The registry is the control point for `plan_mode`, `max_instances`, `model` hint, `skills`, and `mcp`. Adding **skills, MCP servers, and models is config-only**; adding a **role** requires a persona, a workspace template, and a flow-graph routing entry.

### 8.4 Agent execution engine — default Coder chat agent (confirmed)

**Decision (Q3):** OpenFlows standardizes on the **default Coder chat agent** (the Coder Chats API) as the execution engine for every role. CLI agents (Claude Code, Codex, aider, goose) launched inside workspaces are **not** an equal parallel path.

Rationale (recorded in full in the team findings doc):

- **Single unified protocol** — every role is driven over one Chats API; one integration to maintain. The harness (`openflows worker`) is the role-agnostic coordination contract that sits above the engine.
- **Security posture** — the LLM loop runs in the control plane via the AI Gateway; worker workspaces hold **no LLM keys**. A CLI agent needs an API key in the workspace, which would reintroduce a key-exfiltration surface and force a broader egress allowlist.
- **Strategy over framework** — Coder owns and upgrades the agent loop; OpenFlows owns coordination. This preserves the core asymmetry: *Coder governs WHERE agents run, OpenFlows governs HOW agents coordinate*.
- **Cost acknowledged** — the dependency is Coder's **Chats API stability** (marked experimental). Mitigations: all Chats API calls are isolated in the `coder-client` crate, and a verified Coder version is pinned (see §4).

The CLI-backend wiring that exists in the codebase (`CliBackend`, `DEFAULT_CLI`, `resolve_coder_module` — v1 fields) is treated as a **deprecated escape hatch**, not a supported default. Any future CLI-backed role must be added behind the same `SessionStart → harness` contract, with a stricter security variant (Coder-secret-injected scoped key, tighter hooks), and only as an explicit exception.

---

## 9. Git Authentication (zoomed in)

### 9.1 Model

OpenFlows deliberately does **not** hand shared GitHub PATs to workspaces. Git identity flows through **Coder's external authentication** (GitHub OAuth), bound per tenant.

### 9.2 Setup

1. **Bootstrap** verifies external auth is configured (`verify_external_auth_configured`).
2. **Tenant add** creates the Coder user + nexus workspace.
3. The operator completes the **GitHub OAuth link** for that tenant in the Coder dashboard.
4. The provisioned worker workspaces inherit scoped git credentials through Coder's workspace environment — never raw tokens in agent prompts.

### 9.3 How agents use it

- FORGE clones, checks out the pair branch, commits, and pushes to GitHub.
- SENTINEL posts inline PR comments and labels (`needs-revision` / `approved`).
- VESSEL reads CI status, merges (squash), and reports.
- NEXUS syncs issues and creates dispatch payloads.

The submission model keeps the human accountable: every agent action inherits the tenant's Coder/GitHub identity, and no LLM key or credential ever enters a worker workspace.

---

## 10. Adding Custom Skills, MCP, and Hooks (zoomed in)

The extension seam is the **live agent registry** (defined and managed entirely through the control plane — see 3.6) plus the `orchestration/plugin/` tree. No Rust changes are needed for the common cases.

### 10.1 Custom Skills

1. Create `orchestration/plugin/skills/my-skill/SKILL.md`.
2. Reference it in the **live registry** under the role's `skills` array (apply via the control panel / `openflows control set-registry`):
   ```json
   { "id": "forge", "skills": ["forge-coding", "my-skill", "shared-harness-protocol"] }
   ```
3. At workspace boot, the Provisioner materializes `SKILL.md` into `.agents/skills/<name>/`.
4. The Coder Agent discovers skills there and loads them via the `read_skill` tool.

Skills are scoped per role and ship alongside a `shared-harness-protocol` skill that teaches the coordination commands.

### 10.2 MCP servers (two ways, coexisting)

- **Per-role via registry:** add the `mcp` object; the Provisioner writes it as `.mcp.json` in the workspace.
  ```json
  { "mcp": { "my-server": { "command": "npx", "args": ["-y", "@my-org/server"] } } }
  ```
- **Centrally via Coder dashboard:** AI Settings → MCP Servers, with tool allow/deny lists and availability policies. These apply to all agent chats, not just OpenFlows.

Workspace `.mcp.json` and dashboard servers are merged.

### 10.3 Hooks (policy + bootstrap + lifecycle)

Hooks are role-specific shell scripts in `orchestration/plugin/hooks/{role}/`, wired into the agent's settings. A representative set:

| Event | Hook | Purpose |
|-------|------|---------|
| `SessionStart` | `session_start.sh` | Bootstrap context (dispatch, phase, workflow) — output becomes session context |
| `PreToolUse` | `pre_bash_guard.sh` | Block dangerous bash (exit 2 blocks) |
| `PreToolUse` | `pre_write_check.sh` | Prevent writes outside workspace |
| `PostToolUse` | `post_write_lint.sh` | Auto-lint after edits |
| `PreCompact` | `pre_compact_handoff.sh` | Persist state before compaction |
| `Stop` | `stop_require_artifact.sh` | Refuse stop until artifact/PR exists |
| `SubagentStop` | `subagent_stop.sh` | Cleanup on subagent exit |

The **SessionStart hook is the bootstrap entrypoint**: chats are created empty, so the hook output is the agent's first (and resume) view of the world — accurate because it reads live dispatch/phase from Redis, and polyglot because it's role-customizable. Policy hooks (`PreToolUse`, `PreWrite`, `Stop`) enforce constraints *before* the agent can act destructively.

### 10.4 Adding a new role

The one extension that needs code: add the registry entry, write the persona, add the workspace template, and add flow-routing in the Controller.

---

## 11. Controller ↔ Human Feedback — Man-in-the-Middle Escalation (zoomed in)

OpenFlows is autonomous but **not unsupervised**. Humans stay in the loop at the decision points that matter, and the "man-in-the-middle" control is the `awaiting_human` escalation.

### 11.1 When a human is pulled in

A ticket transitions to `AwaitingHuman` when:

- An agent hard-fails on a required artifact (e.g., SENTINEL can't read `PLAN.md`).
- Recovery attempts are **exhausted** (max 3) — reconciliation stops retrying and escalates.
- The task requires a security decision, ambiguous spec judgement, or an architectural call.
- VESSEL hits a merge block or conflict it can't resolve.

The Controller's `reconcile()` never lets `awaiting_human` deadlock the fleet: an escalated ticket is parked (not repeatedly retried), and the human is notified.

### 11.2 Execution path

`NexusNode::mark_ticket_awaiting_human(...)`:
1. Sets the ticket status to `AwaitingHuman { worker_id, reason, attempts }` and writes it back to Redis.
2. Calls `notify_awaiting_human(...)`.
3. The `NotificationService` fires **fire-and-forget** messages to configured channels.

### 11.3 Notification channels

From `crates/notifier`:

| Channel | Env vars |
|---------|----------|
| Slack | `SLACK_WEBHOOK_URL` |
| Discord | `DISCORD_WEBHOOK_URL` |
| WhatsApp (Twilio) | `WHATSAPP_ACCOUNT_SID`/`API_KEY`, `WHATSAPP_AUTH_TOKEN`, `WHATSAPP_FROM_PHONE`, `WHATSAPP_TO_PHONE` |

Notifications are:
- **Batched** — max 1 per channel per ticket per 5 minutes (cooldown), preventing alert floods.
- **Operationally safe** — fire-and-forget; a failing channel logs but never fails the orchestration loop.
- **Actionable** — include ticket ID, role, reason, workspace link, and GitHub link so a human can jump straight to the decision.

### 11.4 The human acts

A human resolves the escalation through the standard surfaces:
- **Comment / close the GitHub issue** — the next reconcile pass re-ingests or closes the ticket.
- **`openflows tenant clean`** — resets stale `awaiting_human`/`failed` tickets back to `Open` and clears worker/recovery counters for a clean restart of the loop.
- **Answer directly in the Coder workspace/chat** — the agent resumes with the human's guidance.

The gate token model reinforces the human check: even with no human present, SENTINEL literally cannot approve a gate it lacks evidence for, and the system would rather block a ticket than merge unverified work.

---

## 12. Deployment (end-to-end)

### 12.1 Infrastructure topology

| Component | Where it runs | Why |
|-----------|---------------|-----|
| Coder server | Host VM / k8s | Control plane (identity, model gateway, API) |
| PostgreSQL | Container (coder-db) | Coder's database |
| Redis | Container | OpenFlows SharedStore (single source of truth) |
| Controller + A2A relay | `openflows-nexus` Coder workspace | Long-lived, trusted, needs Coder API access |
| Worker agents | Ephemeral Coder workspaces | Governed, isolated, disposable |

### 12.2 The reference deployment (`docker-compose.yml`)

The shipped compose file stands up the whole control plane:

- `redis:7-alpine` — SharedStore (`--appendonly yes` for durability).
- `postgres:16-alpine` — Coder DB.
- `ghcr.io/coder/coder` — Coder server with:
  - `CODER_PG_CONNECTION_URL` → postgres
  - `CODER_ACCESS_URL` / `CODER_HTTP_ADDRESS`
  - `CODER_PROVISIONER_DAEMONS=1` + `CODER_PROVISIONER_DAEMON_TYPE=docker` + `DOCKER_HOST` (provisioner-in-process using the docker socket)
  - GitHub signups enabled
  - a temporary bind-mount of `./.dev-binaries` for local binaries during testing

The operator then runs `openflows bootstrap`, `openflows tenant add owner/repo`, and completes GitHub OAuth.

### 12.3 Image build (`Dockerfile`)

Two-stage build:

1. **Builder** (`rust:1.88-bookworm`) — compiles release binaries (`openflows`, `openflows-doctor`).
2. **Runtime** (`debian:bookworm-slim`) — installs `ca-certificates`, `curl`, `git`, `nodejs`, `npm`, and the Claude Code CLI (`@anthropic-ai/claude-code`); creates a non-root `openflows` user; installs the binaries; sets a process healthcheck.

> **Decision (Q3) reconciliation:** the primary execution engine is the **default Coder chat agent** (see 8.4); the bundled Claude Code CLI is retained **only as the deprecated CLI-backend escape hatch** for future role-level exceptions — it is not part of the default path.

Entry: `ENTRYPOINT ["openflows"]`.

### 12.4 Bootstrap sequence for a fresh environment

1. Stand up Coder + Redis + Postgres (compose).
2. `openflows bootstrap` — create admin, templates, verify LLM + GitHub auth; initializes the **agent registry** in the control plane to the default team baseline if none exists (see 3.6).
3. `openflows tenant add owner/repo` — creates the tenant user + nexus workspace.
4. Complete GitHub OAuth for that tenant in the Coder dashboard (or via the OpenFlows control panel).
5. The Controller auto-starts in the nexus workspace (startup script) and begins the poll loop.
6. Open a GitHub issue → the team picks it up and runs it to a merged PR.
7. Configure notify channels (Slack/Discord/WhatsApp) for `awaiting_human` escalations.
8. Operator manages the fleet through the **OpenFlows control panel** (registry, halt/target/continue, live status) — see 3.5–3.7.

### 12.5 Health & lifecycle

- Controller self-heals through the error/retry poll loop.
- Harness heartbeats detect stale workers (stale after 90s).
- Workspaces are torn down after merge.
- `openflows doctor` diagnoses Coder integration health.

---

## 13. Security Model (summary)

| Property | Mechanism |
|----------|-----------|
| Key exfiltration | No LLM/git keys in worker workspaces; AI Gateway in control plane |
| Identity | Per-user Coder SSO; per-tenant scoped session tokens |
| Network isolation | Workspace egress allowlist (control plane + GitHub + Redis) |
| Command control | Role `plan_mode` + `PreToolUse`/`PreWrite`/`Stop` hooks + A2A allowlist |
| Audit | Coder audit log + typed SharedStore events + `audit:a2a:*` |
| Multi-tenancy | Coder RBAC + Redis `ns:{tenant}:` prefixes |
| Review integrity | Workspace isolation; SENTINEL delegates verification, never mutates FORGE's tree |
| Gate integrity | Single-use Redis GETDEL tokens; only SENTINEL approves |

---

## 14. Related Documents

- `docs/architecture/openflows-controller.md` — internal architecture of the OpenFlows Controller (Subsystem 01 deep-dive).
- `docs/architecture/openflows-worker-workspace.md` — worker workspaces, SessionStart bootstrap, and executor setup (Subsystem 03 deep-dive).
- `docs/architecture/openflows-a2a-relay.md` — full A2A JSON-RPC/SSE protocol and the receiver/executor surface (Subsystem 04 deep-dive).
- `docs/architecture/openflows-redis-shared-store.md` — the shared Redis infrastructure deep-dive (Channel B, the single source of truth).
- `docs/architecture/openflows-system-architecture.md` §10 — skills, MCP, models, roles (extension points).
- `docs/architecture/openflows-system-architecture.md` §13 — AI governance and network policy.
- `docs/architecture/openflows-system-architecture.md` §5.4 — multi-tenant model and Redis namespacing.
- `quick_start.md` — setup, startup, troubleshooting.
