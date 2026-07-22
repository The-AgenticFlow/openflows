# Agent Orchestration Flow and Protocol

**Status:** Current implementation reference  
**Scope:** Coder-only OpenFlows controller, PocketFlow routing, agent actions, and SharedStore handoffs

OpenFlows implements an autonomous development team as a paced controller loop over a declared flow graph. Coder governs where agents run: identity, workspaces, model access, chat execution, and isolation. OpenFlows governs how agents coordinate: tickets, workers, routing actions, state recovery, review, merge, and documentation follow-up.

This document describes the protocol the system currently implements.

---

## 1. Runtime Model

The long-lived controller runs inside the `openflows-nexus` Coder workspace.

Required environment:

| Variable | Meaning |
|---|---|
| `CODER_URL` | Coder server URL |
| `CODER_SESSION_TOKEN` | scoped tenant-owner Coder token |
| `REDIS_URL` | Redis SharedStore URL |
| `OPENFLOWS_TENANT` | tenant namespace |
| `GITHUB_REPOSITORY` | target repository, `owner/repo` |

At startup the controller:

1. validates the Coder-only environment;
2. connects to Redis-backed `SharedStore`;
3. materializes or validates bundled orchestration files;
4. loads `orchestration/agent/registry.json`;
5. builds agent nodes;
6. wires the PocketFlow routing table;
7. runs flow passes forever on a fixed poll interval.

The controller does not spin in a tight idle loop. Each flow pass ends when a node returns a terminal action, an unrouted action, or the explicit pause marker `__pause__`. The outer controller then sleeps for `CONTROLLER_POLL_INTERVAL` before polling again.

---

## 2. Core Protocol Primitives

### Node Lifecycle

Every agent node follows the PocketFlow `prep -> exec -> post` protocol.

| Phase | Purpose |
|---|---|
| `prep(store)` | read SharedStore, external APIs, and local configuration into a structured input |
| `exec(input)` | perform the role's work or compute the role's decision |
| `post(store, output)` | commit state changes and return an `Action` string |

`Flow::run()` treats returned actions as routing signals. There is no hidden router outside the action table.

### Action Strings

Actions are the protocol's control messages. They are small strings like `work_assigned`, `pr_opened`, `review_approve`, or `deployed`.

Special flow markers:

| Marker | Meaning |
|---|---|
| `__stop__` | end the controller flow run |
| `__pause__` | end the current flow pass without routing; the outer controller polls again later |

`__pause__` is used for legitimate waiting states such as "Forge is still working" or "Nexus has no immediate work." It prevents a hot route cycle between nodes while keeping the controller alive.

### SharedStore

SharedStore is the protocol data plane. In production it is Redis-backed. Nodes communicate by reading and writing typed records and ticket-scoped keys rather than by directly calling each other.

Global keys:

| Key | Shape | Purpose |
|---|---|---|
| `tickets` | `Vec<Ticket>` | all tracked GitHub issues and synthetic tickets |
| `worker_slots` | `HashMap<String, WorkerSlot>` | active Forge worker capacity and workspace bindings |
| `pending_prs` | `Vec<Value>` | PRs waiting for Vessel merge processing |
| `command_gate` | map/object | proposed privileged or risky commands awaiting decision |
| `documentation_queue` | queue/list | Lore documentation work |
| `repository` | string | target GitHub repo |
| `ci_readiness` | enum | whether CI exists, is missing, or setup is in progress |
| `registry_json` | string | loaded agent registry snapshot |

Ticket-scoped keys:

| Key Pattern | Purpose |
|---|---|
| `ticket:{id}:workspace:{role}` | Coder workspace assigned to a ticket role |
| `ticket:{id}:dispatch:{role}` | prompt/dispatch payload sent to a role |
| `ticket:{id}:chat:{role}` | Coder Agent Chat ID for a role |
| `ticket:{id}:chat_action:{role}` | last controller action for that chat |
| `ticket:{id}:review:{role}` | review payload written by Sentinel |
| `ticket:{id}:status` | coarse ticket execution status |
| `ticket:{id}:handoff` | Forge handoff metadata |
| `ticket:{id}:diff_status:{role}` | diff/status metadata from Coder chats |
| `ticket:{id}:recovery_attempts` | bounded recovery counter |
| `heartbeat:{role}-T-{ticket_id}` | workspace heartbeat record |

---

## 3. Agent Roles

| Agent | Role | Execution Style | Main Outputs |
|---|---|---|---|
| NEXUS | orchestrator and recovery brain | deterministic state sync plus decision logic | ticket assignments, workspace/chat dispatches, recovery routes |
| FORGE | implementation worker monitor | observes Coder Agent Chats and SharedStore handoffs | `pr_opened`, `failed`, `__pause__` |
| SENTINEL | review gate | reads Sentinel review payloads and follows up with Forge | `review_approve`, `review_reject`, `no_work` |
| VESSEL | merge and CI gate | deterministic GitHub API processing | `deployed`, `deploy_failed`, `ci_fix_needed`, `conflicts_detected` |
| LORE | documentation keeper | documentation generation and docs PRs | `docs_complete`, `no_work` |

