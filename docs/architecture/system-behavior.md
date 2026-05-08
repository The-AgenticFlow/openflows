# AgentFlow System Architecture

## Overview

AgentFlow is an autonomous AI development team that implements, validates, and merges code changes without human intervention. The system is a cyclic flow of three agents -- NEXUS (orchestrator), FORGE-SENTINEL (implementation), and VESSEL (merge gatekeeper) -- connected through a shared state store.

The key architectural principle is that **NEXUS is the orchestrator of the entire pipeline**, not just a ticket assigner. It can detect broken states at any point in the flow and resume the pipeline at the correct phase.

---

## Agent Roles

| Agent | Role | Implementation | Capabilities |
|---|---|---|---|
| **NEXUS** | Orchestrator | LLM-driven (AgentRunner) | Ticket assignment, flow recovery, command gating, pipeline routing |
| **FORGE-SENTINEL** | Implementation pair | Event-driven Claude Code processes | Code implementation, plan review, segment evaluation, PR creation |
| **VESSEL** | Merge gatekeeper | Deterministic Rust code | CI polling, squash merge, ticket closure, event emission |

---

## Flow Graph

```
                    +----------------------------------------------+
                    |                                              |
                    v                                              |
+----------+  work_assigned  +--------------+  pr_opened  +---------+
|          | --------------> |              | -----------> |         |
|  NEXUS   |                 | FORGE-SENTINEL|             | VESSEL  |
|          | <-------------- |              | <---------- |         |
+----------+  failed/        +--------------+  deployed/  +---------+
    |         suspended         |               deploy_failed
    |                           |               merge_blocked
    |         merge_prs         |  conflicts_     no_work
    | --------------------------+  detected          |
    |                           |                    |
    |         no_work           +--------------------+
    +--------> (loop)          |
              NEXUS <-----------+
```

### Routing Table

| Source | Action | Target | Meaning |
|---|---|---|---|
| NEXUS | `work_assigned` | FORGE-SENTINEL | Assign ticket to worker for implementation |
| NEXUS | `merge_prs` | VESSEL | Resume merge pipeline for pending PRs |
| NEXUS | `no_work` | NEXUS | No actionable items, loop back |
| NEXUS | `approve_command` | FORGE-SENTINEL | Approve worker's suspended command |
| NEXUS | `reject_command` | NEXUS | Reject worker's command, recycle worker |
| FORGE-SENTINEL | `pr_opened` | VESSEL | Implementation complete, PR created |
| FORGE-SENTINEL | `failed` | NEXUS | Implementation failed, retry possible |
| FORGE-SENTINEL | `suspended` | NEXUS | Worker needs command approval |
| FORGE-SENTINEL | `no_tickets` | NEXUS | No workers assigned/working (empty batch) |
| VESSEL | `deployed` | NEXUS | PR merged successfully |
| VESSEL | `deploy_failed` | NEXUS | CI failed or merge blocked |
| VESSEL | `merge_blocked` | NEXUS | Merge conflict or other blockage |
| VESSEL | `conflicts_detected` | FORGE-SENTINEL | Merge conflicts — same worker resolves and pushes |
| VESSEL | `no_work` | NEXUS | No pending PRs to process |

---

## Pipeline Phases

A ticket moves through three phases. NEXUS is responsible for ensuring every ticket completes the entire pipeline.

### Phase 1: Implementation (NEXUS -> FORGE-SENTINEL)

**Trigger:** `work_assigned` action from NEXUS
**Agent:** FORGE-SENTINEL pair (Claude Code FORGE + Claude Code SENTINEL)
**Completion signal:** PR opened on GitHub, `pr_opened` action returned

The FORGE-SENTINEL pair runs an event-driven lifecycle:
1. FORGE writes PLAN.md -> SENTINEL reviews -> CONTRACT.md agreed
2. FORGE implements segments -> SENTINEL evaluates -> segment-N-eval.md
3. SENTINEL final review -> final-review.md
4. FORGE opens PR -> STATUS.json written

