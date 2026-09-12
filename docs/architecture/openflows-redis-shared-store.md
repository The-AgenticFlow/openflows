# OpenFlows Shared Redis Infrastructure — Internal Architecture

**Document type:** Internal architecture (deep-dive)
**Scope:** The shared Redis layer — the `SharedStore` (pocketflow-core) + the `openflows worker` Redis client — that is the **single source of truth for all durable state** in the OpenFlows system. Covers the two-writer model, the tenant key namespace, the durable key map, the phase state machine, gate `GETDEL` semantics, heartbeats, the event ring buffer, the A2A mirror keys, and the security/operational posture.
**Companion docs:** `openflows-system-architecture.md` (system-wide, authoritative; see §5.4 for multi-tenancy and §13 for network policy), `openflows-controller.md` (Subsystem 01, the primary Redis writer), `openflows-worker-workspace.md` (Subsystem 03, the harness client), `openflows-a2a-relay.md` (Subsystem 04, A2A mirroring).

---

## 1. Role & Responsibilities

Redis is the **only durable state store** in the system and the **single source of truth** for how the OpenFlows team is coordinated. Neither the Coder control plane's PostgreSQL, nor GitHub, nor any agent workspace holds durable OpenFlows state — they are views or actions; Redis is the record.

Its responsibilities:

- **Coordinate the team** — tickets, worker slots, assignment dispatches, phase transitions, gate approvals, reviews, PR lifecycle, merge/deploy results.
- **Serialize intent** — the phase state machine and single-use gate tokens are *enforced by the store*, so no agent can skip or forge a step.
- **Carry liveness** — heartbeat keys let the Controller know a worker is alive or silently dead, with no Controller-side "last seen" tracker.
- **Be the durable tail for A2A** — every terminal A2A verification result is mirrored here before it can affect a gate.
- **Hold the live control plane** — the agent registry (`registry_json`) and the halt/target/continue `control` state live here so operators can steer the fleet with no restart.

**The asymmetry holds here too:** Coder owns *identity and workspaces*; OpenFlows owns *coordination state*. Redis is the concrete expression of "OpenFlows governs HOW agents coordinate."

### The two-writer rule

There are exactly **two** processes that ever write to Redis, and nothing else may:

| Writer | What it writes | Why it is trusted |
|--------|----------------|-------------------|
| **The Controller** (`openflows`, `pocketflow-core::SharedStore`) | orchestration state: `tickets`, `worker_slots`, `pending_prs`, `registry_json`, `control:*`, dispatch/chat keys | long-lived, trusted, control-plane process |
| **`openflows worker`** (Harness A, inside the `openflows` binary) | worker coordination: `status`, `gate`, `review`, `handoff`, `pr`, `heartbeat`, and the A2A executor keys | the *only* Redis client inside worker workspaces |

The **Agent Harness (B)** — hooks, commands, skills — never touches Redis; it is the agent-facing surface that funnels every durable action into the `openflows worker` surface's typed commands (see `openflows-worker-workspace.md` §5.6).

> **Design rule (enforced, not aspirational):** `redis-cli` is disallowed. Every write is serde-validated against a schema; a malformed write exits non-zero and is **never** silently accepted. This is what makes "the store is the source of truth" a real guarantee rather than a convention.

---

## 2. Topology & Connectivity

```
                        ┌─────────────────────────────────────────┐
                        │        CODER CONTROL PLANE              │
                        │  (identity · AI Gateway · Chat API)     │
                        └─────────────────────────────────────────┘
                HTTPS / API │
   ┌─────────────────────── ▼ ─────────────────────────────────────┐
   │                   OPENFLOWS CONTROLLER (nexus)                │
   │  ┌─────────────────────────┐     ┌─────────────────────────┐  │
   │  │ SharedStore (pocketflow)│     │ A2A relay (Axum :3000)  │  │
   │  │  writes orchestration   │     │  mirrors results here   │  │
   │  └───────────┬─────────────┘     └───────────┬─────────────┘  │
   └──────────────┼───────────────────────────────┼────────────────┘
                  │                               │
   ┌──────────────▼───────────────┐   ┌───────────▼───────────────┐
   │        REDIS (SharedStore)   │   │  (out-of-band A2A→Redis)  │
   │   ns:{tenant}:*  ← key space │   └───────────────────────────┘
   └───▲──────────▲───────────────┘
       │          │
  FORGE/SENTINEL/VESSEL/LORE  (worker workspaces)
       │  openflows worker (Harness A) — the ONLY Redis client
```

