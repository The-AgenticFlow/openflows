# OpenFlows Architecture: Orchestrator & Agents

## Overview

OpenFlows is a multi-agent orchestration system that runs autonomous AI development teams on Coder workspaces. Each agent is a specialized AI role (Forge, Sentinel, Vessel, Lore) that collaborates through Redis-backed coordination.

```
┌─────────────────────────────────────────────────────────────────────┐
│                         NEXUS (Orchestrator)                        │
│  - Assigns tickets to workers                                         │
│  - Monitors progress and recovery                                    │
│  - Coordinates the team flow                                         │
│                              │                                        │
│    ┌─────────────────────────┼─────────────────────────┐            │
│    │                         │                         │            │
│    ▼                         ▼                         ▼            │
│ ┌──────────┐           ┌────────────┐           ┌──────────┐      │
│ │   FORGE  │──────────▶│  SENTINEL  │──────────▶│  VESSEL  │      │
│ │  Builder │   PR      │  Reviewer  │  approve  │  Merge   │      │
│ └──────────┘           └────────────┘           └──────────┘      │
│      │                       │                                          │
│      └───────────────────────┴──────────────────────────────────┐  │
│                            │                                        │  │
│                            ▼                                        │  │
│                      ┌──────────┐                                    │  │
│                      │   LORE   │                                    │  │
│                      │ Document │                                    │  │
│                      └──────────┘                                    │  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │     Redis       │
                    │  (SharedStore)  │
                    │  - Tickets      │
                    │  - Dispatch     │
                    │  - Status       │
                    │  - Heartbeats   │
                    └─────────────────┘
```

## The Nexus (Orchestrator)

Nexus is the central coordinator that runs as the controller in a dedicated Coder workspace. It owns all coordination logic:

### Responsibilities
- **Ticket Assignment**: Fetches GitHub issues, creates tickets, assigns to idle workers
- **Worker Provisioning**: Creates Coder workspaces for Forge/Sentinel workers on demand
- **Recovery**: Monitors for crashed workspaces, orphaned tickets, stale workers
- **Team Coordination**: Routes work between agents based on phase transitions

### Decision Loop

```
Nexus.loop():
  1. prep()   → Sync GitHub issues, check worker health, load Redis state
  2. exec()   → Rule-based routing (no LLM needed for Coder-only design)
  3. post()   → Execute decision (assign ticket, provision workspace, pause)
  4. repeat() every 15 seconds
```

### Key State (Redis)

| Key | Description |
|-----|-------------|
| `tickets` | List of all ticket IDs |
| `ticket:{id}:status` | Current status (assigned, working, awaiting_human) |
| `ticket:{id}:dispatch` | Task payload for the worker |
| `worker_slots` | Map of worker IDs → status + workspace_id |
| `ns:{tenant}:heartbeat:{worker}` | Last heartbeat timestamp |

### Agent Bootstrapping

When Nexus assigns a ticket to a worker, it:

1. **Provisions the workspace** (forge/sentinel/vessel/lore template)
2. **Creates a Coder Chat** with the agent's full persona in the initial message
3. **Agent receives**:
   - Full agent persona (identity, capabilities, non-negotiables)
   - Ticket dispatch (via `openflows-harness dispatch read`)
   - Coordination protocol (harness commands, phase workflow)
   - Ticket ID assignment

The persona is loaded from `orchestration/agent/agents/{role}.agent.md` and embedded in the chat message, giving the agent complete context about their role and system expectations.

## The Agents

### FORGE (Builder)

The primary code generator. FORGE writes code against a `CONTRACT.md` that Sentinel helps refine.

```
┌─────────────────────────────────────────────────────────────┐
│ FORGE Workspace                                             │
│                                                              │
│  ┌────────────────┐     ┌──────────────────────────────────┐│
│  │ Coder Agent    │────▶│ openflows-harness                ││
│  │ (LLM Session)  │     │ - dispatch read (get task)       ││
│  └────────────────┘     │ - status set/ get (track phase)  ││
│                          │ - pr opened (record PR)          ││
│                          │ - heartbeat start (alive signal) ││
│                          └──────────────────────────────────┘│
│                                      │                        │
│                                      ▼                        │
│                              ┌─────────────┐                 │
│                              │    Redis    │                 │
│                              │ SharedStore │                 │
│                              └─────────────┘                 │
└─────────────────────────────────────────────────────────────┘
```

