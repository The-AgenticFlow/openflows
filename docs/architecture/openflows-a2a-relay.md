# OpenFlows A2A Relay — Internal Architecture

**Document type:** Internal architecture (deep-dive)
**Scope:** Subsystem 04, Channel C of the OpenFlows system — the A2A relay that enables Sentinel↔Forge delegated verification.
**Companion docs:** `openflows-system-architecture.md` (system-wide, authoritative; see §6 for the communication plane and §7 for the orchestration cycle), `openflows-controller.md` (the host process).

---

## 1. Role & Responsibilities

The A2A relay is a **live task-exchange broker** hosted inside the `openflows-nexus` (Controller) workspace. It implements the A2A (Agent-to-Agent) protocol over JSON-RPC + SSE to let **SENTINEL** (the reviewer) submit a `verify` task that is executed by **FORGE** (the executor) in FORGE's own isolated workspace — without SENTINEL ever gaining shell access to FORGE's filesystem.

The relay is the enforcement chokepoint for this capability:

- **Pair-scoped routing** — tasks flow only within an owning `(pair_id, role)` pair, never across pairs.
- **Command allowlisting** — only a small set of safe test/build command prefixes may be executed.
- **Idempotency** — duplicate submissions are deduplicated on `(pair_id, sha256(request_body))`, but **not back-to-back**: an identical request is only collapsed to an earlier task after several intervening turns (see §4).
- **Audit** — every accepted request, result, and rejection is mirrored to Redis under `audit:a2a:*`.
- **Single kill switch** — one server to disable the whole capability.

The relay does **not** introduce a second source of truth. Every terminal result is **mirrored into Redis** (the SharedStore) before the A2A task is acknowledged complete, preserving the hard rule:

> **Redis remains the single source of truth for durable artifacts. A2A is used only for live task exchange, and every terminal A2A result is mirrored into Redis before the task is acknowledged complete.**

The relay is also deliberately narrow: it only understands the `verify` task type in v1. It is not a general RPC channel, and widening it (allowlist, task types, cross-pair traffic) is a deliberate, reviewed change to the relay — not a runtime configuration a workspace can influence.

---

## 2. Why a relay and not peer-to-peer

Coder workspaces are reliably good at making **outbound** connections and bad at accepting **inbound** ones — there is no stable address for "the FORGE workspace for pair `T-048`" that SENTINEL could dial short of wiring up `coder_app` URLs and a discovery mechanism for every pair.

So the design places a **relay inside the existing `nexus` workspace** and has both SENTINEL and FORGE open **outbound** connections to it. This is not merely a networking convenience; it is what makes the isolation argument hold:

- **Authorization** — nexus is the only place that knows a pair's role map.
- **Command allowlisting** — nexus rejects non-matching `verify` commands before they ever reach an executor.
- **Audit** — nexus is the chokepoint that writes the durable trail.
- **One kill switch** — one relay to disable, not N peer connections.

The alternative (workspaces dialing each other after nexus hands out short-lived tokens) was considered and deferred — it solves nothing for v1 and adds token-exchange + NAT problems.

---

## 3. Components

The relay spans three crates. Each has a distinct responsibility and roughly maps to one task in the implementation plan.

| Component | Crate / file | Responsibility |
|-----------|--------------|----------------|
| **Shared protocol types** | `crates/a2a-protocol` | Serde wire contracts, Redis key helpers, command allowlist |
| **Relay server** | `crates/agent-nexus/src/a2a/` | Axum HTTP + JSON-RPC handlers, routing table, sessions, idempotency, mirroring |
| **Worker client** | `crates/openflows-harness/src/a2a_client.rs` | `reqwest`-based JSON-RPC client used by both SENTINEL and FORGE |
| **Executor sandbox** | `crates/openflows-harness/src/executor.rs` | Process-group-isolated command execution with timeout + output capture |

### 3.1 `a2a-protocol` — the wire contract

Defines the `verify` task shape and the invariants shared by both ends. This crate holds the contract **only**; it implements no server or client behavior.

- `TASK_TYPE_VERIFY = "verify"` — reserved so future task types can be added without breaking this constant (`lib.rs:23`).
- `DEFAULT_COMMAND_ALLOWLIST` — the only command prefixes the relay accepts (`lib.rs:28`):
  ```
  cargo test · cargo build · cargo clippy
  npm test  · pnpm test    · make test · bun test
  ```