**Connectivity:** workers have restricted egress that permits `redis` (the `REDIS_URL` target) but denies everything else beyond the control plane and GitHub. The Controller and the workers both reach the same logical store; tenant isolation (not a separate Redis instance per tenant) is what keeps them apart (see §3).

---

## 3. The Key Namespace & Tenancy

### 3.1 `ns:{tenant}:` prefixing

Every key is written and read as **`ns:{tenant}:{key}`**. `SharedStore::ns_key()` (`pocketflow-core/src/store.rs:192`) prepends the tenant namespace to the logical key name, so a store instance built for tenant `acme` can structurally never address tenant `globex`'s keys — collision is impossible at the key-name layer, not merely discouraged.

Tenant resolution (`new_redis_with_tenant`, `store.rs:179`), in priority order:

1. An **explicit `tenant` argument** (the Controller/harness wiring passes it explicitly).
2. Else the **`OPENFLOWS_TENANT`** env var.
3. Else **`"default"`**.

The resolved tenant is baked into the store instance, so every operation on that instance uses the same prefix.

### 3.2 Two access modes

| Mode | API | Uses tenant prefix? | Used by |
|------|-----|---------------------|---------|
| **Namespaced** | `get`/`set`/`del`/`keys`/`get_typed`/`set_typed` | Yes — always | The Controller flow and the harness |
| **Raw (admin-only)** | `raw_keys` / `raw_del` | No — full key strings | Tenant enumerate / list / purge admin commands only |