Coder Agent Chats do the LLM-driven workspace work. OpenFlows does not treat chat text as routing truth; chat status and structured SharedStore artifacts are translated into action strings by the nodes.

---

## 4. Flow Graph

Current controller graph:

```text
NEXUS
  work_assigned   -> FORGE
  merge_prs       -> VESSEL
  approve_command -> FORGE
  reject_command  -> NEXUS

FORGE
  pr_opened       -> SENTINEL
  failed          -> NEXUS
  no_tickets      -> NEXUS
  suspended       -> NEXUS
  __pause__       -> end pass, poll later

SENTINEL
  review_approve  -> VESSEL
  review_reject   -> FORGE
  no_work         -> NEXUS

VESSEL
  deployed        -> LORE when enabled, otherwise NEXUS
  deploy_failed   -> NEXUS
  ci_fix_needed   -> FORGE
  merge_blocked   -> NEXUS
  conflicts_detected -> FORGE
  awaiting_human  -> NEXUS
  no_work         -> NEXUS

LORE
  docs_complete   -> NEXUS
  no_work         -> NEXUS
```

The happy path is:

```text
NEXUS -> FORGE -> SENTINEL -> VESSEL -> LORE -> NEXUS
```

The controller may skip or re-enter stages depending on recovery state. For example, if `pending_prs` already contains PRs, NEXUS can route directly to VESSEL with `merge_prs`.

---

## 5. Ticket and Worker State

### Ticket

Tickets are synced from GitHub issues and stored as `Ticket`.

| Status | Meaning | Assignable |
|---|---|---|
| `open` | ready for work | yes |
| `assigned` | assigned but not yet fully active | no |
| `in_progress` | active Forge work | no |
| `completed` | implementation done but not necessarily merged | no |
| `merged` | PR merged | no |
| `failed` | retryable failure while attempts remain | yes, until `MAX_ATTEMPTS` |
| `exhausted` | retry budget exhausted | no |
| `awaiting_human` | human intervention required | no |

### WorkerSlot

Workers represent named Forge capacity from the registry, for example `forge-1`.

| Status | Meaning |
|---|---|
| `idle` | available for assignment |
| `assigned` | ticket bound to worker |
| `working` | Coder chat/workspace is actively building |
| `done` | implementation finished and worker can later be recycled |
| `suspended` | command gate or manual decision required |

Worker slots may include a `workspace_id`, binding OpenFlows state to the Coder workspace created for that worker.

---

## 6. NEXUS Protocol

NEXUS is the first node of every flow pass. Its job is to turn world state into the next safe route.

During `prep`, NEXUS:

1. loads the registry and synchronizes worker slots;
2. syncs GitHub issues into `tickets`;
3. checks CI readiness;
4. injects or prioritizes CI setup work when needed;
5. synchronizes open PRs into `pending_prs`;
6. inspects Coder workspaces and chats;
7. reconciles stale tickets, workers, workspaces, chats, and PRs.

During `post`, NEXUS applies the selected decision:

| Decision | Store Effects | Action |
|---|---|---|
| assign work | bind ticket to worker, ensure workspace/chat dispatch | `work_assigned` |
| process pending PRs | preserve PR queue and route merge gate | `merge_prs` |
| approve command | clear command gate and resume worker | `approve_command` |
| reject command | clear command gate and recycle/retry safely | `reject_command` |
| no actionable work | reset idle counter and pause this pass | `__pause__` |

Priority order:

1. recover existing PRs and broken state;
2. handle command gates and human decisions;
3. satisfy CI-first work;
4. assign new implementation work;
5. pause when nothing is immediately actionable.

---

## 7. FORGE Protocol

FORGE is implemented as `ForgePairNode`, a batch node that monitors all tickets assigned to Forge workers.

During `prep_batch`, it selects tickets with `assigned` or `in_progress` status whose worker role resolves to `forge`.

During each item execution, it records which worker/ticket pair is being monitored. The actual coding happens in the Coder Agent Chat running against the worker workspace.

During `post_batch`, FORGE:

1. reads each worker's Coder chat status;
2. updates `ticket:{id}:status` for active builds;
3. checks `pending_prs` for a PR belonging to the ticket;
4. checks handoff/status keys when no PR exists yet;
5. summarizes the batch into one route action.

FORGE returns:

| Condition | Action |
|---|---|
| at least one monitored ticket has a pending PR | `pr_opened` |
| a monitored ticket failed or awaits human input | `failed` |
| monitored work is still running | `__pause__` |
| no assigned Forge tickets exist | `no_tickets` |

The important invariant is that "still working" is not a route back to NEXUS. It pauses the current pass so the outer controller can poll later.

---

## 8. SENTINEL Protocol

SENTINEL reads structured review payloads from:

```text
ticket:{id}:review:sentinel
```

Each review payload contains a verdict such as `approve` or `reject`, plus report metadata.

SENTINEL returns:

| Verdict State | Store Effects | Action |
|---|---|---|
| approved | mark ticket status `approved`, mark Sentinel chat action complete | `review_approve` |
| rejected | send follow-up to Forge chat, archive Sentinel chat, mark complete | `review_reject` |
| no review payloads | no state transition | `no_work` |