**STATUS.json recognized values:**

| STATUS | PairOutcome | Flow Path |
|---|---|---|
| `PR_OPENED`, `COMPLETE`, `complete`, `completed`, `COMPLETED`, `SEGMENTS_COMPLETE`, `SEGMENT_COMPLETE_AWAITING_REVIEW` | PrOpened (if pr_url present) or Blocked | -> VESSEL or -> NEXUS |
| `IMPLEMENTATION_COMPLETE` | Blocked ("needs push/PR creation") | -> NEXUS (forge attempts auto-push with secret scrubbing) |
| `IMPLEMENTATION_COMPLETE` (push failed) | Blocked ("Push rejected: secrets detected — GH013: ...") | -> NEXUS (blocked with actual error detail) |
| `PENDING_REVIEW` | Non-terminal — FORGE requests SENTINEL review, harness continues event loop | Continue watching |
| `BLOCKED` | Blocked | -> NEXUS (suspended) |
| `FUEL_EXHAUSTED` | FuelExhausted | -> NEXUS (failed, possibly retryable) |
| `SEGMENT_N_DONE` (intermediate) | Non-terminal — harness continues event loop | Continue watching |
| Any other string | FuelExhausted ("Unknown status: X") | -> NEXUS (failed) |

### Phase 2: Merge (VESSEL)

**Trigger:** `pr_opened` action from FORGE-SENTINEL, or `merge_prs` action from NEXUS
**Agent:** VESSEL (deterministic Rust code, no LLM)
**Completion signal:** `deployed` action (merged) or `deploy_failed` action (failed)

VESSEL processes each entry in `pending_prs` from the shared store:
1. Fetch PR details from GitHub API
2. If CI workflows exist: poll CI status until terminal (success/failure/timeout, 10s interval, 10min timeout)
3. If CI green (or no CI): squash merge with ticket reference
4. Update ticket status to Merged, remove from pending_prs, recycle worker to Idle

**Input (from shared store):**
- `pending_prs`: Array of `{number, ticket_id, branch, worker_id}`
- `repository`: "owner/repo" string
- `ci_readiness`: Ready / Missing / SetupInProgress

### Phase 3: Done (back to NEXUS)

VESSEL returns `deployed` or `deploy_failed` -> NEXUS re-evaluates state for next action.

---

## Shared State Store

All agents communicate through a SharedStore (in-memory or Redis-backed).

### Store Keys

| Key | Type | Written By | Read By | Description |
|---|---|---|---|---|
| `tickets` | `Vec<Ticket>` | NEXUS, FORGE, VESSEL, init | NEXUS, FORGE, VESSEL | All tracked work items and their status |
| `worker_slots` | `HashMap<String, WorkerSlot>` | NEXUS, FORGE, VESSEL, init | NEXUS, FORGE | Available forge worker slots and their state |
| `pending_prs` | `Vec<serde_json::Value>` | NEXUS, FORGE, VESSEL, init | NEXUS, VESSEL | PRs awaiting VESSEL merge processing |
| `command_gate` | `HashMap<String, Value>` | FORGE, NEXUS | NEXUS | Suspended workers awaiting command approval |
| `ci_readiness` | `CiReadiness` enum | NEXUS | NEXUS, VESSEL | Whether CI workflows exist in the repository |
| `repository` | `String` | init | NEXUS, VESSEL | "owner/repo" string for GitHub API calls |
| `_no_work_count` | `u32` | NEXUS | NEXUS | Consecutive no_work cycles (stops at 3) |
| `_forge_batch_workers` | `Vec<String>` | FORGE prep | FORGE post | Worker IDs in current batch (for failure cleanup) |
| `ticket:{id}:status` | `String` | VESSEL | (external) | Per-ticket merge status for dependency resolution |
| `command_gate::{id}` | `CommandProposal` | CommandGate | NEXUS | Dangerous command proposal from a worker |
| `command_gate::{id}::decision` | `CommandDecision` | NEXUS | CommandGate | Approval/rejection of a proposed command |