`raw_keys("ns:*")` scans all tenants (used by `openflows status` / `tenant list` / `store list`); `raw_del` is how `tenant remove --purge` and `store purge` wipe a whole `ns:{tenant}:*` keyspace. Raw mode is never used in the normal Controller loop. The `openflows store` command group exposes an explicit manual-cleanup surface: `store list` (per-tenant key counts), `store purge <tenant>` (delete a tenant's whole keyspace), and `store reset <tenant>` (clear only the Controller-owned orchestration keys — `tickets`, `worker_slots`, `pending_prs`, `registry_json`, `ci_readiness`, `repository`, `documentation_queue` — leaving worker gate/review/handoff keys intact), and `store wipe` (delete **every** `ns:*` key across all tenants — a global reset; requires confirmation unless `--yes`).

### 3.3 Why this is sufficient isolation

Redis namespacing guarantees **data-plane** isolation (which process can address which keys). Coder RBAC guarantees **access-plane** isolation (which user/token may reach the control plane and which workspaces). They compose: a user admitted to tenant B's control plane still cannot address tenant A's keys because the store instance for B simply cannot form tenant-A key strings. This is the multi-tenant model of `openflows-system-architecture.md` §5.4.

---

## 4. The SharedStore API (`crates/pocketflow-core/src/store.rs`)

`SharedStore` presents a uniform interface over two backends — an in-memory backend (dev/tests only) and Redis (the sole runtime backend).

| Method | Behavior |
|--------|----------|
| `new_redis(url)` / `new_redis_with_tenant(url, tenant)` | Open a Redis-backed store; resolve tenant (§3.1) |
| `new_in_memory()` / `new_in_memory_with_tenant(t)` | Dev/test backends only; never used at runtime |
| `get` / `set` / `del` | Namespaced JSON value read/write/delete |
| `get_typed` / `set_typed` | Namespaced serde (de)serialization to a Rust type |
| `keys(pattern)` | Namespaced SCAN (matches `ns:{tenant}:{pattern}`) |
| `raw_keys(pattern)` / `raw_del(key)` | Admin un-namespaced scan / delete on full key strings |
| `emit(agent, event_type, payload)` | Append to the in-process event ring buffer (§7) |
| `get_events_since(cursor)` / `event_count()` | Read the ring buffer (TUI tail, LORE `ticket_merged` detection) |

**Values are JSON.** The store serializes `serde_json::Value` / typed `T` to JSON strings; Redis stores opaque JSON. The *schema* of that JSON is the real contract — enforced by the harness on writes and by typed reads (`get_typed<T>`) on the Controller side.

> **Durability note:** the production compose file runs `redis:7-alpine` with `--appendonly yes`, so durable facts survive restarts. The event ring buffer (§7) is the one exception — it is an in-process, non-persisted structure, deliberately kept separate from durable keys.

---

## 5. The Durable Key Map

All keys are shown relative to their `ns:{tenant}:` prefix. This is the complete, authoritative inventory of durable state.

### 5.1 Orchestration state (written by the Controller)

| Key | Type | Purpose |
|-----|------|---------|
| `tickets` | `Vec<Ticket>` | Known tickets ingested from GitHub |
| `worker_slots` | `HashMap<String, WorkerSlot>` | Worker availability + workspace IDs per role/slot |
| `pending_prs` | `Vec<Value>` | PRs awaiting VESSEL (merge/CI/conflict handling) |
| `ci_readiness` | `CiReadiness` | Whether CI workflows exist (GitHub/workspace/local) |
| `repository` | `String` | Current repo (`owner/repo`), used for provisioning |
| `command_gate` | — | Command approval gate state |
| `documentation_queue` | `Vec<Value>` | LORE documentation requests |
| `registry_json` | `String` | **Live agent registry** — control-plane source of truth (system §3.6) |
| `control:*` | — | Control state: `paused \| drained \| targeted \| auto` + `targets` set; `dispatch_budget` |
| `ticket:{id}:chat:{role}` | `String` | Coder chat ID bound to a ticket+role |
| `ticket:{id}:workspace:{role}` | — | Workspace ID for a ticket+role |
| `ticket:{id}:dispatch:{role}` | `DispatchPayload` | Task assignment handed to a worker |
| `ticket:{id}:recovery_attempts` | int | Recovery counter (bounded at 3) |
| `_ci_fix_attempts_*`, `_conflict_attempts_*`, `_merge_blocked_*` | int | Per-PR attempt counters (survive re-add to `pending_prs`) |

### 5.2 Worker coordination state (written by `openflows worker`)

| Key | Type | Purpose |
|-----|------|---------|
| `ticket:{id}:status` | `{phase, role, ts}` | Harness phase object (drives the state machine, §6) |
| `ticket:{id}:gate:{phase}` | `GateApproval` | Single-use gate token (§6.3) |
| `ticket:{id}:review:{role}` | `ReviewPayload` | Review verdict (SENTINEL) |
| `ticket:{id}:deployment` | — | Vessel merge/deploy result |
| `ticket:{id}:handoff` | — | `CONTRACT.md` handoff written by FORGE |
| `heartbeat:{role}:{ticket}` | `HeartbeatRecord` | Liveness beacon (§8) |

### 5.3 A2A mirror keys (written by the harness executor / relay mirror)

| Key | Type | Purpose |
|-----|------|---------|
| `pair:{id}:plan` | — | Planning artifact (A2A plan gate) |
| `pair:{pair_id}:verification` | `VerifyResult` | Terminal A2A verification result (mirrored before ACK) |
| `audit:a2a:{task_id}:request` | `VerifyRequest` | Original request (replay / resubscribe) |
| `audit:a2a:{task_id}:result` | `VerifyResult` | Immutable result artifact |
| `audit:a2a:{task_id}:stdout` / `:stderr` | String | Bounded (10 KB) output tails |
| `audit:a2a:rejected` | — | Most recent rejected request (debug aid) |

> **Hard rule (system §6.3):** A2A lives in the relay for *live task exchange*; Redis is the *single source of truth for durable artifacts*, and every terminal A2A result is **mirrored into Redis before the task is acknowledged complete**. An unpersisted result (`completed_unpersisted`) cannot approve a gate.

### 5.4 Who reads what

| Consumer | Reads |
|----------|-------|
| Controller (`NexusNode`) | `tickets`, `worker_slots`, `pending_prs`, `registry_json`, `control:*`, `ticket:{id}:status`, `ticket:{id}:chat:*`, heartbeat |
| Controller (`VesselNode`) | `pending_prs`, `repository`, `ci_readiness`, `ticket:{id}:deployment` |
| Controller (`LoreNode`) | event ring buffer (`ticket_merged`) + `ticket:{id}:deployment` |
| Harness (FORGE/SENTINEL/VESSEL) | `ticket:{id}:dispatch:*`, `ticket:{id}:status`, `ticket:{id}:gate:*`, `ticket:{id}:handoff` |
| SENTINEL | `pair:{id}:verification` (A2A result), `ticket:{id}:review:*` |

---

## 6. The Phase State Machine & Gate Semantics (enforced by the harness)

The harness's `status_set` enforces a gated phase machine. This is the *security-relevant* part of Redis: the store itself prevents an agent from skipping or forging a step.

### 6.1 The machine

```
planning ──[gate]──▶ building ──▶ testing ──▶ review_ready
    │                                  │            │
    ▼                                  ▼            ▼
 blocked (stuck)                   blocked      awaiting_human (review)
                                                  │
                                                  ▼
                                              merged (VESSEL)
```

### 6.2 Enforced rules

- **Fresh entry** — a new ticket can only enter `planning` (or `blocked`); it cannot skip the gate.
- **First-phase check** — `planning` is the only legal initial state, so an agent cannot jump straight to `building`.
- **Gated transition** — leaving `planning` (to `building`) **requires consuming a SENTINEL approval token**.
- **Approver authorization** — only the `sentinel` role may approve a gate; `authorize_gate_approver` (`crates/openflows-harness/src/store.rs:96`) rejects any non-SENTINEL role (FORGE cannot approve its own plan; case-insensitive).

### 6.3 The single-use gate token (Redis `GETDEL`)

The gate approval is written by the harness (`gate_approve`, `store.rs:286`) as a token key. To leave `planning`, `status_set` consumes that token with Redis **`GETDEL`** (`store.rs:239`) — which atomically reads *and deletes* the key:

- The token is **single-use**: a second attempt to consume it finds nothing and the transition is rejected.
- The consumption is **atomic**: no other process can consume the same token, and there is no read-then-delete race.
- Because only SENTINEL can create the token and the token is single-use, the planning gate is structurally un-skippable.

> **Why this lives in Redis and not in the Controller:** the enforcement must happen at the point of the write, inside the untrusted worker, before the agent can act. The store (via the harness) is the chokepoint — the Controller provisions the ticket into `planning`, SENTINEL writes the token, and the harness refuses `planning → building` without it.

---

## 7. The Event Ring Buffer

The store maintains a fixed-size, in-process FIFO of structured `StoreEvent`s (capped at `1000` slots; oldest dropped) — `store.rs:15,252`.

```
StoreEvent { agent, event_type, payload, ts }
```

- **Written by** every node lifecycle phase (`emit`) and VESSEL's `ticket_merged`.
- **Read by** the TUI tail loop (`get_events_since(cursor)`) and LORE's merged-ticket detection.
- **Not persisted** — it is per-process and reset on restart, by design.

> **The split that matters:** the ring buffer is an *ephemeral audit/trace seam*, not a durable source of facts. Anything that must survive a restart or long periods lives in an **explicit durable key** (e.g. `ticket:{id}:status`, `ticket:{id}:deployment`). System doc §4.3 (controller) states this explicitly: durable facts live in keys; the ring is cheap and short-window.

---

## 8. Heartbeat & Liveness

Heartbeats are how the Controller tells a workspace (and its agent) is alive — or silently dead — **without polling every container**.

### The numbers

| Number | Meaning |
|--------|---------|
| **30s** | write cadence — the daemon refreshes the TTL every 30s |
| **120s** | Redis TTL (`Expiration::EX(120)`, `store.rs:519`) — if the daemon stops, the key self-expires in ≤ ~2 min |
| **90s** | NEXUS staleness threshold — a worker silent >90s is declared **stale** and recoverable |

### The mechanism

- The harness `heartbeat_start` (daemonized, `store.rs:500`) writes `ns:{tenant}:heartbeat:{role}:{ticket}` every 30s with `HeartbeatRecord { ts, ws_id, status: "running" }` and a 120s TTL.
- Because the key is **self-expiring**, NEXUS needs no "last seen" tracker: if the key is present → alive; if gone → dead for at most ~2 minutes.
- NEXUS's `reconcile()` declares a worker **stale after 90s** of silence and treats it as crashed → tear down and re-provision the workspace, re-assign the ticket, **bounded to 3 recovery attempts** before `awaiting_human` escalation (system §7.5).
- `heartbeat_stop` removes the key on clean teardown.

**Why a separate daemon and not the agent writing inline:** the beacon must keep firing while the LLM agent is idle, thinking, or blocked on a gate. A `nohup`'d background process outlives any single agent action, and it preserves the rule that `openflows worker` is the only Redis client in the workspace.

> **Design note (staleness vs. TTL):** NEXUS acts at 90s staleness, *before* the 120s TTL actually expires. In steady state a healthy worker writes every 30s, so 90s staleness is safe; the gap means NEXUS may declare a worker stale slightly before its key disappears — an intentional early-reaction to transient Redis/network blips, at the cost of the occasional needless recovery on a healthy-but-silent worker.

---

## 9. Concurrency & Consistency Model

The system is **not** transactionally atomic end-to-end; it converges. This is a deliberate architectural choice (controller §7.4):

- **Idempotent passes** — the Controller re-reads all state from Redis each poll and nudges it forward; nothing depends on cross-pass memory in the process.
- **Eventual consistency via reconciliation** — a partial failure mid-pass (e.g. a workspace provisioned but the chat not created) is repaired on the *next* pass by `reconcile()`/`inspect_coder_recovery`, not in the failing pass.
- **Where atomicity IS required, Redis primitives provide it:**
  - Gate consumption — `GETDEL` (atomic read+delete, §6.3).
  - Heartbeat TTL — `EX(120)` (self-expiring key).
  - A2A mirror-before-ACK — the relay holds the task lock while writing, so a concurrent consumer never sees a completed task without a durable result (`openflows-a2a-relay.md` §5, §6).

**Self-healing loop:** a flow pass that errors never kills the Controller — it is logged and retried on the next poll. Durable facts in Redis plus next-pass reconciliation are what guarantee eventual correctness.

---

## 10. Security & Operational Posture

| Property | Mechanism |
|----------|-----------|
| **Single source of truth** | Redis is the only durable store; no secondary record exists to drift |
| **Two writers only** | Controller + `openflows worker`; `redis-cli` disallowed; all writes serde-validated |
| **Tenant isolation** | `ns:{tenant}:` prefixing (data-plane) + Coder RBAC (access-plane) |
| **Gate integrity** | Single-use Redis `GETDEL` tokens; only SENTINEL may approve; first-phase `planning` required |
| **Liveness detection** | Self-expiring heartbeat keys; 90s staleness → bounded recovery → `awaiting_human` |
| **A2A durability** | Every terminal result mirrored to Redis before ACK; unpersisted results cannot gate |
| **Admin-only raw access** | `raw_keys`/`raw_del` restricted to tenant enumerate/list/purge commands |
| **Network** | Workers allow `redis` egress and deny everything else (beyond control plane + GitHub) (§2) |
| **Durability** | `redis:7-alpine` with `--appendonly yes` in the reference deployment |

### Operational commands (the CLI surface over Redis)

| Command | Redis effect |
|---------|--------------|
| `openflows status [--tenant] [--json]` | Reads `ns:*` namespaces → `tickets` / `worker_slots` / `pending_prs` |
| `openflows tenant add/list/remove` | Creates/reads/tears down tenant fleet; `remove --purge` does `raw_del` over `ns:{tenant}:*` |
| `openflows tenant clean` | Resets stale `awaiting_human`/`failed` tickets to `Open`, clears recovery + `worker_slots` |
| `openflows gate approve/status` | Writes a gate token via `Harness::gate_approve` / reads `gate_status` |
| `openflows control set-registry <json>` | Writes `registry_json` (validated; malformed rejected) |

> **`openflows gate` reuses Harness A:** the CLI's `gate approve` path goes through `openflows_harness::Harness` (not `pocketflow-core`), so it composes with the same namespacing and `authorize_gate_approver` semantics that the worker harness enforces — one gate model, one write path, on both sides.

---

## 11. Failure Modes

| Failure | Observable effect | Recovery |
|---------|-------------------|----------|
| Redis write rejected (malformed/typed) | Harness exits non-zero; agent reads stderr | Agent retries with correct payload; nothing invalid is ever persisted |
| Redis down during a Controller pass | `flow.run` returns Err | Logged; loop retries next 15s poll (self-healing) |
| Worker goes silent | Heartbeat key TTL expires | NEXUS declares stale at 90s, re-provisions (≤3 attempts) |
| Redis down during A2A mirror | Task does not complete cleanly; `completed_unpersisted` | Unpersisted result cannot approve a gate; request replayable from `audit:a2a:{task_id}:request` |
| Gate token already consumed | `GETDEL` returns nothing; transition rejected | FORGE must wait for a fresh SENTINEL approval — the gate cannot be replayed |
| Whole tenant keyspace corrupt/stuck | `openflows tenant clean` | Resets stale tickets + recovery counters to `Open` for a clean restart |

---

## 12. Related Documents

- `openflows-system-architecture.md` — §6.2 (Channel B: Redis SharedStore), §7 (orchestration cycle), the authoritative overview.
- `openflows-controller.md` — §4 (SharedStore API + durable key map), §7 (paced poll loop & convergence).
- `openflows-worker-workspace.md` — §5.1 (Harness A, the only Redis client), §5.6 (the two writers / coordination contract), §6 (heartbeat).
- `openflows-a2a-relay.md` — §6 (the mirror-before-ACK durability invariant).
- `tenancy` — multi-tenant model and `ns:{tenant}:` namespacing is covered in §3 above and `openflows-system-architecture.md` §5.4.
- `governance` — AI governance and network policy (Redis egress) is covered in `openflows-system-architecture.md` §13.
- `openflows-system-architecture.md` §4 — pinned Coder version and Chats API stability notes.