- `is_allowlisted(argv)` — `argv` must start with an allowlisted prefix; empty `argv` is always rejected (`lib.rs:40`).
- `VerifyRequest` — Sentinel → relay → Forge payload: `pair_id`, `kind`, `cwd`, `argv`, `timeout_secs`, `env_allowlist`, `expect` (`verify.rs:45`).
- `VerifyResult` — terminal outcome: `task_id`, `exit_code`, `timed_out`, `duration_ms`, `stdout_ref`, `stderr_ref`, `artifacts`, `executor` (`verify.rs:87`).
- `VerifyResult::satisfies(expect)` — pass/fail decision; a **timed-out result never passes** regardless of exit code (`verify.rs:102`).
- `VerifyKind` (`Command` implemented, `ArtifactCheck` reserved) and `VerifyCwd` (`repo` | `worktree`) are enums, not free-form values.
- Redis key helpers: `verification_key`, `audit_task_key`, `audit_rejected_key` (`keys.rs`).

> **Durability contract:** the result's stdout/stderr are **not** sent inline over A2A. Only Redis references (`audit:a2a:{task_id}:stdout` / `:stderr`) travel, capped at the executor's tail (see §7). Large test output never blows up the A2A message size.

### 3.2 The relay server (`agent-nexus/src/a2a/`)

The relay is an **Axum HTTP server** started as a background task by the Controller boot sequence (`agentflow.rs:178` → `start_a2a_relay`). It binds to `A2A_RELAY_ADDR` (default `127.0.0.1:3000`).

```
mod.rs           start_a2a_relay()  — spawn Axum server, return Arc<A2ARelay>
http_server.rs   create_router()    — routes + JSON-RPC handlers + SSE + health/card
routing.rs       A2ARelay           — routing table, tasks, idempotency, buffers, cancel
verify_handler.rs submit_verify_request — validate + dedup + submit
tests.rs         integration/unit tests
```

#### Router surface (`http_server.rs:76`)

| Path | Method | Purpose |
|------|--------|---------|
| `/rpc` | POST | All JSON-RPC methods (`message/send`, `tasks/*`) |
| `/` | GET | SSE progress stream, `?task_id=<uuid>` |
| `/health` | GET | Liveness ("A2A relay healthy") |
| `/.well-known/agent-card.json` | GET | A2A agent card advertising capability + methods |

#### JSON-RPC methods

| Method | Caller | Purpose |
|--------|--------|---------|
| `message/send` | Sentinel | Submit a `verify` request (returns `task_id`) |
| `tasks/get` | Sentinel/Forge | Poll task lifecycle state + terminal result |
| `tasks/claim` | Forge | Claim the next **pending** task for its pair (role-gated) |
| `tasks/complete` | Forge | Submit a terminal `VerifyResult` (task_id + pair guarded) |
| `tasks/cancel` | Sentinel | Cancel a running task (sets cancel flag; synthetic result) |
| `tasks/resubscribe` | Sentinel | Replay buffered events since a sequence number after disconnect |
| `tasks/push_progress` | Forge | Stream a stdout/stderr chunk to SSE subscribers |

#### A2ARelay in-memory state (`routing.rs:151`)

All live task/session state is **in-memory** in the relay process; Redis is only the durable mirror. Arbitrated with async locks:

| State | Type | Purpose |
|-------|------|---------|
| `sessions` | `RwLock<HashMap<(pair_id, role), A2ASession>>` | Registered workspace sessions |
| `tasks` | `Mutex<HashMap<task_id, TaskEntry>>` | Task lifecycle entries |
| `idempotency` | `Mutex<HashMap<key, task_id>>` | Delayed dedup map `(pair_id, sha256(body))` → task (dedups only after several turns, never back-to-back) |
| `idempotency_ts` | `Mutex<HashMap<key, Instant>>` | TTL tracking for idempotency eviction |
| `event_buffers` | `Mutex<HashMap<task_id, EventBuffer>>` | Bounded progress buffers for replay |
| `broadcast_senders` | `RwLock<HashMap<task_id, broadcast::Sender>>` | Live SSE fan-out |
| `cancel_tokens` | `Mutex<HashMap<task_id, Arc<AtomicBool>>>` | Cooperative cancellation flags |

#### Task lifecycle state machine (`routing.rs:32`)

```
Pending ──claim (Forge)──▶ Running ──complete──▶ Completed
   │                          │
   └─────cancel───────────────┴──────cancel──▶ Cancelled
```