**Workflow Phases:**
1. `planning` → Analyze task, create CONTRACT.md
2. `building` → Implement solution
3. `testing` → Run tests
4. `review_ready` → PR is open, awaiting Sentinel

**Hooks (Claude Code):**
- `session_start.sh` → Provides dispatch context, workflow guide
- `pre_bash_guard.sh` → Policy enforcement (no direct Redis, no destructive commands)
- `post_write_lint.sh` → Auto-format on write
- `stop_require_artifact.sh` → Refuse to stop unless PR is created

### SENTINEL (Reviewer)

Adversarial reviewer that checks code against CONTRACT.md before merging.

**Approach:**
- `plan_mode: true` → Uses more thinking tokens for thorough review
- Reviews security, test coverage, adherence to contract
- Creates blocking comments on PR if issues found

**Workflow:**
1. Receives handoff notification
2. Reviews diff against CONTRACT.md
3. Posts review comments to GitHub PR
4. Labels PR as `needs-revision` or `approved`

### VESSEL (DevOps)

Handles CI/CD operations and merging.

**Responsibilities:**
- Monitors CI status on PRs
- Handles merge conflicts
- Squash-merges green PRs
- Tears down workspaces after merge

**Workflow:**
1. Watch for PRs labeled `approved`
2. Verify CI is green
3. Handle any merge conflicts
4. Merge with squash strategy
5. Archive ticket, clean up workspace

### LORE (Documentation)

Maintains project documentation. Disabled by default.

**Responsibilities:**
- Updates CHANGELOG.md on merge
- Creates architecture decision records
- Maintains team knowledge base

## Coordination Protocol

### Harness Commands

All coordination happens through `openflows-harness` (never direct Redis):

```bash
# Ticket context
openflows-harness dispatch read     # Get task payload
openflows-harness dispatch status   # Show assignment status

# Phase tracking  
openflows-harness status get        # Current phase
openflows-harness status set <phase>  # Update progress

# PR coordination
openflows-harness pr opened --pr N --branch B --title "X"
openflows-harness pr merged --pr N

# Handoff to next agent
openflows-harness handoff write --contract changes.md --notes "..."
```

### Phase Transitions

```
planning ──────▶ building ──────▶ testing ──────▶ review_ready
    │                                │                │
    │                                │                ▼
    ▼                                ▼           awaiting_human
blocked (if stuck)               blocked         (Sentinel review)
                                               │
                                               ▼
                                          merged (Vessel)
```

### Heartbeat Protocol

Workers send heartbeats every 30 seconds. Nexus monitors and marks workers as stale after 90 seconds of silence.

```
forge-1: { status: "working", last_heartbeat: 1719000000 }
forge-2: { status: "idle", last_heartbeat: 1719000030 }
```

## Workspace Lifecycle

```
1. Template created (openflows-forge, openflows-sentinel, etc.)
2. Nexus provisions workspace for worker
3. Startup script runs:
   - Installs openflows-harness (REQUIRED)
   - Copies hooks from orchestration volume
   - Writes hooks to ~/.claude/settings.json
   - Starts heartbeat daemon
   - Clones repo
4. Nexus creates Coder Chat for the workspace
5. First message sent to kick off agent
6. Agent starts working via harness
7. On PR merge: workspace deleted
```

## Security Model

- **Network**: Only Coder control plane + GitHub + Redis allowed
- **Hooks**: Prevent direct Redis access, destructive commands
- **Token Scoping**: Each tenant gets limited-scope tokens
- **Isolation**: Tenants isolated by Redis key prefixes

## Extension Points

- **Add Agent**: Extend `agent-vessel`, `agent-lore` or create new crate
- **Add Skill**: Drop in `orchestration/plugin/skills/{role}/`
- **Add Hook**: Add to `orchestration/plugin/hooks/{role}/`
- **Add Model**: Configure in Coder dashboard, reference in `registry.json`