### Value Structures

#### Ticket (`crates/config/src/state.rs`)

```rust
pub struct Ticket {
    pub id: String,           // e.g. "T-001", "T-CI-001"
    pub title: String,
    pub body: String,
    pub priority: u32,
    pub branch: Option<String>,
    pub status: TicketStatus,
    pub issue_url: Option<String>,
    pub attempts: u32,        // MAX_ATTEMPTS = 3
}
```

#### TicketStatus (`crates/config/src/state.rs`)

```rust
#[serde(tag = "type")]
pub enum TicketStatus {
    Open,                                                    // Assignable
    Assigned { worker_id: String },                          // Not assignable
    InProgress { worker_id: String },                        // Not assignable
    Completed { worker_id: String, outcome: String },        // Not assignable
    Merged { worker_id: String, pr_number: u64 },            // Not assignable (terminal)
    Failed { worker_id: String, reason: String, attempts: u32 }, // Assignable if attempts < 3
    Exhausted { worker_id: String, attempts: u32 },          // Not assignable (terminal)
}
```

#### WorkerSlot (`crates/config/src/state.rs`)

```rust
pub struct WorkerSlot {
    pub id: String,       // e.g. "forge-1"
    pub status: WorkerStatus,
}

#[serde(tag = "type")]
pub enum WorkerStatus {
    Idle,
    Assigned { ticket_id: String, issue_url: Option<String> },
    Working { ticket_id: String, issue_url: Option<String> },
    Done { ticket_id: String, outcome: String },
    Suspended { ticket_id: String, reason: String, issue_url: Option<String> },
}
```

#### pending_prs Entry (JSON Value, not strongly typed)

Entries come from two sources with different schemas:

**From FORGE post_batch** (minimal):
```json
{ "number": 42, "ticket_id": "T-001", "branch": "forge-1/T-001", "worker_id": "forge-1" }
```

**From NEXUS sync_open_prs** (GitHub-sourced, richer):
```json
{
  "number": 42, "ticket_id": "T-001",
  "head_sha": "abc123", "head_branch": "forge-1/T-001",
  "base_branch": "main", "title": "PR title",
  "mergeable": true, "has_conflicts": false
}
```

The `worker_id` field is required for VESSEL to recycle the worker after merge/conflict.

### Emitted Events

Events are written to a ring buffer in the SharedStore. Each event has `{ agent, event_type, payload, ts }`.

**Automatic lifecycle events** (emitted by the Node/BatchNode framework for every node):
`prep_started`, `prep_done`, `exec_started`, `exec_done`, `post_started`, `post_done`, `batch_prep_started`, `batch_empty`, `batch_exec_started`, `batch_exec_done`, `batch_done`

**Domain events** (emitted by VesselNotifier):

| Agent | Event Type | Trigger | Payload |
|---|---|---|---|
| `vessel` | `ticket_merged` | Successful merge | `{ ticket_id, pr_number, sha }` |
| `vessel` | `ci_failed` | CI status terminal-failure | `{ ticket_id, pr_number, reason }` |
| `vessel` | `merge_blocked` | GitHub merge API rejection | `{ ticket_id, pr_number, reason }` |
| `vessel` | `ci_timeout` | CI poll max attempts reached | `{ ticket_id, pr_number }` |
| `vessel` | `ci_missing` | Merged without CI validation | `{ ticket_id, pr_number }` |
| `vessel` | `conflicts_detected` | Unresolvable merge conflicts | `{ ticket_id, pr_number, conflicted_files }` |

**CommandGate events**:

| Agent | Event Type | Trigger |
|---|---|---|
| `{worker_id}` | `command_gate_proposed` | Worker proposes dangerous command |
| `{worker_id}` | `command_gate_approved` | Nexus approves command |
| `{worker_id}` | `command_gate_rejected` | Nexus rejects command |