`TaskEntry` (`routing.rs:139`) carries `task_id`, the original request, the idempotency key, the requester, the state, and the terminal result once set.

---

## 4. The happy path: SENTINEL verifies FORGE's code

```
SENTINEL workspace      NEXUS A2A relay                FORGE workspace
   │ message/send             │                             │
   │  { verify request }      │                             │
   └──────────────────────────▶│ 1. validate (allowlist,     │
                              │    pair match, timeout)     │
                              │ 2. dedup (pair, sha256)     │
                              │    delayed — not back-to-back │
                              │ 3. create task_id (Pending) │
                              │ 4. mirror request to Redis  │
                              └────────────────────────────▶│ tasks/claim
                                                             │  (role=forge,
                                                             │   pair_id)
                                     tasks/claim ──▶ Running ─┘
                                                             │ claim
                                                             ▼
                                                              spawn process
                                                              (proc-group)
                              ◀── tasks/push_progress ───────┘
                                  (stdout/stderr chunks → SSE)
                              ◀── tasks/complete ─────────────┘
                                  (VerifyResult)
   ◀── SSE stream / tasks/resubscribe ────┘
   ◀── tasks/get (Completed + result) ─────┘
                              │ 5. mirror result to Redis
                              │    pair:P:verification
                              │    audit:a2a:{id}:result
```

Walkthrough:

1. **Submit** — SENTINEL calls `message/send` with a `VerifyRequest` and its pair token. The relay first checks the token hash Nexus registered for that `pair_id`, then `submit_verify_request` (`verify_handler.rs:13`) validates against `validate_verify_request` (`routing.rs:224`):
   - **Pair match** — `req.pair_id` must equal the authenticated requester pair.
   - **Allowlist** — `argv` must match `is_allowlisted`.
   - **cwd** — must deserialize to `Repo` or `Worktree` (serde rejects anything else).
   - **timeout** — must be `> 0` and `≤ 3600` seconds.
   Rejections are logged to `audit:a2a:rejected` and returned as an RPC error.
2. **Dedup** — `check_or_create_task` (`routing.rs:280`) hashes `(pair_id, sha256(body))` as the idempotency key. Crucially, dedup is **not** back-to-back: an immediately repeated identical request creates a **new** task rather than collapsing onto the previous one. Only once an earlier identical request has aged several turns (and fallen outside the active dedup window) does a repeated submission resolve to that earlier task, so a legitimate re-verify of the same command isn't swallowed by the immediately preceding invocation.
3. **Create** — a fresh UUIDv4 `task_id` is generated, `TaskEntry` inserted as `Pending`, the request mirrored to `audit:a2a:{task_id}:request`, and an event buffer + broadcast channel initialized.
4. **Claim** — FORGE's `verify serve` loop polls `tasks/claim` for its pair. The handler rejects any role other than `forge` (`http_server.rs:259`) and marks the next `Pending` task `Running` (`routing.rs:346`).
5. **Execute** — the executor (`executor.rs:43`) spawns the command in its own **process group**, enforces the timeout, and captures stdout/stderr. While running, it may push progress chunks via `tasks/push_progress`, which the relay buffers and broadcasts over SSE (`/`).
6. **Complete** — FORGE calls `tasks/complete` with the `VerifyResult`. Guards check `result.task_id` matches the request's task_id (`http_server.rs:299`) and the caller's pair matches the owning task's pair (`http_server.rs:309`). `complete_task` (`routing.rs:369`) transitions to `Completed`, sets the result, and — critically — **mirrors the result to Redis before releasing the task lock** (§6), so a concurrent `tasks/get` never observes a completed task without a durable result.
7. **Collect** — SENTINEL polls `tasks/get` (or subscribes to SSE) and reads the terminal `VerifyResult`, then decides pass/fail via `satisfies()`.

---

## 5. Concurrency & locking model

The relay is a single process handling many concurrent tasks and clients. All shared state is protected by `tokio` async locks:

- **`tasks`** — `Arc<Mutex<HashMap<...>>>`. Take the lock to read or mutate task entries.
- **`sessions` / `broadcast_senders`** — `Arc<RwLock<HashMap<...>>>` (read-heavy lookups).
- **`idempotency` / `idempotency_ts` / `event_buffers` / `cancel_tokens`** — `Arc<Mutex<HashMap<...>>>`.

Key ordering correctness points:

- `complete_task` **holds the `tasks` lock while mirroring** to Redis (`routing.rs:369-384`), guaranteeing the durability invariant (§6) is atomic relative to `tasks/get` visibility.
- `claim_next_task` finds the first `Pending` task for the pair and flips it to `Running` under one lock acquisition (`routing.rs:346`), so two Forge readers cannot claim the same task.
- `EventBuffer` is `VecDeque`-backed with monotonic sequence numbers and FIFO eviction, bounded to `1000` events or `1 MiB`, whichever is reached first (`routing.rs:63`).

> **In-memory caveat:** all live state (tasks, idempotency map, buffers, sessions) is lost if the relay process restarts. Because results are durably mirrored to Redis and requests are replayable from `audit:a2a:{task_id}:request`, an in-flight verification's *evidence* survives a relay restart even though the live task queue does not. Sentinel must re-derive from Redis state and "when in doubt, don't approve" (§8).

---

## 6. Durability: the mirror-before-ACK invariant

The relay never acks a task as complete until its result is durable in Redis. `mirror_result` (`routing.rs:404`) writes:

| Redis key (tenant-namespaced `ns:{tenant}:...`) | Content |
|--------------------------------------------------|---------|
| `pair:{workspace}:verification` | Latest `VerifyResult` for the pair (note: keyed on the **executor workspace**, e.g. `forge-T-048`) |
| `audit:a2a:{task_id}:result` | Immutable result artifact |
| `audit:a2a:{task_id}:request` | Original request (for replay / resubscribe) |
| `audit:a2a:{task_id}:stdout` / `:stderr` | Bounded output tails (written by the executor) |
| `audit:a2a:rejected` | Most recent rejected request (debugging aid; not append-only) |

`mirror_request` (`routing.rs:429`) writes the request when the task is created so it can be replayed later.

**Failure semantics — "when in doubt, don't approve":**

- **Executor offline** — no FORGE claims the task; it stays `Pending`. SENTINEL sees neither `executor_unavailable` nor a result and must record a `blocked` verdict. Unknown ≠ pass.
- **Timeout** — the executor kills the process group and returns a `timed_out: true` result with `exit_code: None`; `satisfies()` never passes a timeout (`verify.rs:102`).
- **SENTINEL disconnects** — the task keeps running and its result is still mirrored to Redis even if SENTINEL never reconnects. SENTINEL can `tasks/resubscribe` to replay buffered events since a seq number.
- **Duplicate submit (stale retry)** — deduped on `(pair_id, sha256(body))` **after several turns**, so a stale retry of an earlier identical request resolves to the earlier task instead of re-running a test suite. Back-to-back identical requests are treated as distinct and each runs (`is_allowlisted`, same argv) — dedup is a delayed, not immediate, mechanism.
- **Redis down during mirror** — `complete_task` returns an error; the task does **not** transition `Completed` cleanly, and the relay does not ack. An unpersisted result must never approve a gate.

---

## 7. The executor sandbox (`openflows-harness/src/executor.rs`)

`execute_verify_task` (`executor.rs:43`) is what actually runs the command inside FORGE's workspace. Its guarantees:

- **Process-group isolation** — `Command::process_group(0)` creates a fresh process group (`executor.rs:73`) so a `killpg` on the group can cleanly terminate the command *and all its children* (e.g. a test runner spawning subprocesses).
- **Timeout enforcement** — the whole run is wrapped in `tokio::time::timeout(timeout_secs)`. On timeout it `killpg(SIGTERM)`, waits a 5s grace, then escalates to `SIGKILL` (`executor.rs:257`), and produces a `timed_out: true` result.
- **Bounded output** — each stdout/stderr stream is truncated to its **last 10 KB tail** (`truncate_to_tail`, `executor.rs:318`) before persisting to `audit:a2a:{task_id}:{stdout,stderr}`. Progress events are streamed per-line while running.
- **Cooperative cancellation** — a shared `Arc<AtomicBool>` cancel token is checked per output line and before execution begins; when set (via `tasks/cancel`), the executor stops reading and kills the process group.
- **Result persistence** — the executor writes `pair:{tenant}:pair:{pair_id}:verification` and `audit:a2a:{task_id}:result` directly to Redis, then returns the `VerifyResult` for the `verify serve` loop to submit via `tasks/complete`.

Standalone note: when `task_id` is `None` (self-test path), a fresh id is generated; otherwise it always carries the relay-assigned id.

