# OpenFlows Controller — Internal Architecture

**Document type:** Internal architecture (deep-dive)
**Scope:** Subsystem 01 of the OpenFlows system — the OpenFlows Controller.
**Companion docs:** `openflows-system-architecture.md` (system-wide, authoritative; the three design choices — dynamic registry, web UI, default Coder chat agent — are decided in §3.5, §4, and §7), `openflows-worker-workspace.md` (the workspaces this Controller provisions).

---

## 1. Role & Responsibilities

The Controller is the **brain of the fleet**. It is a single, long-lived process that runs inside the `openflows-nexus` Coder workspace (the **control plane**). It is the *only* OpenFlows component that talks to the Coder control-plane API to provision workspaces and create agent chats, and it is the *only* process that writes to the SharedStore (aside from the `openflows worker` surface inside worker workspaces).

Its responsibilities:

- **Ingest** — pull GitHub issues into tickets.
- **Dispatch** — assign assignable tickets to idle worker slots (FORGE, SENTINEL, VESSEL, LORE).
- **Provision** — create Coder workspaces from role templates and bind per-ticket agent chats.
- **Coordinate** — route work between roles via the flow graph and typed Action edges.
- **Recover** — reconcile stale/orphaned/crashed state every pass; bounded retries then human escalation.
- **Escalate** — park `awaiting_human` tickets and notify via configured channels.
- **Host the A2A relay** — the HTTP server enabling Sentinel↔Forge delegated verification.

The Controller does **not** run LLM calls for the agents — that is the Coder AI Gateway's job. Worker workspaces carry **no LLM keys**.

---

## 2. Execution Environment & Entrypoint

**Binary:** `openflows` → subcommand `run` (default). **Source:** `binary/src/bin/agentflow.rs`.

The Controller is fail-fast on required environment (injected by the Coder template, no fallback):

| Variable | Purpose |
|----------|---------|
| `CODER_URL` | Coder server base URL |
| `CODER_SESSION_TOKEN` | Scoped tenant-owner token (chat + workspace CRUD, never admin) |
| `REDIS_URL` | SharedStore connection |
| `OPENFLOWS_TENANT` | Tenant identifier (namespaces every Redis key) |

> **Note:** No `GITHUB_TOKEN`/PAT is used. The controller's GitHub API auth (issue/PR sync, CI checks, worker token resolution) resolves the token solely from the tenant owner's **Coder external-auth token** (`CODER_EXTERNAL_AUTH_<ID>_TOKEN`/`_ACCESS_TOKEN`/`_TOKEN_FILE`). The GitHub repository URL is injected per-tenant by the nexus template (from the `github_repository` coder parameter set during `tenant add`), not supplied by the operator.