### Ticket Lifecycle

```
                     +--------------------------------------+
                     |         (VESSEL conflict             |
                     |          recovery)                    |
                     |              |                        |
                     v              |                        |
   Open --> Assigned --> InProgress --> Completed --> Merged
     ^          |                         (pr_opened)        |
     |          |                              |             |
     |          v                              |             |
     |        Failed <-------------------------+             |
     |       (attempts < 3)          VESSEL marks Failed    |
     |          |                   on CI failure /         |
     |          v                   merge conflict           |
     |        Exhausted                                      |
     |       (attempts >= 3)                                 |
     |          |                                            |
     +----------+  (nexus resets conflict-Failed             |
       (re-assignable)   tickets back to Open)               |
```

| Status | Meaning | Assignable? |
|---|---|---|
| Open | Unassigned, ready for work | Yes |
| Assigned { worker_id } | Assigned to a worker | No |
| InProgress { worker_id } | Actively being worked on | No |
| Completed { worker_id, outcome } | Implementation done (outcome e.g. "pr_opened") | No |
| Merged { worker_id, pr_number } | PR merged, fully complete | No |
| Failed { worker_id, reason, attempts } | Failed, retryable if attempts < 3 | Yes (if attempts < 3) |
| Exhausted { worker_id, attempts } | Max retries reached, terminal | No |

> **Note on conflict recovery:** Nexus's deterministic conflict recovery resets `Failed` tickets (where reason contains "Merge conflicts") back to `Open` regardless of attempts. This means conflict-triggered rework is not capped by `MAX_ATTEMPTS`. Only forge-level failures (spawn failure, fuel exhaustion) can produce the `Exhausted` terminal state.

### Worker Lifecycle

```
  Idle --> Assigned --> Working --> Done --> (recycled to Idle)
              |                         |
              v                         v
          Suspended                 (recycled to Idle
          (command gate)             when assignable tickets exist)
```

| Status | Meaning |
|---|---|
| Idle | Available for assignment |
| Assigned { ticket_id, issue_url } | Assigned but not started |
| Working { ticket_id, issue_url } | Actively working |
| Done { ticket_id, outcome } | Completed work (auto-recycled to Idle when assignable tickets exist) |
| Suspended { ticket_id, reason, issue_url } | Waiting for command approval |

---

## Flow Recovery (NEXUS as Orchestrator)

The critical design feature is that **NEXUS can detect and resume the flow at any point**. This handles:

- Network failures between agents
- Agent crashes (FORGE process dies, VESSEL timeout)
- Unrecognized STATUS.json values that cause fuel exhaustion
- Process restarts (tickets/workers left in intermediate states)

### Reconciliation (NexusNode::reconcile())

On every NEXUS cycle, prep() runs reconcile() which scans the shared store for inconsistencies:

| Detection | Condition | Root Cause |
|---|---|---|
| **Unmerged PRs** | pending_prs has entries but VESSEL never ran | FORGE crashed after creating PR, network failure prevented vessel routing, STATUS.json unrecognized |
| **Orphaned tickets** | Ticket in Assigned/InProgress but worker is Idle or missing | FORGE crashed before updating ticket status, process restart lost worker state |
| **Stale workers** | Worker in Assigned/Working but ticket is Open (was reset by recovery) | Cross-state inconsistency after partial recovery |
| **Stale suspended workers** | Worker in Suspended but ticket is already Completed/Merged | Command gate was never cleared after ticket completed |
| **Completed without PR** | Ticket Completed{outcome:"pr_opened"} but no matching entry in pending_prs | PR data was lost from the store (rare) |

### FlowRecovery Data Structure