Approval routes to VESSEL. Rejection routes back to FORGE so the implementation chat receives the review report and continues work.

---

## 9. VESSEL Protocol

VESSEL is deterministic merge automation. It does not rely on an LLM decision.

During `prep`, VESSEL reads:

| Input | Meaning |
|---|---|
| `repository` | GitHub owner/repo |
| `pending_prs` | PR queue |
| `ci_readiness` | CI availability |

During `exec`, VESSEL fetches each PR, checks or polls CI, detects conflicts, and attempts merge when allowed.

During `post`, VESSEL updates tickets, worker slots, PR queues, and events.

Returned actions:

| Outcome | Action |
|---|---|
| PR merged | `deployed` |
| CI or merge failed | `deploy_failed` |
| Forge should fix CI | `ci_fix_needed` |
| merge conflicts need implementation rework | `conflicts_detected` |
| human intervention required | `awaiting_human` |
| nothing to merge | `no_work` |

`conflicts_detected` routes directly to FORGE, preserving the same worker and PR context where possible.

---

## 10. LORE Protocol

LORE is optional and only added to the graph when enabled in the registry.

It consumes documentation work derived from merged tickets and documentation queue entries. It can generate changelog entries, ADRs, retrospectives, README updates, or documentation synchronization changes.

LORE returns:

| Condition | Action |
|---|---|
| documentation work completed | `docs_complete` |
| no documentation work exists | `no_work` |

Both routes return to NEXUS.

---

## 11. Coder Workspace and Chat Contract

OpenFlows uses Coder as the governed execution substrate.

The contract is:

1. NEXUS assigns a ticket to a worker slot.
2. NEXUS ensures a Coder workspace exists for that worker/ticket role.
3. NEXUS writes dispatch metadata into SharedStore.
4. NEXUS creates or resumes a Coder Agent Chat.
5. The Coder Agent performs workspace work using control-plane governed credentials and tools.
6. The chat writes structured status, handoff, review, PR, and heartbeat artifacts.
7. OpenFlows nodes translate those artifacts into routing actions.

OpenFlows should not depend on free-form chat prose for control flow. The stable contract is SharedStore keys, Coder chat status, GitHub PR state, and action strings.

Chat status interpretation:

| Coder Chat Status | OpenFlows Interpretation |
|---|---|
| `Running` | work is active; update coarse status and pause |
| `Waiting` | chat may be complete or waiting for follow-up, depending on `chat_action` and artifacts |
| `Error` | mark chat interrupted and let NEXUS recovery handle it |
| `RequiresAction` | escalate or mark awaiting human where appropriate |
| `Pending` | no route yet; keep polling |

---

## 12. Recovery Rules

The protocol is designed so the controller can restart or re-enter the graph without losing the pipeline.

NEXUS reconciliation detects:

| Condition | Meaning |
|---|---|
| unmerged PRs | `pending_prs` still has PRs requiring VESSEL |
| orphaned tickets | ticket references a worker that is idle or missing |
| stale workers | worker references a ticket that no longer needs it |
| completed without PR | ticket says implementation completed but PR queue lost the entry |
| crashed workspaces | Coder workspace is unavailable or unhealthy |
| crashed chats | Coder chat is errored or interrupted |

Recovery is conservative:

1. merge or reprocess existing PRs before assigning fresh work;
2. recycle stale worker state only when ticket state proves it is safe;
3. retry failed tickets only within attempt limits;
4. route conflicts back to Forge rather than assigning a fresh worker;
5. pause instead of looping when no immediate route is useful.

---

## 13. Design Invariants

These invariants keep the system understandable:

1. The flow graph is explicit in `binary/src/bin/agentflow.rs`.
2. Nodes communicate through SharedStore and action strings.
3. Coder owns runtime identity, workspace isolation, chat execution, and model governance.
4. OpenFlows owns ticket state, recovery, role protocol, and merge policy.
5. LLM agents may produce artifacts, but deterministic nodes decide routing from structured state.
6. In-progress and idle waiting states return `__pause__`, not a self-looping action.
7. `pending_prs` is the source of truth for merge work still owed to VESSEL.
8. NEXUS always gets a chance to reconcile before new work is assigned.

---

## 14. Source Map

| Area | Source |
|---|---|
| controller graph and poll loop | `binary/src/bin/agentflow.rs` |
| flow engine | `crates/pocketflow-core/src/flow.rs` |
| node lifecycle and pause/stop markers | `crates/pocketflow-core/src/node.rs` |
| shared state types and action constants | `crates/config/src/state.rs` |
| NEXUS orchestration | `crates/agent-nexus/src/lib.rs` |
| FORGE monitoring | `crates/agent-forge/src/lib.rs` |
| SENTINEL review routing | `crates/agent-sentinel/src/lib.rs` |
| VESSEL merge gate | `crates/agent-vessel/src/node.rs` |
| LORE documentation agent | `crates/agent-lore/src/lib.rs` |
| Coder integration architecture | `docs/architecture/openflows-coder-integration.md` |