Runtime-supplied (injected per-tenant by the nexus workspace template, not operator-set startup env):
| Variable | Purpose |
|----------|---------|
| `GITHUB_REPOSITORY` | Target repo in `owner/repo` form (derived from tenant config, injected into the tenant's nexus workspace) |
| `OPENFLOWS_HOME` | Orchestration files root |
| `A2A_RELAY_ADDR` | A2A relay bind address (default `127.0.0.1:3000`) |
| `OPENFLOWS_REGISTRY_PATH` / `OPENFLOWS_REGISTRY_JSON` | Registry resolution |
| `ARTIFACTS_DIR` | Artifact output directory |
| GitHub auth | Tenant owner's linked external-auth token (`CODER_EXTERNAL_AUTH_*_TOKEN`), injected at runtime |
| `SLACK_WEBHOOK_URL` | Slack notification webhook |
| `DISCORD_WEBHOOK_URL` | Discord notification webhook |
| WhatsApp variables | WhatsApp notification config |

The `run_controller()` boot sequence (`agentflow.rs:147`):

```
 ┌────────────────────────────────────────────────────────────────────┐
 │ 1. Validate environment (fail-fast on missing required vars)       │
 │ 2. Open tenant-scoped SharedStore (Redis)                          │
 │ 3. Start A2A relay (background Axum HTTP :3000)                    │
 │ 4. Install the hosted-version harness file (from the templates)    │
 │    into the workspace at startup — orchestration is a FILE, not a  │
 │    directory                                                        │
 │ 5. Load agent registry → env vars + registry_json store key        │
 │ 6. Construct the five role nodes (Nexus, Forge, Sentinel, Vessel,  │
 │    Lore)                                                           │
 │ 7. Build the Flow graph with typed Action routes                   │
 │ 8. Enter the paced poll loop — one flow pass every 15s, forever,   │
 │    with self-healing (see §7)                                      │
 └────────────────────────────────────────────────────────────────────┘
```

---

## 3. The PocketFlow Runtime (execution model)

The Controller's logic is expressed as a **directed flow graph** of nodes. This is provided by `crates/pocketflow-core`.

### 3.1 The `Node` trait

Every role implements the `Node` trait (`pocketflow-core/src/node.rs`), which enforces a strict three-phase contract:

```rust
trait Node: Send + Sync {
    fn name(&self) -> &str;
    async fn prep(&self, store: &SharedStore) -> Result<Value>;  // READ only
    async fn exec(&self, prep_result: Value) -> Result<Value>;   // external I/O, NO store writes
    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action>; // WRITE + route
}
```

The contract is enforced structurally: `exec()` does **not** receive the store, so a node cannot write mid-computation. `Node::run()` (default method) sequences `prep → exec → post` and emits lifecycle events (`prep_started`, `exec_started`, `post_done`, …) to the store's event ring buffer on each transition — this is the audit/trace seam.

### 3.2 The `Flow` state machine

`Flow` (`pocketflow-core/src/flow.rs`) connects nodes by **Action strings**:

- A node returns an `Action` from `post()`.
- The flow looks the Action up in the current node's **route table** (`action → node_name`).
- If no route exists (an unbound Action with no entry in the route table), the flow **stops for that pass** — but the Controller treats this as an anomaly and **escalates to a human with the action**: the unknown `Action` string is surfaced via `mark_ticket_awaiting_human` + `notify_awaiting_human` so it is visible and auditable rather than silently swallowed (see §10).
- If the Action is `STOP_SIGNAL` (`__stop__`) or `PAUSE_SIGNAL` (`__pause__`), the flow terminates the pass early — routing is skipped.

**Self-healing guards** (return `PAUSE_SIGNAL` instead of crashing):
- `max_steps` — a **hard pass cap** on the total number of node executions a single `flow.run(&store)` pass may perform. Default **10 000**; the Controller sets it to **1000**. Once the pass has executed `max_steps` nodes it stops (pauses) rather than looping forever — the guard, not the logic, is what bounds runaway traversal in one pass.
- `max_visits_per_node` — a **per-node cycle detection** limit: the flow counts how many times a single node is visited within one pass and, once exceeded, pauses to break out of tight loops. Default **20** (the Controller keeps **20**). Because a cycle such as A→B→A→B visits each node once per 2-node swing, this threshold is reached in roughly `2 × threshold` total steps (≈40 steps for a 20-visit cap at 2 nodes per loop), catching ping-pong within a single pass rather than spinning.

Because a paused pass returns `Ok(PAUSE_SIGNAL)`, it is **not** an error — the paced loop simply retries next poll. Idle/in-progress states also *pause* the pass rather than error.

---

## 4. The SharedStore (durable state)

Provided by `pocketflow-core/src/store.rs`. A key-value store with an identical interface on both backends — the **in-memory** backend is used **only for unit/CI tests**, and **Redis** is the sole runtime backend for all real use. The Controller always uses Redis (`SharedStore::new_redis_with_tenant`). There is **no dev-mode in-memory fallback**: any non-test run must connect to a real Redis (`REDIS_URL`), so behavior seen locally matches production.

### 4.1 Tenancy

Tenancy is how a single shared Redis instance keeps every tenant's data hard-isolated from every other tenant's. The mechanism:

- **Key namespacing.** Every key is written/read as **`ns:{tenant}:{key}`**. `SharedStore::ns_key()` prepends the namespace to the logical key name, so no tenant can ever collide with (or read) another tenant's keys by name.
- **Tenant resolution.** `new_redis_with_tenant` derives the tenant from three sources, in priority order: an **explicit `tenant` argument** (passed in by the harness/controller wiring), else the **`OPENFLOWS_TENANT` env var**, else `"default"`. The derived tenant is then baked into *every* operation on that store instance, so a store built for tenant A cannot address tenant B's keys.
- **Interaction with Coder RBAC.** Redis namespacing provides the data-plane isolation (which process can touch which keys), while **Coder RBAC** provides the access-plane isolation (which user/token may reach the control plane at all). They compose: a user allowed into tenant B's control plane still cannot address tenant A's keys.
- **Raw (admin-only) operations.** `raw_keys()` / `raw_del()` operate on the full, already-namespaced key strings and therefore deliberately bypass the prefixing. They are restricted to **admin/CLI** commands — tenant enumerate, list, purge — and are never used in the normal Controller flow.

Concretely: tenant `acme` reading `tickets` hits Redis key `ns:acme:tickets`; tenant `globex` reading `tickets` hits `ns:globex:tickets`. The two can coexist on the same Redis with zero interference.

Raw (un-namespaced) scan/delete helpers — `raw_keys()` / `raw_del()` — operate on full keys and are used only by **admin/CLI** commands (tenant enumerate, list, purge).

### 4.2 API surface

| Method | Behavior |
|--------|----------|
| `get`/`set`/`del` | Namespaced value read/write/delete (JSON) |
| `get_typed`/`set_typed` | Typed serde (de)serialization |
| `keys(pattern)` | Namespaced SCAN |
| `raw_keys`/`raw_del` | Raw scan/delete for admin |
| `emit` / `get_events_since` / `event_count` | **Event ring buffer** (fixed 1000 slots, drops oldest). Every node lifecycle phase pushes a `StoreEvent { agent, event_type, payload, ts }`. Used for auditing and for LORE's `ticket_merged` detection. The ring buffer is **per-process** (not persisted across restart) — durable facts live in explicit keys. |

**How the ring buffer is managed.** The event buffer is a fixed-size, in-memory (per-process) FIFO capped at **1000 slots**. Each event is appended to the tail; when the buffer is full, the **oldest slot is dropped** to make room for the new event (hence "drops oldest"). It is scoped to the running process and is **not persisted** — on restart the buffer starts empty. Consequently it is an ephemeral, short-window audit/trace seam: it is not a source of durable facts. Anything that must survive a restart or long periods is written to an **explicit, durable store key** (e.g. `ticket:{id}:status`, `ticket_merged` detection reads recent events but the underlying `ticket:{id}:deployment` / status keys are the durable record). This split keeps the hot-path ring cheap while durable state lives in the key-value keys enumerated in §4.3.

### 4.3 Durable key map (per tenant)

| Key pattern | Type | Purpose |
|-------------|------|---------|
| `tickets` | `Vec<Ticket>` | Known tickets |
| `worker_slots` | `HashMap<String, WorkerSlot>` | Worker availability + workspace IDs |
| `pending_prs` | `Vec<Value>` | PRs awaiting VESSEL |
| `ci_readiness` | `CiReadiness` | Whether CI workflows exist (GitHub/workspace/local) |
| `repository` | `String` | Current repo (owner/repo), for provisioning |
| `command_gate` | — | Command approval gate state |
| `documentation_queue` | `Vec<Value>` | LORE documentation requests |
| `registry_json` | `String` | **Live agent registry** (control-plane source of truth; see §11) |
| `ticket:{id}:status` | `{phase, role, ts}` | Harness phase object |
| `ticket:{id}:gate:{phase}` | `GateApproval` | Single-use gate token |
| `ticket:{id}:chat:{role}` | `String` | Coder chat ID |
| `ticket:{id}:dispatch:{role}` | `DispatchPayload` | Task assignment |
| `ticket:{id}:review:{role}` | `ReviewPayload` | Review verdict |
| `ticket:{id}:deployment` | — | Vessel merge/deploy result |
| `ticket:{id}:workspace:{role}` | — | Workspace ID for a ticket+role |
| `ticket:{id}:recovery_attempts` | int | Recovery counter |
| `heartbeat:{role}-T-{ticket}` | JSON | Liveness |
| `pair:{id}:plan` | — | Planning artifact (A2A plan gate) |
| `pair:{pair_id}:verification` | — | A2A verification terminal result (mirrored) |
| `audit:a2a:{task_id}:*` | — | A2A audit logs |
| `_ci_fix_attempts_*`, `_conflict_attempts_*`, `_merge_blocked_*` | int | Per-PR attempt counters (survive pending_prs re-add) |

---

## 5. The Flow Graph & Routing

Built in `agentflow.rs:257` with `Flow::new("nexus")`. This is the **single place all possible transitions are declared** — there is no hidden routing logic elsewhere.

```
 START: nexus

   ┌──────┐  work_assigned     ┌────────────┐  pr_opened     ┌──────────┐
   │ nexus │ ────────────────▶│ forge_pair │ ─────────────▶│ sentinel │
   └──────┘  approve_command   └────────────┘                └──────────┘
      │  ▲                      │ planning_gate                │  │
      │  │                      │ review_ready ────▶ nexus     │  │
      │  │                      │ failed ──────────▶ nexus     │  │
      │  │                      │ no_tickets ──────▶ nexus     │  │
      │  │                      │ suspended ───────▶ nexus     │  │
      │  │                      ◀───── review_reject ──────────┘  │
      │  │                      ◀── ci_fix_needed / conflicts ────│────┐
      │  │                                                        │    │
      │  │  sentinel_spawned ─────────────────────────────────────▶│    │
      │  │                                                             │
      │  │  merge_prs     ┌────────┐  deployed    ┌──────┐            │
      │  └───────────────▶│ vessel │ ───────────▶│ lore │            │
      │                    └────────┘              └──────┘            │
      │   reject_command ──▶ nexus   │ deploy_failed ──▶ nexus         │
      │                              │ merge_blocked ──▶ nexus         │
      │                              │ awaiting_human ─▶ nexus         │
      │                              │ no_work ────────▶ nexus         │
      │                              │ ci_fix_needed ──▶ forge_pair ──┘
      │                              │ conflicts_detected ▶ forge_pair
      │                              (lore: docs_complete / no_work ──▶ nexus)
      └──────────────────────────────────────────────────────────────────┘
```

**Edges (node → routes):**

- **nexus** → `forge_pair` on `work_assigned` / `approve_command`; → `vessel` on `merge_prs`; → `sentinel` on `sentinel_spawned`; → self on `reject_command`.
- **forge_pair** → `sentinel` on `pr_opened`; → `nexus` on `planning_gate`, `review_ready`, `failed`, `no_tickets`, `suspended`.
- **sentinel** → `vessel` on `review_approve`; → `forge_pair` on `review_reject`; → `nexus` on `no_work`.
- **vessel** → `lore` on `deployed` (Lore is always enabled, so this branch always routes to Lore, never directly to Nexus); → `forge_pair` on `ci_fix_needed` / `conflicts_detected`; → `nexus` on `deploy_failed`, `merge_blocked`, `awaiting_human`, `no_work`.
- **lore** → `nexus` on `docs_complete` / `no_work`.

> **Note:** `PAUSE_SIGNAL` (`__pause__`) and `STOP_SIGNAL` (`__stop__`) terminate the pass without routing and are **not listed** in the route table — they are handled by the Flow runtime (§3.2).

---

## 6. The Five Role Nodes

### 6.1 NexusNode — the orchestrator (`crates/agent-nexus/src/lib.rs`)

The **root node**. Fields: `persona_path`, `registry_path`, `a2a_relay: Option<Arc<A2ARelay>>`.

**`prep()`** — the bulk of controller work; every pass it:
1. `sync_registry(store)` — reconciles the live registry with `worker_slots`.
2. Resolves `repository` (env or store) and persists it.
3. `sync_issues(store, owner, repo)` → writes `tickets`.
4. `sync_open_prs(...)` → writes `pending_prs` (with de-dupe/skip guards).
5. `check_ci_readiness(...)` → writes `ci_readiness` (detects local/workspace/GitHub CI config).
6. Ticket normalization: auto-resolve unrecognized statuses, drop stale CI-setup tickets, ensure/prioritize the CI-first ticket.
7. Recycles `Done` workers → `Idle` when assignable tickets exist.
8. `Self::reconcile(...)` → `FlowRecovery`, then `inspect_coder_recovery` + `repair_coder_recovery` (crashed workspaces/chats).
9. Re-provisions busy-but-empty workspaces; `create_chat_for_ticket_id` per active worker.
10. `poll_harness_status_and_spawn_agents(...)` — spawns SENTINEL for `planning`/`review_ready`.
11. `mode(mode, prompt) -> ()` — an explicit **mode switch function** the Controller uses to change the control state at runtime. It takes the target `mode` (`paused | drained | targeted | auto`) and an optional `prompt` (human-readable reason/instruction recorded alongside the switch), and persists the new state to `ns:{tenant}:control:*` (see §11.2). The Controller calls this from the web-UI/API path, so a mode change applies on the next poll without restart, and `prep()` consults the resulting control state as part of its decisions.

Returns a JSON decision-set: `tickets`, `assignable_tickets`, `worker_slots`, `open_prs`, `command_gate`, `repository`, `owner`, `repo_name`, `ci_readiness`, `ci_must_go_first`, `flow_recovery`.

**`exec()`** — rule-based decision (LLM runner removed). Yields one of:
- `sentinel_spawned` / `merge_prs` / `no_work` / `PAUSE_SIGNAL` / `work_assigned`.

**`post()`** — applies the decision:
- `merge_prs` → route to Vessel (only if `pending_prs` non-empty).
- `work_assigned` → `recover_orphans`; set ticket `Assigned`; mark slot `Assigned`; `provision_coder_workspace`; `create_chat_for_ticket_id`; `sync_assignment_to_github` (issue assign, once-only comment, label).
- `no_work` → `PAUSE_SIGNAL`.
- `approve_command` / `reject_command` → clear `command_gate`, move slot accordingly.

**Key methods:** `sync_issues`, `sync_open_prs`, `provision_coder_workspace`, `destroy_coder_workspace`, `create_chat_for_assignment`/`create_chat_for_ticket_id`, `resume_chat`, `poll_harness_status_and_spawn_agents`, `spawn_lore_for_merged_tickets`, `check_ci_readiness`, `sync_assignment_to_github`/`post_comment_once`, `recover_orphans`, `reconcile`, `inspect_coder_recovery`/`repair_coder_recovery`, `mark_ticket_awaiting_human`/`notify_awaiting_human`, `release_worker_slot`, gate/phase helpers, and the **A2A relay module** (see §8).

**Recovery structures** (all in `FlowRecovery`): `unmerged_prs`, `orphaned_tickets`, `stale_workers`, `completed_without_pr`, `crashed_workspaces`, `crashed_chats`, each with `has_*` flags and `needs_recovery`.

### 6.2 ForgePairNode — the builder (`crates/agent-forge/src/lib.rs`)

A **thin monitor** over Coder agent chats; the coding intelligence lives in the Coder control plane. Implements `BatchNode` (one item per assigned/in-progress FORGE ticket). Fields: `workspace_root`, `registry_path`.

**`prep_batch()`** — items for `Assigned`/`InProgress` tickets whose worker role is `forge` (ticket id, worker id, workspace id, status).

**`exec_one()`** — no external I/O (pass-through). Debug-logs the ticket/worker pair and returns the item unchanged.

**`post_batch()`** — the real logic. Acquires a `CoderClient` from the store/env, reads `tickets` and `worker_slots`, then iterates each item:

1. **Harness status** — `read_harness_status` (→ `ticket:{id}:status` phase object, deserialized as `HarnessStatus { phase, role, ts }`) and routes:
   - `review_ready` → syncs the harness PR to `pending_prs` (`read_harness_pr_info` + `sync_harness_pr_to_pending`) if PR info exists; otherwise flags `review_ready` (for Sentinel spawn).
   - `blocked` → flags `failed`.
   - `planning` → flags **`planning_gate`** (pending SENTINEL gate review).
   - `building`/`testing` → flags `in_progress`.
   - Any other phase → silently ignored.

2. **Coder chat monitoring** — fetches the chat via `get_chat` then calls `sync_chat_status_to_store`:
   - `Running` → sets `ticket:{id}:status` to `building` (unless already `building`/`planning`).
   - `Waiting` → info-level analysis based on `chat_action` (completed, interrupted, created, etc.); no store write.
   - `Error` → sets `chat_action = "resume_needed"` (first sighting logs diagnostic with last-message metadata; subsequent polls degrade to debug). **⚠ Does not set a routing flag** — crash recovery is handled by Nexus's heartbeat/reconcile path.
   - `RequiresAction` → logs intent to set `awaiting_human` but **does not write to the store or set a routing flag** (known gap; human-blocked agents rely on Nexus stale-heartbeat recovery).
   - `Pending` → debug log only.

3. **Pending PRs / handoff** — reads `pending_prs` and checks if this ticket already has a tracked PR (→ `pr_opened`); otherwise checks for a `ticket:{id}:handoff` key.

4. **Ticket status fallback** — if no harness status or PR detected: `TicketStatus::Failed` → `has_failed`, `TicketStatus::AwaitingHuman` → `has_failed` (intentional conflation; both route to Nexus via `ACTION_FAILED`).

Returns one routing `Action` by priority: `pr_opened` → `planning_gate` → `review_ready` → `failed` → `PAUSE_SIGNAL` (for in-progress or fallback), else `no_tickets` (when `results` is empty).

> **Note:** The `suspended` action is registered in the flow graph route table but is **never emitted** by `post_batch` — it is dead routing reserved for future use.

### 6.3 SentinelNode — the adversarial reviewer (`crates/agent-sentinel/src/lib.rs`)

Implements `Node`. Field: `registry_path`. Constants: `ACTION_REVIEW_APPROVE = "review_approve"`, `ACTION_REVIEW_REJECT = "review_reject"`.

**`prep()`** — reads `tickets` and `worker_slots`; iterates each `Assigned`/`InProgress` ticket:
1. **PR review check** — reads `ticket:{id}:review:sentinel` (→ `ReviewPayload { verdict, report, pr_number }`). If present, adds to `reviewable` with `review_type = "pr_review"`.
2. **Planning gate check** — reads `ticket:{id}:status`. If phase is `planning` and `ticket:{id}:gate:planning` does **not** exist and a sentinel chat exists (`ticket:{id}:chat:sentinel`), adds to `planning_gate_pending`. If gate already approved, skips (handled by ForgePairNode detecting the phase transition).
3. **Chat monitoring** — fetches the sentinel chat status: `Running` → debug log; `Waiting` with no review → info ("may need follow-up"); `Error` → sets `chat_action = "interrupted"`; `RequiresAction` → info log (no store write).

Returns JSON: `{ reviewable, planning_gate_pending }`.

**`exec()`** — passes through `reviewable` entries unchanged and creates synthetic verdict entries for `planning_gate_pending` (with `verdict = "planning_gate_pending"`, `review_type = "planning_gate"`). Returns `{ verdicts, has_reviews, has_planning_gates }`.

**`post()`** — if no reviews and no planning gates, returns `no_work`. Otherwise iterates verdicts:
- **`approve`** → sets `ticket:{id}:status = "approved"`, marks sentinel `chat_action = "completed"`, deletes the review key (`ticket:{id}:review:sentinel`). Sets `any_approved`.
- **`reject`** → reads the review report from the review key. Looks up the **forge chat via the assigned `worker_id`** (e.g. `ticket:{id}:chat:forge-1`, **not** the literal string `"forge"`). Calls `send_rejection_follow_up` (posts the report into the forge chat). Archives the sentinel chat. Sets sentinel `chat_action = "completed"`. Deletes the review key. Sets `any_rejected`.
- **`planning_gate_pending`** → re-reads `ticket:{id}:gate:planning`:
  - If gate is now approved: checks `pair:{id}:plan`. If plan **missing** → hard-fail: writes a `blocked` `ReviewPayload` to the review key, archives sentinel chat, marks `chat_action = "completed"`. If plan **present** → archives sentinel chat, releases the sentinel worker slot to `Idle`, sets `any_planning_approved`.
  - If gate still unapproved: info log ("review in progress — waiting for chat"), pauses.

Returns by priority: `review_approve` → `no_work` (planning gate approved) → `review_reject` → `PAUSE_SIGNAL`.

> **Note on A2A:** `SentinelNode` itself reviews via Coder chats and a plan-artifact gate check — it does not call the A2A `verify` protocol directly. The **A2A relay** (§8) is hosted by Nexus and is what Sentinel/Forge workspaces use for delegated verification.

### 6.4 VesselNode — the DevOps / merge gatekeeper (`crates/agent-vessel/src/`)

The **only** agent allowed to merge/tear down. `lib.rs` is a facade re-exporting `ci_poller`, `conflict_resolver`, `merger`, `node`, `notifier`, `types`. Implements `Node` (in `node.rs`). Fields: `config: VesselConfig`, `client: GithubRestClient`, `poller: CiPoller`, `merger: PrMerger`. Constants: `MAX_CONFLICT_RESOLUTION_ATTEMPTS = 3`, `MAX_CI_FIX_ATTEMPTS = 3`, `ENV_WORKSPACE_ROOT = "AGENTFLOW_WORKSPACE_ROOT"`.

**`prep()`** — reads `repository` (→ owner/repo via `parse_repository`), `pending_prs`, `ci_readiness` (typed as `CiReadiness` enum). If `ci_readiness` is `None` and repo info is available, falls back to `client.has_workflows()` (GitHub API check; defaults to `true` on error).

**`exec()`** — per pending PR: `get_pull_request`; if repo CI is missing, probe the PR's own commit for check suites/runs (handles the "PR adds CI that runs on itself" case); then either `merge_without_ci` (no CI → `CiMissing`) or `process_single_pr` = docs-PR short-circuit → **CI poll** (`poller.poll_until_terminal`) → conflict detection → **merge** (`merger.merge`). Outcomes: `Merged`, `MergeBlocked`, `CiFailed` (with structured `failure_detail`), `CiTimeout`, `CiMissing`, `Conflicts`, `DocsPrClosed`.

**`post()`** handles each outcome:
- **Merged** → emit `ticket_merged`, set ticket `merged`, write `ticket:{id}:deployment`, **destroy Coder workspace** (archive chats + delete), close GitHub issue, remove from `pending_prs`, recycle worker (Done→Idle) → `any_success`.
- **CiMissing** → emit `ci_missing` + `ticket_merged` (merged without CI validation), set ticket `merged_no_ci`, **stop** (not destroy) Coder workspace, close GitHub issue, remove from pending, recycle worker → `any_success`.
- **DocsPrClosed** → log + remove from `pending_prs` (LORE will regenerate on next deployment) → `any_success`.
- **CiFailed / CiTimeout** → if attempts ≥ `MAX_CI_FIX_ATTEMPTS` mark ticket failed, remove from pending → `any_failure`; else write `CI_FIX.md` (structured failure detail), find and reassign a FORGE worker (derived from branch or fallback to idle), increment `_ci_fix_attempts_`, remove from pending → `any_ci_fix`. If no worker available, mark ticket failed → `any_failure`.
- **MergeBlocked** → increment `_merge_blocked_` counter (only if reason looks like a conflict), mark ticket failed → `any_failure`.
- **Conflicts** → if attempts ≥ `MAX_CONFLICT_RESOLUTION_ATTEMPTS` → **escalate to `awaiting_human`** (via `mark_ticket_awaiting_human` + notify), remove from pending → `any_awaiting_human`; else find and reassign worker for conflict rework, increment `_conflict_attempts_`, remove from pending → `any_conflicts`. If no worker available, mark ticket failed → `any_failure`.

Returns action by priority: `awaiting_human` → `conflicts_detected` → `deployed` → `ci_fix_needed` → `deploy_failed` → `no_work`.

Key helpers: `stop_coder_workspace_for_*` / `destroy_coder_workspace_for_*` (teardown + archive chats + clear slot `workspace_id`), `recycle_worker`, `process_single_pr`/`merge_without_ci`/`handle_conflicts` (local git `merge origin/<default>` + `CONFLICT_RESOLUTION.md`, with GitHub `list_conflicted_files` fallback), `assign_worker_for_ci_fix`/`assign_worker_for_conflict_rework`, `find_idle_forge_worker`, `derive_worker_id_from_branch`, attempt counters (`get_ci_fix_attempts`/`increment_ci_fix_attempts`, `get_conflict_resolution_attempts`/`increment_conflict_resolution_attempts`, `increment_merge_blocked_attempts`), `has_any_check_runs`, docs-PR short-circuit (`is_docs_pr`/`close_docs_pr_with_conflicts`), `mark_ticket_failed`/`mark_ticket_awaiting_human`, `reconcile` (startup detect of already-merged PRs), `from_env()` constructor.

### 6.5 LoreNode — the documenter (`crates/agent-lore/src/lib.rs`)

Implements `Node`; one of the **five always-constructed role nodes** — LORE is always enabled, just like Nexus, Forge, Sentinel and Vessel. It is unconditionally added to the flow graph in `agentflow.rs`, so `deployed` always routes from Vessel to Lore (Lore is never bypassed back to Nexus). Fields: `config: LoreConfig`, `adr_generator: AdrGenerator`, `changelog_manager: ChangelogManager`, `readme_manager: ReadmeManager`, `docs_manager: DocsManager`, `retrospective_generator: RetrospectiveGenerator`. Constructors: `new(workspace_root, persona_path)`, `new_with_registry(workspace_root, persona_path, registry_path)`, `from_config(config)`, `from_env()`.

**`prep()`** — calls `get_documentation_tasks` (reads store events filtering `ticket_merged`, skipping docs PRs) → produces `LoreTask` variants (`ChangelogUpdate`, `AdrGeneration`, `Retrospective`, `DocSync`, `ReadmeUpdate`); `get_merged_tickets_from_store` (scans event ring buffer for `ticket_merged` events → `MergedTicketInfo`); `load_persona` (reads persona file from disk, `.ok()` — failure is non-fatal).

**`exec()`** — processes each task locally (file/git):
- `ChangelogUpdate` → `process_changelog_update` (ensure file exists, categorize from PR, add entry).
- `AdrGeneration` → `process_adr_generation` (generate architectural decision record).
- `Retrospective` → returns `NoWork` (stub — not yet implemented).
- `DocSync` → `process_doc_sync`.
- `ReadmeUpdate` → `process_readme_update`.

Failed tasks are logged at `warn` level and included as error outcomes (non-fatal).

**`post()`** — if any tasks produced work (`has_work = true`): collects changed files (changelog, ADRs, etc.), calls `commit_and_push_docs` (on `lore/docs-{timestamp}` branch) then `open_docs_pr` (appends docs PR to `pending_prs`); emits `changelog_updated` and/or `adr_written` events to the ring buffer. **Always returns `docs_complete`** regardless of whether files changed or the PR open succeeded. Returns `no_work` only when `has_work` was false (no documentation tasks existed).

---

## 7. The Paced Poll Loop & Self-Healing

Source: `agentflow.rs:326-348`.

The Controller is a single, long-lived process whose entire behavior is driven by an **infinite heartbeat loop**. It does not react to events reactively; instead it re-evaluates the whole system from scratch on a fixed cadence and nudges state forward each time.

```rust
loop {
    match flow.run(&store).await {
        Ok(action)      => log (self-healing: paused/idle is normal),
        Err(e)          => log error (NEVER kill the controller),
    }
    tokio::time::sleep(CONTROLLER_POLL_INTERVAL).await;  // 15s
}
```

### 7.1 What one pass does

Each iteration of the loop (each **pass**) runs `flow.run(&store)` once — that is, it walks the entire flow graph end to end: **Nexus** `prep → exec → post` (decide + apply), then routes through whichever handoff the Action selects (Forge, Sentinel, Vessel, Lore) before the pass returns. Every pass therefore re-discovers the world: re-syncs GitHub issues/PRs, re-reads the live registry, reconciles worker slots and crashed workspaces, and lets each role advance any in-flight ticket by exactly one step. Nothing about a pass assumes knowledge gained in earlier passes beyond what is persisted in the SharedStore (Redis) — statefulness lives in the store, not in the loop.

### 7.2 Why "paced" (15s) rather than event-driven or tight-loop

The 15s sleep (`CONTROLLER_POLL_INTERVAL`) is what makes the loop **paced, not busy**:

- It amortizes the cost of the work — re-syncing issues, PRs, CI, workspaces — so the Controller is not hammering the Coder API, GitHub API, or Redis hundreds of times a second.
- It gives **transient failures time to self-resolve**. A Redis blip, a Coder timeout, or a GitHub rate-limit is almost always gone by the time the next pass runs 15s later. The loop lets those failures be absorbed instead of turning into a crash spiral.
- Most passes have little or nothing to do (things are idle, or work is mid-flight in a worker). Idle nodes return `PAUSE_SIGNAL`, which is **normal** — it is logged and the pass simply ends. The pace means these no-op passes are cheap.

### 7.3 Self-healing (the "NEVER kill" contract)

The architectural rule is: **a single bad pass must never take down the Controller.**

- If `flow.run` returns `Ok`, it is logged. A `PAUSE_SIGNAL` / idle result is expected and logged as such.
- If `flow.run` returns `Err`, it is **logged, not propagated**. There is no `panic`, no `?` that aborts the loop, no process exit. The error is swallowed for this pass, and the loop simply advances to the next poll.
- Because the loop retries every 15s, a recurring fault keeps being retried until it clears, and a one-off fault is forgotten in 15s. The only way the Controller stops running is being killed externally (deploy, crash of the host, manual stop).

This turns a fragile "fail fast" controller into a **resilient "keep trying" controller**: correctness over the long run comes from convergence across many passes, not from any single pass succeeding.

### 7.4 Converging to consistency

Because each pass is idempotent and re-reads state from Redis, the Controller **converges** toward a consistent state over successive passes rather than requiring transactional atomicity in any one pass. A partial failure mid-pass (e.g. a workspace was provisioned but the chat was not created before a timeout) is repaired by the reconciliation logic in Nexus's `prep()` on the next pass (`recover_orphans`, `inspect/repair_coder_recovery`). This is why the loop can afford to be "fire and forget" per pass — durable facts in Redis plus next-pass reconciliation are what guarantee eventual correctness.

### 7.5 Pause / target / continue control

The paced loop is also the seam through which an operator steers the Controller at runtime without a restart. A `paused | drained | targeted | auto` control state (persisted under `ns:{tenant}:control:*`, see §11.2) is consulted inside Nexus's `prep()` each pass:

- `paused` → Nexus returns `no_work`; in-flight work continues but nothing new is picked up (**graceful halt**).
- `drained` → stop picking up new tickets.
- `targeted` → filter `sync_issues` to a target set (repo / issue / label).
- `auto` → normal operation.

Because the loop re-reads this state every pass, a mode change issued through the web UI / `mode(mode, prompt)` applies on the **next poll** — no restart, no down-time window.

### 7.6 Summary

> The Controller's core loop is: *walk the whole system once, apply one step of progress everywhere, absorb any error, sleep 15 seconds, repeat forever.* Pacing keeps it cheap and lets transient failures dissolve; self-healing guarantees a bad pass never kills the process; per-pass reconciliation in Nexus guarantees eventual consistency; and the control state lets an operator pause/drain/target it live.

---

## 8. The A2A Relay (delegated verification)

Source: `crates/agent-nexus/src/a2a/mod.rs` + `http_server.rs` + `routing.rs` + `verify_handler.rs`.

- **What:** An Axum HTTP server (`start_a2a_relay`, default `127.0.0.1:3000`) run as a background task inside the Controller (Nexus workspace).
- **Why:** Coder workspaces can initiate outbound connections but accept inbound poorly; the relay lets both SENTINEL and FORGE dial **outbound** to it, making NEXUS the enforcement chokepoint.
- **Protocol (v1, pull-based JSON-RPC/HTTP):** SENTINEL submits via `message/send`; FORGE claims via `tasks/claim`, executes, reports via `tasks/complete`; SENTINEL polls `tasks/get` for the terminal state. SSE (`GET /`) is reserved for future streaming.
- **Durability:** every terminal result is **mirrored to Redis** before the task is acknowledged complete (`pair:{pair_id}:verification`, `audit:a2a:{task_id}:*`). A result that cannot be persisted cannot approve a gate (`completed_unpersisted`).
- **Failure semantics ("when in doubt, don't approve"):** FORGE offline → `executor_unavailable` → SENTINEL records `blocked`; timeout → `timed_out:true` never satisfies expectations; duplicate requests deduped by `(pair_id, sha256(body))`.
- **Allowlisting & audit:** only safe command prefixes pass; rejections → `audit:a2a:rejected`; every accepted request/result is durably logged. One relay = one kill switch.

---

## 9. The Coder Integration Layer

`crates/coder-client/src/lib.rs` is the **only** crate that touches the Coder API. The Controller isolates all Chat API and workspace CRUD behind `CoderClient`:

- **Workspaces:** `create_workspace`/`create_workspace_for_user`/`create_role_workspace`, `start/stop/delete_workspace`, `get_workspace`, `wait_for_workspace_ready`/`wait_for_workspace_ssh`, `workspace_exec_*` (legacy/deprecated for CLI spawning), `workspace_read_file`/`write_file`.
- **Chats API (the LLM loop):** `create_chat`, `get_chat`/`get_chat_opt`, `list_chats`, `send_chat_message`, `get_chat_messages`, `archive_chat`, `interrupt_chat`, `list_chat_models`, `create_ticket_chat`, `archive_ticket_chats`.
- **Admin/bootstrap:** `create_first_user`, `login_with_password`, `list_users`, `get_me`, `create_api_token`, `push_template`, `list_templates`, `list_organizations`.
- **Model resolution:** `model_config_id` expects a UUID; `create_ticket_chat` passes `None` and lets the server use the default model (matched against the org-scoped `GET /api/v2/organizations/{organization}/chats/models`).

The Chat lifecycle within the Controller: **provision workspace → create empty chat → SessionStart hook boots the agent with initial context** (the agent is never given a giant hardcoded prompt).

---

## 10. Failure & Recovery Model

`NexusNode::reconcile()` + `inspect_coder_recovery`/`repair_coder_recovery` run every pass and repair:

1. Unmerged PRs not processed by VESSEL.
2. Orphaned tickets (assigned/in-progress but worker idle/missing).
3. Stale workers referencing dead tickets.
4. Completed-without-PR tickets.
5. Crashed workspaces (heartbeat stale > 90s).
6. Crashed chats (status `Error`).
7. Tickets stuck in `planning` without a SENTINEL chat.

**Bounded recovery:** max 3 `recovery_attempts` per ticket → then `awaiting_human` escalation: ticket parked (not repeatedly retried), worker released, `NotificationService` fires to Slack/Discord/WhatsApp (batched: max 1 per channel per ticket per 5 min, fire-and-forget).

A human resolves via: comment/close the GitHub issue, `openflows tenant clean` (resets stale `awaiting_human`/`failed` back to `Open`), or answering directly in the Coder chat.

---

## 11. Control-Plane Design (from the recent decisions)

The system-facing decisions documented in `openflows-system-architecture.md` (dynamic registry, web UI control plane, default Coder chat agent) integrate with the Controller as follows:

### 11.1 Dynamic agent registry (no file)
- **Today:** the Controller loads the registry in `agentflow.rs` (from path/env) and writes `registry_json` into the store (see §4.3). `sync_registry` reconciles `worker_slots` from it every pass.
- **Decision:** the registry becomes **entirely control-plane defined** — the `registry_json` store key is the sole source of truth; the bundled `registry.json` is eliminated. The control path is `openflows control set-registry <json>` / a web-UI endpoint. Because `sync_registry` re-reads the store each pass, a change applies **on the next poll without a restart** and `worker_slots` rescales automatically.
- **Guardrail:** overrides must preserve `effective_instances()` semantics (v1 `instances` vs v2 `max_instances`) so a partial override can never zero-out a role.

### 11.2 Halt / target / continue
- A `control` state (`paused | drained | targeted | auto`) plus a `targets` set (repo / issue / label) is stored in Redis (`ns:{tenant}:control:*`).
- Nexus's `prep()` consults it: `paused` → return `no_work` (graceful halt, in-flight work continues); `drained` → stop picking up new tickets; `targeted` → filter `sync_issues` to the target set.
- Exposed via `openflows control pause|resume|drain|target …` and the web UI.

### 11.3 Default Coder chat agent
- Confirmed: the Controller drives the **Coder Chats API** for every role (see §9). CLI-agent spawning via `workspace_exec` is deprecated (`coder_process.rs` stub). The `cli`/`CliBackend` v1 fields are an escape hatch only.

---

## 12. Security Posture (Controller-specific)

| Property | Mechanism |
|----------|-----------|
| Trust boundary | Controller runs in the long-lived, trusted `openflows-nexus` workspace; workers are ephemeral/untrusted |
| Least privilege | `CODER_SESSION_TOKEN` is scoped to chat + workspace CRUD, never admin |
| No credentials in workers | Controller holds Coder/GitHub tokens; workers hold none |
| Store writers only | Controller (pocketflow-core) + `openflows worker` inside workers; `redis-cli` disallowed |
| Gate integrity | Single-use Redis GETDEL tokens; only SENTINEL may approve |
| Review integrity | Workspace isolation; SENTINEL delegates verification via A2A, never mutates FORGE's tree |
| Audit | Coder audit log + typed SharedStore events + `audit:a2a:*` |

---

## 13. Related Documents

- `openflows-system-architecture.md` — system-wide architecture (this Controller is Subsystem 01; see §7 for the orchestration cycle, §8 for the agent harness, and §10 for extension points).
- `openflows-redis-shared-store.md` — the shared Redis layer this Controller writes to (the SharedStore deep-dive; see §3 for tenancy).
- `openflows-worker-workspace.md` — the worker workspaces this Controller provisions (see §3 for bootstrapping).
- `openflows-a2a-relay.md` — the full A2A JSON-RPC/SSE delegated-verification protocol this Controller hosts.
- `openflows-system-architecture.md` §13 — security model and network policy (governance).