```rust
pub struct FlowRecovery {
    pub unmerged_prs: Vec<UnmergedPr>,          // PRs in pending_prs awaiting merge
    pub orphaned_tickets: Vec<OrphanedTicket>,  // Tickets assigned to idle/missing workers
    pub stale_workers: Vec<StaleWorker>,         // Workers stuck on non-existent/completed tickets
    pub completed_without_pr: Vec<String>,       // Completed tickets missing from pending_prs
    pub has_unmerged_prs: bool,
    pub has_orphaned_tickets: bool,
    pub has_stale_workers: bool,
    pub has_completed_without_pr: bool,
    pub needs_recovery: bool,
}
```

This structure is computed in prep() and passed to the NEXUS LLM as `flow_recovery` context. The persona uses it to prioritize recovery over new work assignment.

### Automatic Recovery (NexusNode::recover_orphans())

When NEXUS returns `work_assigned`, the post() method automatically runs recover_orphans() which:

1. **Resets orphaned tickets**: Tickets in Assigned/InProgress whose worker is Idle or missing are reset to Open (re-assignable)
2. **Recycles stale suspended workers**: Workers in Suspended whose ticket is already Completed/Merged are recycled to Idle
3. **Recycles stale assigned workers**: Workers in Assigned/Working whose ticket was reset to Open by recovery are recycled to Idle

### NEXUS Decision Priority

The NEXUS persona enforces this strict priority order:

1. **Unmerged PR recovery** -> `merge_prs` action -> routes to VESSEL
2. **Command gate** -> `approve_command` / `reject_command`
3. **CI-first rule** -> assign CI setup ticket if ci_readiness is Missing
4. **New work** -> `work_assigned` action -> routes to FORGE-SENTINEL
5. **No work** -> `no_work` action -> loops back to NEXUS

This ensures that completed work (PRs awaiting merge) is always processed before new work is started.

---

## NEXUS Cycle (Detailed)

Each NEXUS cycle follows the prep -> exec -> post pattern:

### prep() -- Gather Context

1. **sync_registry** -- Load registry.json, add new WorkerSlots as Idle
2. **Parse repository** -- Split store["repository"] into owner/repo_name
3. **sync_issues** -- Call GitHub API, filter out PRs, create Ticket objects
4. **check_ci_readiness** -- Call GitHub API for workflow files
5. **ensure_ci_setup_ticket** -- Inject synthetic CI ticket if needed
6. **prioritize_ci_first** -- Sort tickets so CI tickets come first
7. **Recycle Done workers** -- Done -> Idle when assignable tickets exist
8. **reconcile** -- Detect flow inconsistencies (unmerged PRs, orphaned tickets, stale workers)
9. **Build context** -- All state + recovery data passed to LLM

### exec() -- LLM Decision

The AgentRunner invokes the LLM with the nexus persona and context. The LLM returns an AgentDecision with action, notes, and optional assign_to/ticket_id/issue_url fields.

### post() -- Apply Decision

| Action | Store Effects | Flow Route |
|---|---|---|
| `merge_prs` | Resets no_work counter | -> VESSEL (via ACTION_MERGE_PRS route) |
| `work_assigned` | Resets no_work counter; runs recover_orphans(); sets ticket to Assigned; sets worker to Assigned; injects CI ticket if needed | -> FORGE-SENTINEL |
| `no_work` | Increments no_work counter; returns STOP_SIGNAL after 3 consecutive | -> NEXUS (loop) |
| `approve_command` | Removes from command_gate; transitions worker Suspended -> Assigned | -> FORGE-SENTINEL |
| `reject_command` | Removes from command_gate; transitions worker to Idle | -> NEXUS (loop) |

---

## Merge Conflict Handling

Merge conflicts are a first-class concern in the flow. When VESSEL detects conflicts on a PR, it routes directly back to the same FORGE-SENTINEL pair that created the PR — the same worker resolves the conflicts and pushes, then VESSEL re-monitors CI.

### Conflict Detection

Conflicts are detected at two points:

1. **CiPoller early detection** (`crates/agent-vessel/src/ci_poller.rs`): Every 3rd CI poll attempt, the poller re-fetches the PR from GitHub and checks `mergeable`. If `mergeable == Some(false)`, it short-circuits the CI poll and returns `CiPollResult::Conflicts`.

2. **Post-timeout check** (`crates/agent-vessel/src/node.rs`): If CI times out, VESSEL re-fetches the PR and checks `has_conflicts()`. If conflicts are found after timeout, it treats it as a conflict case.

### Conflict Rework Flow

```
  VESSEL detects conflicts (CiPollResult::Conflicts or timeout + has_conflicts)
       |
       v
  VesselNode.handle_conflicts():
       |-- Abort any in-progress rebase in worktree
       |-- git merge origin/main --no-edit in worktree (produces conflict markers)
       |-- List conflicted files (git diff --name-only --diff-filter=U)
       |-- Write CONFLICT_RESOLUTION.md to pair's shared/ dir with:
       |     - List of conflicted files
       |     - Step-by-step resolution instructions
       |     - "Resolve markers, commit, push, write STATUS.json"
       |
       v
  VesselNode.post() (VesselOutcome::Conflicts):
       |-- emit "conflicts_detected" event
       |-- Keep PR in pending_prs (VESSEL will re-check it after push)
       |-- Re-assign worker from Done -> Assigned (same ticket, same worker)
       |-- return "conflicts_detected" action
       |
       v
  Flow routes directly to FORGE-SENTINEL (forge_pair)
       |
       v
  ForgeSentinelPair.run():
       |-- Detects CONFLICT_RESOLUTION.md in shared/ dir
       |-- Skips plan review + final review (plan_approved=true, final_approved=true)
       |-- Writes TASK.md with conflict resolution instructions
       |-- FORGE resolves conflict markers, commits, pushes
       |-- FORGE writes STATUS.json with PR_OPENED
       |-- Pair cleanup: removes CONFLICT_RESOLUTION.md
       |
       v
  VESSEL re-monitors CI on updated branch
       |-- If CI passes -> merge -> ticket_merged
       |-- If new conflicts -> repeat conflict rework loop
```

### No Nexus Involvement

Conflicts route directly from VESSEL → FORGE-SENTINEL, bypassing NEXUS. This is because:

1. **Same worker, same worktree:** The worker that created the PR has the full implementation context in its worktree. Sending the ticket through NEXUS would lose this context.
2. **Same PR, same branch:** After resolving conflicts and pushing, the existing PR is updated — no need to create a new PR.
3. **Faster cycle:** No LLM call needed to decide which worker gets the ticket. The same worker that knows the code resolves the conflicts.

### Key Design Decisions

1. **Direct VESSEL → FORGE routing:** `conflicts_detected` routes to `forge_pair`, not `nexus`. The same worker that created the PR resolves the conflicts.

2. **PR stays in pending_prs:** The conflicting PR is NOT removed from `pending_prs`. After forge pushes the resolution, VESSEL will pick it up on the next cycle.

3. **Worker re-assigned, not recycled:** The worker is moved from `Done` back to `Assigned` (same ticket), so forge_pair picks it up in `prep_batch`.

4. **git merge origin/main in worktree:** VESSEL runs `git merge origin/main --no-edit` in the worktree to produce conflict markers. This gives FORGE the exact diff context it needs to resolve conflicts intelligently.

5. **CONFLICT_RESOLUTION.md as instruction file:** Written to the pair's shared directory. FORGE reads it and knows exactly which files have conflicts and how to resolve them. Deleted on pair cleanup after successful PR.

6. **Conflict rework is NOT capped by MAX_ATTEMPTS:** Since the same worker is re-assigned (not a fresh start), conflict rework doesn't increment the ticket's `attempts` counter.

---

## Failure Scenarios and Recovery

### Scenario 1: FORGE crashes after PR creation (the original bug)