---

## 8. Security model of the relay

| Property | Mechanism |
|----------|-----------|
| **Review integrity** | SENTINEL never touches FORGE's filesystem; it only submits allowlisted commands and reads results. |
| **Command control** | Static allowlist enforced by the relay before dispatch; no `sh -c`, no free-form shell, no `sudo`. |
| **Pair isolation** | Routing and all guards are keyed on `pair_id`; claim is restricted to the `forge` role; submit/claim/get/complete/cancel/progress require the matching pair token. |
| **Durability / audit** | Every request, result, and rejection is mirrored to tenant-namespaced Redis keys. |
| **Single kill switch** | One relay server; `A2A_RELAY_ADDR` bind. |
| **Unguessable IDs** | UUIDv4 task IDs add a layer against accidental cross-talk. |

**Known trust boundary:** Nexus derives a pair token from `CODER_CHAT_HOOK_SECRET` and the ticket id, injects it into the paired SENTINEL/FORGE workspaces as `A2A_PAIR_TOKEN`, and stores only its SHA-256 hash in the relay for validation. This prevents another reachable workspace from claiming a different `pair_id` by self-declaration alone. It does not make FORGE execution a process sandbox: once authenticated, verification still runs with FORGE's workspace permissions, environment, caches, and network access.

The companion principle outside the relay: **SENTINEL must hard-fail — never approve — when a required artifact (PLAN.md, a diff, a persisted verify result) is missing or unreadable.** The relay exists to get evidence *into* SENTINEL's hands, not to excuse approving without it.

---

## 9. Configuration

| Setting | Env var | Default | Notes |
|---------|---------|---------|-------|
| Bind address | `A2A_RELAY_ADDR` | `127.0.0.1:3000` | In the Coder docker deployment, workspaces dial the relay at `openflows-nexus:3000` |
| Pair token | `A2A_PAIR_TOKEN` | none | Injected into Sentinel/FORGE by Nexus; required for all pair-scoped verification RPCs |
| Max task timeout | — | `3600` s (hard cap) | Enforced in `validate_verify_request` (`routing.rs:263`) |
| Output tail | — | `10 KB` / stream | `truncate_to_tail` in the executor |
| Event buffer | — | `1000` events **or** `1 MiB` | FIFO eviction in `EventBuffer` |
| Idempotency TTL | — | `3600` s | `cleanup_idempotency` (`routing.rs:441`) |

The harness client warns if `A2A_RELAY_ADDR` is unset — the loopback fallback (`127.0.0.1:3000`) only works for local testing and will not reach the relay from a provisioned workspace (`a2a_client.rs:33-43`).

---

## 10. Lifecycle & integration with the Controller

1. **Startup** — `start_a2a_relay` is called in the Controller boot sequence (`agentflow.rs:178`). It binds the listener, spawns the Axum server as a background task, and returns `Arc<A2ARelay>`. Failure is non-fatal: the relay logs a warning and the Controller continues (verify requests simply unavailable; see `agentflow.rs:185-188`).
2. **Wiring** — the relay is handed to `NexusNode` via `with_a2a_relay` so NEXUS can check relay health and surface pending verify tasks in orchestration decisions (`agent-nexus/lib.rs:264`).
3. **Workers** — the FORGE workspace template starts the `verify serve` daemon on boot (for FORGE only per §5.1 of the system doc). Both SENTINEL and FORGE use `openflows worker` A2A client operations: `verify request` (Sentinel), `verify serve` (Forge executor loop), and `verify list` (inspection).
4. **Teardown** — the relay lives as long as the Controller/nexus workspace does. Because all durable evidence is in Redis, a relay restart does not destroy the capability's record.

---

## 11. What this deliberately does not do yet

- **No dedicated `verifier` role** — FORGE is the v1 executor, but the schema is executor-agnostic (`executor.role` is a field), so a future dedicated verifier workspace is a routing change, not a protocol change.
- **No arbitrary command execution** — the allowlist is static and small; widening it is a deliberate, reviewed change to the relay.
- **No cross-pair verification** — the routing table is strictly keyed on `pair_id`, matching the isolation guarantee this whole design protects.
- **No process isolation** — FORGE still runs verification with its workspace permissions; the pair token only authenticates relay ownership.
- **`ArtifactCheck` / artifact hashing** — reserved but not built; `VerifyResult.artifacts` is populated as `vec![]` (TODO in the executor).

---
