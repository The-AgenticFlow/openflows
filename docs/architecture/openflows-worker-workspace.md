# OpenFlows Worker Workspace — Internal Architecture

**Document type:** Internal architecture (deep-dive)
**Scope:** Subsystem 03 of the OpenFlows system — the ephemeral worker workspaces (FORGE, SENTINEL, VESSEL, LORE) that the Controller provisions from Coder templates.
**Companion docs:** `openflows-system-architecture.md` (system-wide, authoritative; see §5.4 for multi-tenant isolation and §13 for network policy), `openflows-controller.md` (the process that provisions them), `openflows-a2a-relay.md` (the A2A executor surface FORGE hosts).

---

## 1. Role & Responsibilities

Each worker workspace is a **short-lived, disposable Coder workspace** built from a role-specific template. It is the *execution seat* for one agent on one ticket. Worker workspaces are deliberately thin and deliberately isolated:

- **They hold no credentials.** No LLM API keys (the AI Gateway lives in the control plane) and no raw GitHub PATs (git identity flows through Coder's external GitHub OAuth).
- **They hold no agent framework.** The AI loop runs in the control plane (Coder Chats API); the workspace only hosts the `openflows` binary (whose `worker` command surface is the coordination CLI) and the repo checkout.
- **They are the only place that talks to Redis** — via the `openflows worker` command surface (Harness A), never directly.
- **They host the Agent Harness (Harness B)** — the agent-facing plugin surface (`orchestration/`: hooks, commands, skills, MCP, personas, standards) that drives the Worker surface of Harness A. **Do not conflate the two harnesses**: A is the compiled command surface (the `openflows worker` CLI: Redis client + gate + A2A executor); B is the discoverable filesystem package the agent calls, and it does **not** touch Redis on its own (see §5.6). Both ship in the same `openflows` binary and the `agent-harness` package — there is one executable.
- **The agent bootstraps hook-first.** On startup the agent **runs the role's `SessionStart` hook** and its stdout becomes the opening context (live assignment, phase, workflow), then the agent is **tool- and hook-capable** through the Agent Harness (`commands/*` + policy hooks) — the Claude Code / Codex model. The Controller provisions and binds the workspace, but the assignment context is **read from live Redis by the hook**, not baked into a chat first message (see §5.5).

A worker workspace's responsibilities (per role) are:

- **FORGE** — plan, implement, test, and open a PR; also hosts the A2A verification executor daemon.
- **SENTINEL** — adversarially review FORGE's plan and PR; approve gates; may delegate verification to FORGE via A2A.
- **VESSEL** — watch CI, resolve merge conflicts, squash-merge approved PRs.
- **LORE** (optional) — update changelog/ADRs after merge.

The core asymmetry holds here as everywhere: **Coder governs WHERE agents run (the workspace), OpenFlows governs HOW they coordinate (the `openflows worker` coordination contract, supervised by the Controller).**

---

## 2. Anatomy of a worker workspace

```
┌──────────────────────────  WORKSPACE  ────────────────────────────┐
│  Coder Agent (LLM chat session, driven by control plane)          │
│    · runs SessionStart hook at boot → stdout = initial context     │
│    · reads skills via `read_skill` from .agents/skills/           │
│    · calls MCP tools (merged .mcp.json + dashboard)               │
│    · calls Agent Harness commands + policy hooks (hook+tool capable)│
│                                                                   │
│  openflows worker CLI   (Harness A — the ONLY Redis client)       │
│    dispatch · status · gate · pr · handoff · review · merge       │
│    plan · heartbeat · verify (request/serve/list)                 │
│                                                                   │
│  Agent Harness (B) — agent-facing plugin surface                  │
│    hooks (role policy + bootstrap)  ~/.openflows/hooks/           │
│    [target] commands/ plugin filesystem  ~/.openflows/commands/   │
│    skills (.agents/skills/<name>/SKILL.md)                        │
│    .mcp.json (per-role) + AGENTS.md (persona)                     │
│    standards (CODING.md · SECURITY.md · REVIEW.md)                │
│  git (clone/checkout/push) + repo checkout on pair branch         │
│                                                                   │
│  [FORGE only] verify serve daemon  (A2A executor, supervised)     │
│                                                                   │
│  NO API keys · NO LLM keys · restricted egress                    │
└───────────────────────────────────────────────────────────────────┘
```

### Required environment (injected by the workspace template)

`openflows worker` is fail-fast on these (`binary/src/worker.rs:207` `require_env`):

| Variable | Purpose |
|----------|---------|
| `REDIS_URL` | Redis SharedStore connection |
| `OPENFLOWS_TENANT` | Tenant identifier (namespaces every Redis key) |
| `OPENFLOWS_TICKET` | Current ticket ID (e.g. `T-42`) — doubles as the A2A `pair_id` |
| `OPENFLOWS_ROLE` | Role (forge / sentinel / vessel / lore) — drives gate authorization |
| `CODER_WORKSPACE_ID` | Workspace identifier, used in heartbeat + A2A audit trail |
| `A2A_RELAY_ADDR` | A2A relay network address (`openflows-nexus:3000`); FORGE executor + SENTINEL client |

---

## 3. Workspace boot sequence

On provision, the Coder template's **startup script** runs a fixed sequence (§5.1 of the system doc):

1. **Install Harness A (the `openflows` binary)** — mandatory; startup fails if it is missing or not on `PATH`. It is **pulled from a GitHub Release at provision time**, not baked into a base image (except the local dev override). See §3.1 for the full release/install model.
2. **Install the Agent Harness (B)** — pulled/provisioned as a first-class, versioned artifact and unpacked into the workspace (today the Controller's `Provisioner` copies its files in via `~/.openflows/`; the target is a packaged install with a pinned version). Under the target plugin-FS contract (see §7) this yields a self-describing `~/.openflows/` tree the Coder agent calls explicitly. See §3.1a for the release/install model.
3. **Wire environment** — set the ticket/role/tenant/Redis/A2A variables above, then register to obtain a runtime session.
4. **Install the role asset bundle** — a hosted workspace runs `openflows-harness artifacts fetch` and installs the persona, skills, commands, standards and hooks from the bundle the Manager serves for this release. The archive is verified against its sha256 before anything is written and pinned to the bundle the harness was built with, so a corrupt archive or a Manager serving a different bundle fails startup instead of producing a half-configured agent. Local mode reads the tenant artifacts volume instead. See §3.3.
5. **Acquire the repository** — the worker copies the repo from a shared **golden mount clone** (rather than a per-spawn network clone) and checks out the pair branch (FORGE) or the target for review (SENTINEL). See §3.2.
6. **Start the heartbeat** daemon (writes a Redis key every 30s with a 120s TTL — see §6).
7. **Verify the Agent Harness (B) surface** — skills, MCP, standards, and persona are installed and discoverable (see §4).
8. **FORGE only: start the A2A executor daemon** — Harness A `openflows worker verify serve` (see §5.4).

Under the hood, file materialization is performed by the **Provisioner** (`crates/provisioner`) over a `WorkspaceTransport` (`crates/provisioner/src/transport.rs`) that executes shell commands inside the workspace via the Coder client — because workspaces don't share a filesystem, the Controller copies artifacts *into* the workspace rather than mounting a volume.

### 3.1 The Harness A release & install model

The worker installs **Harness A** (the `openflows` binary, whose `worker` command surface is the coordination CLI) by runtime download at boot (`crates/coder-client/templates/openflows-forge/main.tf:85-123`, mirrored in the sentinel/vessel/lore templates):

1. **Local dev override** — if `/opt/openflows-dev/openflows` is bind-mounted (local testing), copy it and skip the download.
2. **Download** — `curl -fsSL --retry 3` from `…/releases/download/{harness_version}/openflows-{harness_version}-x86_64-unknown-linux-musl.tar.gz` (the single `openflows` release tarball), where `harness_version` defaults to `harness-edge` (latest main-branch build) or a pinned tag/version.
3. **Extract + install** — untar to `/tmp`, locate the `openflows-*` dir, `sudo mv` the `openflows` binary to `/usr/local/bin/openflows`, `chmod +x`.
4. **Fail loudly** — retry 3x; if still not executable, log `FATAL` and `exit 1` so the workspace never comes up uncoordinated.

Because the asset bundle is now pinned to the bundle the harness was built with (§3.3), `harness_version` has to name the release the Manager runs. A hosted deployment should set it to a release tag; the `harness-edge` default only lines up while the Manager also tracks `main`.

**Release concept — one binary everywhere.** There is **one** `openflows` binary that contains both the Controller (`openflows run`) and the Worker surface (`openflows worker …`); the four roles (FORGE, SENTINEL, VESSEL, LORE) are not separate binaries — they are the same worker command surface driven by different `OPENFLOWS_ROLE`/`OPENFLOWS_TICKET` env plus the role-scoped Agent Harness (B) content (hooks/skills/persona). The release therefore publishes two independently releasable pieces:

| Artifact | Contains | Deployed to |
|----------|----------|-------------|
| `openflows-<ver>-<target>.tar.gz` | The single binary (`openflows`, `openflows-doctor`) | Control plane (`openflows run`) **and** every worker workspace (`openflows worker …`), **installed** at boot (§3.1) |
| `agent-harness-<ver>-<target>.tar.gz` | **Harness B** — hooks · commands · skills · mcp · personas · standards (the §7 plugin tree) | Every worker workspace, **installed** at boot via `openflows install` (§3.1a) |

Both the Controller and Harness A share **one version number and one binary** (no divergent versions, no separate artifact to reconcile). The asset name is exact and versioned (`openflows-<ver>-<target>.tar.gz`) so unattended startup scripts can `curl` a stable, predictable URL and (with the shipped `.sha256`) verify integrity before installing. The Agent Harness (B) is its **own independently versioned, installed artifact** (see §3.1a) — which is exactly why the two "harness" names stay distinct even though Harness A is no longer a separate executable.

> **Alignment note:** the templates reference the worker asset as `openflows-{harness_version}-x86_64-unknown-linux-musl.tar.gz`, matching the single `openflows` release tarball produced by `release-assets.yml`, so the worker and the controller now resolve the same artifact name.

### 3.1a The Agent Harness (B) release & install model

The Agent Harness (Harness B) is **installed and provisioned just like Harness A** — not copied in ad hoc. The two harnesses are deliberately symmetric: Harness A ships inside the single `openflows` binary (the `openflows worker` command surface), Harness B is the installed *plugin package*, and both must be present for the workspace to coordinate. There are two coexisting install transports today, converging on one in the target:

1. **Controller-side provisioning (today).** The `Provisioner` (`crates/provisioner/src/provision.rs:27`) copies the Agent Harness files *into* the workspace over `WorkspaceTransport` — skills → `.agents/skills/`, `.mcp.json`, standards, personas/`AGENTS.md`. Because workspaces share no filesystem, the Controller copies rather than mounts (see the transport note at the end of §3).
2. **Workspace-side package install (target).** The Agent Harness ships as a **versioned, independently released artifact** (its own project), pulled at boot and unpacked to `~/.openflows/` by `openflows install` (§7) — the plugin-tree analogue of Harness A's download-and-install. This gives B the same properties A has: a pinned/`-edge` version, a predictably named asset with integrity verification, and a **fail-loud** rule (the workspace never boots with a partially installed Agent Harness).

**Versioning.** Harness A and the Controller share one version number and one binary; the Agent Harness (B) is **versioned independently** because it is its own project. The workspace pins/validates whichever `agent-harness` revision it resolved, mirroring how Harness A validates its `harness_version` — so a worker is never held to an outdated or partial Agent Harness.

> This is the part that previously read as "materialize content": the Agent Harness is **not** loose files to be sprinkled in — it is a release artifact to be installed, exactly like the `openflows` binary. The docs, the release pipeline, and the boot must treat B as a first-class installed component.

### 3.2 Repo acquisition: the golden mount clone

Every FORGE (or SENTINEL) spawn currently runs its own `git clone`/`pull` into its private docker volume (`openflows-forge/main.tf:213-225`). A full clone on every ephemeral spawn is slow and wasteful. The target design removes the network clone from the per-workspace path by reusing the existing tenant-shared read-only volume:

**Hosted mode does not take this path.** A hosted workspace has no shared volume
to seed: it installs the artifact bundle from the Manager and clones the
repository with a short-lived credential the Manager brokers for it. The golden
clone stays a local-mode optimisation, which is what keeps read-write-many
storage optional and keeps no copy of a customer repository outside the tenant's
own workspaces.

**Topology (one network fetch, many local copies, local mode).**

```
                NEXUS (control plane, has creds + network)  [per tenant]
   golden repo on shared volume  /home/coder/.openflows/artifacts/repo/
   ── git fetch/reset  (refresh-before-copy) ──────────────────────┐
                                                                    ▼
        openflows-artifacts-<tenant>  (read-only shared volume)
                                                                    │
        ┌───────────────────────────────┬──────────────────────────┘
        ▼                               ▼
   FORGE workspace                 SENTINEL workspace
   1. cp -a golden → own volume     1. cp -a golden → own volume
   2. refresh to latest (git fetch) 2. refresh to latest (git fetch)
   3. checkout pair branch          3. checkout target branch
   4. begin work                    4. begin work
```

1. **Golden clone on the shared volume.** NEXUS maintains a fresh checkout of the target repo at `/home/coder/.openflows/artifacts/repo/` on the tenant-scoped `openflows-artifacts-<tenant>` volume it already owns (`openflows-nexus/main.tf:164-181`; NEXUS already pulls/clones the repo at startup, `openflows-nexus/main.tf:117-122`). NEXUS holds the GitHub credentials and network egress, so the expensive fetch happens **once**, in the control plane.
2. **Refresh before copy.** When a workspace that needs the repo is about to be provisioned (or on a short cadence), NEXUS runs `git fetch --prune` + `reset --hard origin/<target>` on the golden clone so the snapshot is current **before** any worker takes a copy. This is the "updates are pulled before any workspace gets its copy" guarantee.
3. **Per-workspace copy (not clone), then refresh, then checkout.** The worker startup script **copies** the golden tree from the read-only shared volume into its own docker volume: `cp -a /home/coder/.openflows/artifacts/repo/. /home/coder/workspace/`. This is a local filesystem copy — no network, no git creds in the worker, orders of magnitude cheaper than a per-spawn clone. **Before any checkout,** the worker refreshes its local copy to the latest (`git fetch` — see the next block), and only then checks out the pair branch (FORGE) or review target (SENTINEL).

**Why this preserves the security/isolation guarantees:**

- **Per-tenant scoping.** The golden clone, its shared volume, and all copies are scoped per tenant: the volume is `openflows-artifacts-<tenant>` and lives under NEXUS's tenant workspace, so one tenant's repo snapshot is never visible to another tenant's workspaces. This composes with the `ns:{tenant}:` Redis keyspace for full tenant isolation.
- **Read-only source.** The shared volume is mounted `read_only = true` (`openflows-forge/main.tf:296-301`), so a worker can never mutate the golden tree; NEXUS is the sole writer.
- **Independent physical copies.** Each worker still gets its **own** tree on its **own** docker volume, so FORGE and SENTINEL never share a writable filesystem — the review-integrity property (SENTINEL cannot touch FORGE's tree) is intact. Golden-copying is purely a *seed*; it is not a live shared mount for writes.
- **No credentials in workers, local mode.** The copy is offline; the only git/network dependency stays in NEXUS. Hosted workers do hold a short-lived brokered credential, because they clone directly.

**Freshness before work (no stale versions):**

- **The golden tree is refreshed before any copy** — NEXUS runs `git fetch --prune` + `reset --hard origin/<target>` on the golden clone before a worker takes its snapshot, so the snapshot reflects the current target, not a stale one.
- **The workspace refreshes to the latest before checking out** — after `cp -a`, the worker runs a small `git fetch` on its local copy **first**, then checks out the current target/pair branch from the fetched head. The copy is the cheap seed; the **refresh always precedes the checkout** so the worker plans and builds against the up-to-date tree, never a stale checkout. (Ordering in the workspace: copy → refresh → checkout → begin work.)
- The same freshness principle applies to the Harness A binary (§3.1): a workspace pulling a pinned `harness_version` verifies it resolves to the expected version (and `harness-edge` always tracks the latest), so the worker isn't held to an outdated binary.

**Caveats to design around:**

- **Branch checkout is per-copy.** The pair branch or review target is checked out in each copy, so two workspaces branching from the same golden snapshot don't collide.
- **Ordering is copy → refresh → checkout → work.** The worker's refresh is a small `git fetch` on its local copy (still cheap relative to a full clone); it must run **before the branch checkout** and before any plan/build step so work never begins on stale content.
- **Disk cost.** Each spawn holds a full physical copy on its volume; this trades network time for disk, which is the intended trade (cheap, local, offline-seedable).

---

### 3.3 Role assets: one bundle, two delivery paths

The role assets are compiled into the `workspace-assets` crate, so the controller and the Manager describe the same release from one definition. A hosted workspace fetches them from the Manager: `GET /api/v1/runtime/artifacts` returns a deterministic gzipped tar and reports its sha256 in `x-artifact-digest`, while `GET /api/v1/runtime/artifacts/{digest}` returns that bundle only. The fetch runs before the persona is installed, so a worker that starts before the Nexus workspace has published anything still boots with a complete bundle. The tenant artifacts volume keeps one job, the golden repository seed in §3.2.

## 4. Provisioning: installing the Agent Harness (B)

The **Provisioner** is the Controller-side install transport for the Agent Harness (B) — the way B's files get into the workspace today, in parallel with Harness A's binary install (see §3.1a for the relationship and the workspace-side package-install target). Everything this section installs is the **Agent Harness (B)** — the agent-facing plugin surface. `Provisioner::provision_role` (`provision.rs:27`) reads the live agent registry for the role and writes into the workspace:

1. **Skills** → `.agents/skills/<name>/SKILL.md` for each skill in the registry entry (`provision.rs:56`). The Coder Agent discovers and loads these via the `read_skill` tool. A permission-denied on the skills dir is logged and skipped — skills are optional, not fatal.
2. **MCP** → `.mcp.json` from the role's `mcp` config (`provision.rs:79`). This is merged client-side with centrally-managed servers from the Coder dashboard.
3. **Standards** → `CODING.md`, `SECURITY.md`, `REVIEW.md` at workspace root (`provision.rs:89`).
4. **Persona** → the role's `<role>.agent.md` is copied as both `<role>.agent.md` **and `AGENTS.md`** (`provision.rs:115-132`). Coder's agents read `AGENTS.md` from the working directory (and `~/.coder/AGENTS.md`) and inject it into the system prompt for every conversation **server-side** — so the persona persists across chats rather than being bundled into a fragile first request.

These are the pieces that, in the §7 target, come from the **Agent Harness package** installed into `~/.openflows/` — keeping the binary (Harness A) and the agent-facing content (Harness B) each installed, versioned, and owned independently.

---

## 5. The agent-side stack

### 5.1 Harness A — `openflows worker`, the ONLY Redis client

Harness A is the worker command surface of the `openflows` binary (`binary/src/worker.rs`) that is the single interface between the agent and the shared store. The agent never calls Redis or Coder directly — the Agent Harness (B) surface it sees wraps these commands and funnels all durable actions through this surface (see §5.6). Command surface:

```bash
openflows worker dispatch read              # task payload
openflows worker status get|set <phase>     # phase state machine
openflows worker gate approve|status        # SENTINEL single-use gate
openflows worker pr get|opened              # record PR
openflows worker handoff write              # CONTRACT.md handoff
openflows worker review submit              # SENTINEL verdict
openflows worker merge done                 # VESSEL merge record
openflows worker plan read|write            # PLAN.md in Redis
openflows worker heartbeat start|stop       # liveness
openflows worker verify request|serve|list  # A2A surface
```

Design guarantees (all in `crates/openflows-harness/src/store.rs`):

- **Typed validation** — every write is serde-validated; malformed writes exit non-zero and the agent reads stderr and retries. `redis-cli` is disallowed.
- **Gate enforcement** — `status_set` enforces the gated phase machine: a fresh ticket must enter via `planning` (or `blocked`); leaving `planning` requires consuming a SENTINEL approval via atomic Redis **`GETDEL`** (single-use). FORGE cannot approve its own plan (`authorize_gate_approver` rejects any non-SENTINEL role, `store.rs:96`).
- **Tenant awareness** — every key is `ns:{tenant}:...` (`store.rs:146`).

The phase state machine (enforced by the harness):

```
planning ──[gate]──▶ building ──▶ testing ──▶ review_ready
    │                                  │            │
    ▼                                  ▼            ▼
 blocked (stuck)                   blocked      awaiting_human (review)
                                                  │
                                                  ▼
                                              merged (VESSEL)
```

### 5.2 Agent Harness (B) hooks — the built-in startup + tool/policy surface

Hooks are a component of the **Agent Harness (B)** and they are the **primary startup behaviour** of a worker agent — the way the agent comes alive, modelled on advanced coding agents (Claude Code, Codex): on startup the agent **runs the startup hook**, reads its output as its initial context, and from then on is capable of **tool calls and further hook invocations** — all surfaced by the Agent Harness.

The **`SessionStart` hook is the built-in bootstrap**, not the chat's first message:

- **On boot (`SessionStart`), the agent runs `session_start.sh`** and its stdout becomes the agent's opening context: live assignment (`dispatch read`), current phase + history (`status get`), the workflow to follow, and the harness command reference (see the `session_start.sh` body, `orchestration/plugin/hooks/forge/session_start.sh`, which declares itself the *"SOLE entrypoint"* and reads live Redis through the harness). This is hook-driven startup as **built-in behaviour**, identical in spirit to how Claude Code's `SessionStart` hook seeds a session — not a Controller-authored chat message.
- **After boot the agent is tool-capable.** It calls the Agent Harness's `commands/*` (tool calls over Harness A) and its policy hooks (`pre_bash_guard.sh`, `pre_write_check.sh`, `post_write_lint.sh`, `stop_require_artifact.sh`) before/after risky actions — the same lifecycle advanced coding agents expose. The role-scoped set:

| Event | FORGE | SENTINEL | Purpose / Blocks? |
|-------|-------|----------|-------------------|
| `SessionStart` | `session_start.sh` | `session_start.sh` | **Built-in bootstrap**: run at startup; stdout becomes the agent's opening context (assignment/phase/workflow) |
| `PreToolUse(Bash)` | `pre_bash_guard.sh` | `pre_bash_readonly_guard.sh` | Block destructive/out-of-policy bash (**Yes, exit 2**) |
| `PreToolUse(Write)` | `pre_write_check.sh` | — | Prevent writes outside workspace |
| `PostToolUse` | `post_write_lint.sh` | `post_write_validate.sh` | Auto-lint / validate after edits (No) |
| `PreCompact` | `pre_compact_handoff.sh` | — | Persist state before compaction (No) |
| `SubagentStart/Stop` | `subagent_start.sh` / `subagent_stop.sh` | same | Subagent lifecycle cleanup |
| `Stop` | `stop_require_artifact.sh` | `stop_require_eval.sh` | Refuse stop until artifact/PR exists (**Yes, exit 2**) |

**How this is delivered as built-in behaviour, engine-agnostically.** The template installs the role's hooks to `~/.openflows/hooks/` and registers the event map in `hooks.json` (`orchestration/plugin/hooks/hooks.json`). On the target contract (§7), `openflows install` writes this self-describing `~/.openflows/` tree, and `AGENTS.md` teaches the agent that its startup hook and tools live there. The startup behaviour then does **not** depend on a particular engine's internal event system or on a Controller-authored first message — any agent that can execute a script and read its output gets the same bootstrap, and the same callable hook/tool surface. (Today a Claude-Code/CLI hook map in `~/.claude/settings.json` wires these for the CLI path; the target contract makes the `~/.openflows/` tree the single, engine-independent source.)

> **This supersedes the "first-message bootstrap" model.** The chat's first message is at most a minimal seed/pointer; the load-bearing context — live assignment, phase, workflow, command reference — comes from the `SessionStart` hook the agent itself runs, exactly as advanced coding agents boot. See §5.5 for the reconciled bootstrap flow.

### 5.3 Skills, MCP, standards, persona

- **Skills** — `.agents/skills/<name>/SKILL.md`, role-scoped, discovered via `read_skill`. `shared-harness-protocol` teaches the coordination commands.
- **MCP** — workspace `.mcp.json` (per-role) is merged with Coder-dashboard central servers.
- **Standards + persona** — `CODING.md`/`SECURITY.md`/`REVIEW.md` and the `AGENTS.md` persona delivered server-side.

### 5.4 The A2A executor (FORGE only)

FORGE workspaces additionally run `openflows worker verify serve` (`store.rs:621`) as a supervised background daemon. This is the A2A **executor** side of delegated verification (see `docs/architecture/openflows-a2a-relay.md`):

1. Health-checks the nexus relay, logs "✓ Forge verify executor ready".
2. Polls `tasks/claim` for its pair (only the `forge` role is permitted to claim).
3. On claim, runs the command via the sandbox (`executor.rs`) with **process-group isolation, timeout enforcement, and bounded (10 KB tail) output capture**.
4. Streams progress (`tasks/push_progress`), monitors cancellation (`tasks/cancel` → atomic cancel token), and submits the terminal result (`tasks/complete`), which the relay mirrors to Redis.
5. Loops until SIGTERM; a crash is restarted by the supervisor.

### 5.5 How the agent bootstraps: hook-driven startup, tool+hook capable

The worker agent starts just like an advanced coding agent (Claude Code, Codex): it **runs a startup hook at session start**, reads that hook's output as its opening context, and is then **capable of tool calls and further hook invocations** — all provided by the Agent Harness (B). This is the primary, engine-agnostic bootstrap; it does **not** lean on the Controller stuffing context into the chat's first message.

**The startup sequence (built-in behaviour, not a Controller-authored prompt):**

1. **Boot** — the worker installs Harness A (binary) and the Agent Harness (B) package (§3/§3.1a), and starts with `AGENTS.md` + `hooks.json` teaching where its startup hook and tools live.
2. **SessionStart hook runs automatically** — the agent executes `session_start.sh` (its role's entrypoint) and its stdout becomes the opening context: live `dispatch read`, current phase + history via `status get`, the workflow, and the `openflows worker` command reference. This is *by design* the sole entrypoint (`orchestration/plugin/hooks/forge/session_start.sh`), reading live coordination state through Harness A — not a frozen prompt.
3. **The agent becomes tool+hook capable** — it calls the Agent Harness `commands/*` (tool calls over Harness A) and invokes its policy hooks (`pre_bash_guard.sh`, `pre_write_check.sh`, `post_write_lint.sh`, `stop_require_artifact.sh`) around risky actions, exactly as Claude Code / Codex expose a lifecycle. See §5.2.
4. **First message is only a seed/pointer** — the chat's first message at most points the agent at its harness ("your startup hook is at `~/.openflows/hooks`; run it"). It is deliberately **not** the source of authority: assignment, phase, and workflow all come from live Redis via the startup hook, so state can never go stale in a hardcoded prompt.

**Today vs. target.** Today there are two wiring paths: the default Coder chat agent gets context from a first message + server-side `AGENTS.md`, while a Claude-Code/CLI fallback fires the hook engine via `~/.claude/settings.json`. The event maps are engine-specific and the hook path is only honoured by the CLI engine. **The target (§7) unifies them**: `openflows install` exposes hooks and commands under `~/.openflows/` as **discoverable, callable tools**, so the *default* Coder agent bootstraps by running `session_start.sh` itself and calls the other hooks as tools — the same built-in behavior, on every engine.

> **Net:** hook-driven startup is the real model. "First-message bootstrap with empty hooks" is the legacy fallback. The agent reads and runs its startup hook at boot, and is then fully tool- and hook-capable through the Agent Harness — which is the whole point of having an Agent Harness at all.

### 5.6 The two harnesses — and the Controller ↔ harness coordination contract

OpenFlows has **two** distinct things that both carry the word "harness". They are different artifacts with different owners, and this section exists so they are never conflated.

| | **Harness A — `openflows worker` (in the `openflows` binary)** | **Harness B — the Agent Harness (the plugin package)** |
|---|---|---|
| **What it is** | A compiled **command surface** — the typed Redis client + gate + A2A executor | A **filesystem plugin package** — the agent-facing surface (hooks, commands, skills, MCP, personas, standards) |
| **Crate / source** | `crates/openflows-harness` (lib) + `binary/src/worker.rs` → `openflows worker` | `orchestration/` (`plugin/` + `agent/`) — an **independent project** |
| **Touches Redis?** | **Yes — it is the ONLY Redis client in a workspace** | No — it reads/writes coordination state *through* the worker surface |
| **Consumed by** | The workspace boot, the A2A relay, and the Controller (for `gate`) | The agent itself, via `~/.openflows/`, `read_skill`, `.mcp.json`, `AGENTS.md` |
| **Release** | Ships **inside** the single `openflows-<ver>-<target>.tar.gz` (`openflows worker …`) — **installed at boot** | Ships as an `agent-harness-<ver>-<target>.tar.gz` package — **also installed at boot** (§3.1a); separate project |
| **Duplicated name source** | the crate is named `openflows-harness`; its CLI is the `worker` subcommand of `openflows` | the plugin skillset grew a `shared-harness-protocol` skill; docs called it "the harness" |

**The relationship:** the **Agent Harness (B)** is the discoverable, agent-facing surface. Its `commands/*.md` are **wrappers over** the worker surface (e.g. `dispatch.md → openflows worker dispatch read`), and its skills/AGENTS.md teach and point the agent at that surface. The **worker surface (A)** is the only thing that actually negotiates with the Controller over Redis and the A2A relay. **Both live in installed components** — A inside the `openflows` binary at boot (§3.1), B's package via provisioning/install (§3.1a) — and under the target §7 contract `openflows install` unpacks B's tree into `~/.openflows/` so the agent calls hooks/commands explicitly.

**The Controller ↔ harness contract (which is with A, the worker surface).** There are exactly **two** processes in the whole system that write to the SharedStore: the **Controller** (in the `openflows-nexus` control-plane workspace) and the **`openflows worker` surface (Harness A)** inside each worker workspace. They are the two ground terms and must not be conflated:

| | **Controller** | **Harness A (`openflows worker`)** |
|---|---|---|
| **Where it lives** | `openflows-nexus` control-plane workspace (long-lived, trusted) | Ephemeral worker workspaces (short-lived, untrusted) |
| **Executable** | `openflows run` (`binary/src/bin/agentflow.rs`) | `openflows worker …` (`binary/src/worker.rs` + `crates/openflows-harness`) |
| **Redis client** | `pocketflow-core::SharedStore` (writes orchestration state) | the *only* Redis client inside a workspace |
| **Credentials** | Holds Coder/GitHub tokens; has network egress | Holds **no** keys; restricted egress |
| **Governs** | WHERE the agent runs + coordination state | HOW the agent reads/writes coordination state |

The two never call each other directly over a private channel — their entire interaction is the **SharedStore keyspace + the A2A relay**, both on the control plane:

```
  CONTROLLER (NEXUS)                        WORKER (FORGE/SENTINEL/...)
 ┌──────────────────────┐                   ┌────────────────────────────┐
 │ orchestration state  │                   │ Agent Harness (B) — the    │
 │  · writes dispatch   │                   │  agent-facing surface      │
 └──────────┬───────────┘                   │  hooks/commands/skills/mcp │
            │  Redis (SharedStore)          │        │ wrap               │
            │                               │        ▼                   │
            │                               │  openflows worker (A) CLI │
            │  · dispatch read / status ────│  dispatch read / status    │
            │◀ gate approve / review ───────│  gate approve / review     │
            │◀ pr opened / handoff ─────────│  submit / pr opened        │
            └───────────────────────────────┘  heartbeat / verify        │
        A2A relay :3000 (hosted by the Controller)                        │
        └────────────────────────────────────────────────────────────────▶│
           FORGE executor (A: verify serve) dials OUT → claims → runs     │
           SENTINEL submits verify (A: verify request) via the same relay │
```

**Who writes what (the two-writer rule):**

| Artifact | Written by | Read by |
|---|---|---|
| `tickets`, `worker_slots`, `pending_prs`, `registry_json`, `control:*` | Controller | Controller |
| `ticket:{id}:dispatch:{role}` | Controller | Harness A (`dispatch read`) |
| `ticket:{id}:status` | Harness A (`status set`) | Controller |
| `ticket:{id}:gate:{phase}` | Harness A (`gate approve`) | Controller + Harness A (`status set` consumes it) |
| `ticket:{id}:review:{role}` | Harness A (`review submit`) | Controller |
| `ticket:{id}:chat:{role}` | Controller | Controller |
| `ticket:{id}:handoff` | Harness A (`handoff write`) | Controller + Harness A (`handoff read`) |
| `heartbeat:{role}:{ticket}` | Harness A (`heartbeat start`) | Controller (`reconcile`) |
| `pair:{id}:verification`, `audit:a2a:*` | Harness A executor / relay mirror | Controller + SENTINEL |

**Why this is a contract, not just shared state:**
- **Typed both directions.** The worker writes through Harness A (`openflows worker`, serde-validated; malformed writes exit non-zero and are never accepted). The Controller writes through `pocketflow-core` typed keys. The keys above, and their serde types, are the *interface* between the two processes.
- **The Agent Harness (B) never speaks Redis.** It is the surface the agent sees; every durable action it triggers funnels into the binary's typed commands. This is why "the only Redis client" is a property of **A**, and why B can live in its own project without weakening the guarantee.
- **Gate is enforced by the composition.** The Controller provisions the ticket into `planning`; Harness A refuses to leave `planning` without consuming a SENTINEL-written gate token via atomic `GETDEL` (`store.rs:96`, `authorize_gate_approver`). Neither side can skip the other's step.
- **The A2A relay is the only non-Redis channel, and it is still Controller-hosted.** FORGE's executor (A: `verify serve`) dials out to the relay inside NEXUS; SENTINEL submits through it. Even this live channel terminates in the control plane and mirrors results to Redis (§5.4, `openflows-a2a-relay.md`).
- **Recovery stays on the Controller side.** If a worker goes silent, the heartbeat key self-expires (§6) and the Controller — never the worker — reconciles and re-provisions. The worker has no authority over its own lifecycle.

This section is the answer to "how do the Controller and the harnesses interoperate?": the **Agent Harness (B)** presents the agent-facing surface, the **binary (Harness A)** is the sole Redis client that negotiates with the Controller over the typed tenant-scoped keyspace plus the Controller-hosted A2A relay, with the asymmetry held throughout — the Controller decides, Harness A executes.

---

## 6. Heartbeat & liveness

The heartbeat is the worker's **"still alive" beacon**. It is a long-lived background daemon (`openflows worker heartbeat start`, launched by the startup script — e.g. `openflows-forge/main.tf:237`) that lets NEXUS tell a workspace (and its agent) is running — and, crucially, when it has **crashed or gone silent** — without NEXUS having to poke every container.

```
  WORKER workspace                       REDIS (SharedStore)                    NEXUS (Controller)
        │  heartbeat start (daemon)            │                                   │
        │  loop every 30s:                      │                                   │
        │  write key with TTL 120s ────────────▶│ ns:{tenant}:heartbeat:{role}:{ticket} │
        │                                      │   { ts, ws_id, status:"running" }    │
        │                                      │                                   │
        │                                      │        every poll pass reads key ──▶│
        │                                      │◀─────────────────────────────────── │  reconcile():
        │                                      │                                    │  key present  → alive
        │  [daemon dies] key expires (120s) ──▶│  (self-deleting)                    │  silent >90s → STALE → recover
```

**What the daemon writes** (`heartbeat_start`, `crates/openflows-harness/src/store.rs:500`): every **30 seconds** it writes a `HeartbeatRecord` to Redis:

```
key:    ns:{tenant}:heartbeat:{role}:{ticket}
value:  { "ts": <unix-epoch seconds>, "ws_id": "<CODER_WORKSPACE_ID>", "status": "running" }
TTL:    120 seconds  (EX(120))
```

**How the numbers work together:**

| Number | Meaning |
|--------|---------|
| **30s** | write cadence — the daemon refreshes the TTL every 30s |
| **120s** | Redis TTL — if the daemon stops writing, the key **expires on its own** within ~2 minutes |
| **90s** | NEXUS staleness threshold — a worker silent for >90s is declared **stale** |

Because the key is **self-expiring** (120s TTL), NEXUS doesn't need a "last seen" tracker: if the key is present, the workspace is alive; if it's gone, the workspace has been dead for at most ~2 minutes. NEXUS's `reconcile()` declares a worker **stale after 90s** of silence and treats the workspace/chat as crashed and recoverable (system doc §7.5).

**What NEXUS does on stale:** the stale worker is marked for **recovery** — tear down and re-provision the workspace, re-assign the ticket — bounded to **3 recovery attempts** before the ticket escalates to `awaiting_human`. Heartbeat staleness is one of the primary inputs to this self-healing loop.

**Why a separate daemon, not the agent writing inline:** the beacon must keep firing even while the LLM agent is idle between tool calls, thinking, or blocked on a gate. A `nohup`'d background process outlives any single agent action, and it honors the rule that `openflows worker` is the only Redis client in the workspace. `heartbeat_stop` deletes the key on clean teardown.

---

## 7. The target contract: the Agent Harness (B) as an installed plugin filesystem the agent calls

The Agent Harness (**Harness B**) — the `orchestration/` plugin content: hooks, commands, skills, MCP, personas, standards — is the agent-facing half of the system (see §5.6 for the A/B split). Like Harness A, it is an **installed, versioned component**: the symmetric counterpart to the `openflows` binary's worker-surface install (§3.1/§3.1a). Today its delivery is partial — hooks via the CLI engine's `~/.claude/settings.json`, skills via `read_skill`, commands bundled rather than installed to one place. The current wiring (§5.2, §5.5) makes the Coder agent's behavior depend on **which agent engine** is present: hooks only fire through the CLI engine's `~/.claude/settings.json`, and the default Coder agent never sees them. The target design removes that dependency by installing the Agent Harness as **a self-describing filesystem plugin surface** that *any* agent — including the default Coder Chat agent — can discover and invoke as ordinary agentic tool calls. Because the Agent Harness is slated to **split into its own project**, this contract is also the boundary definition for that split: B is installed and versioned independently of the `openflows` binary, exactly as A is.

Concrete target layout produced by the boot — the two harnesses each installed: Harness A's binary on `PATH`, and the Agent Harness package unpacked to `~/.openflows/` (an `openflows install` subcommand writes this tree):

```
~/.openflows/                        # plugin root (installed by the Agent Harness package)
├── bin/openflows                    # Harness A — the `openflows` binary, on PATH (already installed)
├── hooks/
│   ├── hooks.json                   # machine-readable event → script map
│   ├── session_start.sh             # bootstrap: emit assignment/phase/workflow
│   ├── pre_bash_guard.sh            # policy: block destructive bash
│   ├── pre_write_check.sh
│   ├── post_write_lint.sh
│   └── stop_require_artifact.sh
├── commands/                        # agentic entry points (wrappers over the harness)
│   ├── dispatch.md | dispatch.sh    # → openflows worker dispatch read
│   ├── status.md  | status.sh       # → openflows worker status get|set
│   ├── gate.md    | gate.sh         # → openflows worker gate approve|status
│   ├── plan.md    | plan.sh         # → openflows worker plan read|write
│   └── verify.md  | verify.sh       # → openflows worker verify request|list
├── skills/…/SKILL.md                # read_skill discovery (already provisioned)
├── mcp.json                         # merged per-role + central
└── AGENTS.md                        # server-injected persona:
                                     #   "your tools live at ~/.openflows; call them"
```

The contract has four properties:

1. **Explicit discovery, no hidden engine.** `AGENTS.md` (server-injected every conversation) points the agent at `~/.openflows/hooks/hooks.json` and `~/.openflows/commands/*` as its tool registry. Readability is independent of whether the engine supports hook events.
2. **Hooks become callable scripts — and the startup hook is the first call.** "SessionStart" is the agent's **built-in first behaviour**: a one-time invocation of `session_start.sh` whose stdout is captured as its opening context (live dispatch, phase, workflow). Policy hooks (`pre_bash_guard.sh`, `stop_require_artifact.sh`) are scripts the agent invokes (or a thin wrapper calls) before risky actions. No reliance on any engine's internal event system or on a Controller-authored first message.
3. **Harness A commands stay the real tools.** `openflows worker` remains the only Redis client; the `commands/` wrappers just make the surface discoverable and uniform. Security properties (typed writes, gate `GETDEL`, `authorize_gate_approver`) are unchanged.
4. **Self-contained install.** `openflows install` (or the template step) writes the whole tree, replacing the Python `~/.claude/settings.json` snippet as the canonical wiring. The tree's content is owned by the Agent Harness project, so installing it is how a workspace pulls in Harness B.

> **Relationship to §5.2/§5.5:** today hooks are wired into agent settings for the CLI fallback only. Under this target, that `settings.json` step becomes optional/legacy; the plugin tree is the single, engine-agnostic contract through which **hook-driven startup** runs (`SessionStart` executes at boot) and the agent stays tool/hook-capable on both the Coder Chat agent and any CLI agent.

---

## 8. Network & security posture

Worker workspaces have **heavily restricted egress** (`openflows-system-architecture.md` §13):

```
ALLOW tcp/443 → coder-control-plane   (workspace daemon + AI Gateway)
ALLOW tcp/443 → github.com            (git push/pull, issue/PR API)
ALLOW           redis                  (SharedStore coordination)
ALLOW           openflows-nexus:3000   (A2A relay, outbound dial)
DENY  everything else
```

Security properties:

- **No key exfiltration surface** — no LLM keys (AI Gateway in control plane) and no raw GitHub PATs (Coder external auth).
- **Agent framework runs centrally** — the workspace hosts the harness CLI (Harness A) and the Agent Harness plugin surface (Harness B), not a full agent runtime with keys.
- **Command control** — Agent Harness (B) `PreToolUse`/`Stop` hooks + A2A allowlist bound what the agent can run.
- **Tenant isolation** — Redis `ns:{tenant}:` prefixes keep one tenant's workspace state invisible to another's.
- **Review integrity** — SENTINEL and FORGE never share a filesystem; SENTINEL interacts with FORGE's code only through the audited A2A allowlist, never by touching FORGE's tree.

---

## 9. Lifecycle

1. **Provision** — Controller creates the workspace from the role template.
2. **Boot** — startup script installs Harness A (`openflows`, runtime pull, see §3.1) **and** installs the Agent Harness (B) package (see §3.1a) — hooks/skills/MCP/standards/persona — then clones the repo, starts heartbeat; FORGE starts the A2A executor. (Target: `openflows install` unifies the §7 plugin tree install.)
3. **Bind** — Controller creates a Coder chat bound to the workspace; the agent's **`SessionStart` hook runs automatically and seeds its opening context**, then the agent works via Agent Harness commands + hooks (see §5.5). The chat first message is at most a pointer to `~/.openflows/`.
4. **Work** — the agent coordinates through the Agent Harness (B) surface (startup hook + commands + policy hooks), which delegates to Harness A (`openflows worker`: dispatch → status → PR → handoff).
5. **Teardown** — the workspace is deleted after the PR merges (VESSEL), and the heartbeat key is removed.

Recovery is the Controller's job, not the workspace's: a stale heartbeat (>90s) or crashed chat marks the workspace for reconciliation, bounded to 3 recovery attempts before `awaiting_human` escalation (system doc §7.5).

---

## 10. Related Documents

- `docs/architecture/openflows-system-architecture.md` — §5 (Subsystem 03), the authoritative overview.
- `docs/architecture/openflows-controller.md` — the process that provisions and binds these workspaces.
- `docs/architecture/openflows-a2a-relay.md` — the A2A executor surface FORGE hosts (`verify serve`).
- `docs/architecture/openflows-redis-shared-store.md` — the shared Redis layer (the other writer half of §5.6, the coordination store, heartbeat keys).
- `docs/architecture/openflows-system-architecture.md` §13 — egress policy and audit model (governance).
- `docs/architecture/openflows-system-architecture.md` §5.4 and `docs/architecture/openflows-redis-shared-store.md` §3 — multi-tenant isolation and Redis namespacing.
- `docs/architecture/openflows-system-architecture.md` §10 — skills, MCP, hooks, and roles extension points.
- `release-plz.toml` + `.github/workflows/release-assets.yml` — how the single `openflows` binary (controller + worker surface) is built and published (§3.1).
- `crates/coder-client/templates/openflows-{forge,sentinel,vessel,lore}/main.tf` — the startup scripts that pull and install Harness A at boot.