**Before the fix:** FORGE writes STATUS.json with "COMPLETED" (uppercase) -> pair harness doesn't recognize it -> FuelExhausted -> forge returns "failed" -> routes to NEXUS -> NEXUS sees no assignable tickets and unmerged PRs but has no `merge_prs` action -> loops on `no_work` until stop.

**After the fix:**
1. Pair harness recognizes "COMPLETED" as a completion status -> PrOpened -> forge returns "pr_opened" -> routes to VESSEL (happy path)
2. Even if pair harness still fails (unknown status), FORGE's post_batch() checks GitHub for an existing PR -> if found, adds to pending_prs -> returns "pr_opened" -> routes to VESSEL
3. If both mechanisms fail and it still routes as "failed" back to NEXUS, reconcile() detects pending_prs has entries -> flow_recovery.has_unmerged_prs = true -> NEXUS returns `merge_prs` -> routes to VESSEL

### Scenario 2: Network failure between FORGE and VESSEL

FORGE successfully creates PR and returns "pr_opened", but the flow crashes before VESSEL runs.

**Recovery:** On restart, reconcile() finds entries in pending_prs -> NEXUS returns merge_prs -> VESSEL processes them.

### Scenario 3: VESSEL times out on CI polling

VESSEL returns "deploy_failed" -> routes to NEXUS. The PR stays in pending_prs.

**Recovery:** On the next NEXUS cycle, reconcile() detects the PR is still in pending_prs -> NEXUS returns merge_prs -> VESSEL retries the merge.

### Scenario 4: FORGE process killed mid-implementation

Ticket stays in Assigned/InProgress, worker stays in Working.

**Recovery:** On the next NEXUS cycle, reconcile() detects orphaned ticket (worker might be Idle after crash) -> recover_orphans() resets ticket to Open -> NEXUS can re-assign it.

### Scenario 5: Process restart with stale state

Tickets in Assigned, workers in Working, pending_prs with unmerged PRs.

**Recovery:** reconcile() detects all inconsistencies at once. NEXUS prioritizes merge_prs first (unmerged PRs), then recover_orphans() resets orphaned tickets so they can be re-assigned.

### Scenario 6: VESSEL detects merge conflicts

VESSEL processes PR, detects conflicts, routes directly to FORGE-SENTINEL for rework.

**Recovery:**
1. VESSEL runs `git merge origin/main` in worktree to produce conflict markers
2. VESSEL writes `CONFLICT_RESOLUTION.md` with conflicted files and instructions
3. VESSEL re-assigns worker from `Done` → `Assigned` (same ticket)
4. Flow routes `conflicts_detected` directly to `forge_pair`
5. FORGE detects `CONFLICT_RESOLUTION.md`, skips plan/final review, resolves conflicts, commits, pushes
6. VESSEL re-monitors CI on the updated branch

---

## Key Source Files

| Component | File |
|---|---|
| NEXUS node | crates/agent-nexus/src/lib.rs |
| NEXUS persona | orchestration/agent/agents/nexus.agent.md |
| FORGE-SENTINEL pair node | crates/agent-forge/src/lib.rs |
| Pair harness (STATUS.json) | crates/pair-harness/src/pair.rs |
| VESSEL node | crates/agent-vessel/src/node.rs |
| VESSEL CI poller | crates/agent-vessel/src/ci_poller.rs |
| VESSEL conflict handling | crates/agent-vessel/src/conflict_resolver.rs (abort_rebase only) |
| VESSEL notifier | crates/agent-vessel/src/notifier.rs |
| State types and constants | crates/config/src/state.rs |
| Action constants | crates/pocketflow-core/src/action.rs |
| Batch node framework | crates/pocketflow-core/src/batch.rs |
| Flow routing engine | crates/pocketflow-core/src/flow.rs |
| Flow definition (production) | binary/src/bin/real_test.rs |
| Flow definition (dev/dry-run) | binary/src/main.rs |
| Agent registry | orchestration/agent/registry.json |
