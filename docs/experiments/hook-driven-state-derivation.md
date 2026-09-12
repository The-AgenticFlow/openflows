# Hook-Driven State Derivation & Behavior Triggering (Experimental)

**Status:** Experimental design
**Relates to:** Coder `agent-lifecycle-hooks` experiment, OpenFlows `crates/agent-nexus/src/hooks/` consumer, `orchestration/plugin/hooks/`, the Controller flow graph (`agentflow.rs`), and the AgentFlow agents (Nexus / Forge / Sentinel / Vessel / Lore).
**Branch:** `experimental/agent-lifecycle-hooks`

This document widens the existing "observe Coder lifecycle hooks and feed back to Coder"
goal (see `coder-lifecycle-hooks-feedback.md`) into a concrete design for **deriving agent
state and triggering agent behaviour from every lifecycle hook**. It is the working file for
the implementation slices A–D below.

---

## 1. The two layers we already have (recap)

| Layer | Where it runs | Transport | Mutable events | Today's job |
|-------|---------------|-----------|----------------|-------------|
| **Client-side per-role hooks** (`orchestration/plugin/hooks/{role}/*.sh`, declared in `hooks.json`) | inside the worker workspace, run by the CLI agent (Claude Code/Codex) | files + `settings.json` | `exit 2` blocks tool/stop | bootstrap context, bash guard, write check, stop-require-artifact (CLI-backend backstop) |
| **Server-side Coder hook consumer** (`crates/agent-nexus/src/hooks/`) | one deployment-wide HTTP endpoint in the Controller (`POST /hooks/chat`) | JWT-signed POST (HS256) | `user_prompt_submit`, `pre_tool_use` (deny/rewrite) | observe, tail audit (`_hook_events_tail`), passive `apply_policy` gate |

Established invariants (from `coder-lifecycle-hooks-feedback.md` §4.5, D7):
- **Hooks are event hints, not commit proofs.** Durable state lives in Redis; the flow graph
  owns who-works-next ordering. Hooks accelerate and enrich what the 15s Controller poll
  already does; they never replace durable state.
- **The consumer is a pure decision function**, not an agent and not an orchestrator.
- **State derives read-only from Redis**; any "trigger" a hook produces must funnel back
  through durable state so the reconciliation pass stays idempotent.

Everything new in this design is strictly **experimental**: gated by
`CODER_EXPERIMENTS=agent-lifecycle-hooks` **and** `CODER_CHAT_HOOK_URL` (via
`CoderHooksConfig::enabled()`). With the experiment off, behaviour is byte-for-byte the
pre-existing default — no experimental code path changes production flow.

---

## 2. Design principles

1. **One event → (derive read-only state) + (optionally publish a wake hint).**
   Every handler is a pure function of `(payload, current Redis state)`. It never mutates
   durable orchestration state to make a decision — but it may *publish a hint* that tells the
   Controller to poll sooner.
2. **The pub/sub kick channel is a wake-you-up, not a state write.**
   `post_tool_use` / `stop` may `PUBLISH` a notice to a Redis channel the Controller
   subscribes to, so the Controller shortens its 15s sleep and re-runs its reconciliation pass
   immediately. The pass still reads durable keys, so the result is identical whether it runs
   now or after a poll — only latency differs.
3. **Per-role policy stays per-role.**
   The consumer resolves `chat_id → role` from the stored chat (D1/D2) and applies the
   appropriate policy; the client-side per-role scripts remain the CLI-backend backstop.
4. **Stateful guards read Redis, never carry state in the hook handler.**
   `pre_tool_use` "first write must be the plan" reads `ticket:{T}:status.phase` and
   `pair:{T}:plan` existence; it does not remember anything across invocations.
5. **Fail open on non-mutable, fail closed on mutable.**
   Observation-only events never fail the dispatch; `pre_tool_use`/`user_prompt_submit`
   deny/rewrite still fail closed on bad JWT/malformed body (unchanged).
6. **Context injection is capped** (D9): `session_start` `model_context` is trimmed to ≤ 16 KiB.
7. **Dedupe on dispatch** (D7): `dispatch_id` + `(chat_id, event, tool_use_id)`.

---

## 3. Hook × behavior matrix (widened)

| # | Hook | Derives (read-only) | Triggers (behavior) | Slice |
|----|------|---------------------|---------------------|-------|
| 1 | `session_start` | role + ticket from `chat_id`; phase from `ticket:{T}:status`; plan from `pair:{T}:plan`; gate from `ticket:{T}:gate:*`; PR from `ticket:{T}:pr`; dispatch from `ticket:{T}:dispatch:{role}` | Injects **`model_context`** (persona + skills + command refs + resume guidance) into the empty chat so the agent wakes fully briefed | **A** |
| 2 | `pre_tool_use` | tool name/input; role; phase; plan existence | Stateful phase-aware guard: deny/rewrite illegal writes (first-write-to-plan, sentinel readonly, workspace escape, destructive bash, control-plane mutation) | **C** |
| 3 | `post_tool_use` | tool name/input/response; which harness mutation actually ran | Publishes a **kick** so the Controller re-polls and resumes the paused agent (e.g. sentinel verdict → nexus resumes forge) without the 15s round-trip | **B** |
| 4 | `pre_compact` | current phase | Re-assert phase freshness + persist `ticket:{T}:hooks:last_compact` snapshot so resume survives compaction | widened (cheap, bundled with A) |
| 5 | `post_compact` | dispatch/plan/handoff resolvability | No-op audit (ensures resume inputs still present) | widened (cheap) |
| 6 | `stop` | tool input (`subagent`, `finish`, reason); role; phase; PR; handoff; workers | **Status-aware authorized stop**: decides "anticipated" (planning gate / review_ready / blocked / fuel_exhausted) vs premature; records `ticket:{T}:hooks:stop`; publishes a kick so Nexus reconciles the paused/stopped agent promptly | **D** |
| 7 | `user_prompt_submit` | prompt parts | Guard against rewriting away attachments (existing); optionally attach prompt-derived intent to audit | widened (low priority) |

Additional widened use cases (beyond the four slices, noted for completeness):
- **Subagent correlation** (D6): even without dedicated Coder subagent hooks, correlate via
  `parent_chat_id`/`root_chat_id` in `post_tool_use`/`stop` so finishing a subagent can kick
  its parent chat. (Future slice E.)
- **Denial feedback loop**: accumulate `ticket:{T}:hooks:denied` so Nexus can escalate
  repeated policy violations (thread into the existing command-gate style routing).
- **Hook-as-liveness**: events corroborate `heartbeat:{role}-T-{T}`.
- **Per-ticket audit ring**: `ticket:{T}:hooks:events` for debugging/resume, complementing
  the global `_hook_events_tail`.

---

## 4. The pub/sub kick channel (slice B foundation)

Both slices B and D need the Controller to wake early. The channel is modeled so it also works
with the in-memory `SharedStore` (dev/tests) via a process-local broadcast, and with Redis via
real `PUBLISH`/`SUBSCRIBE`.

**Channel name:** `openflows:{tenant}:hooks:kick` (namespaced per tenant).

**Payload (one JSON object, small):**
```json
{
  "chat_id": "coder:chat:...",
  "dispatch_id": "...",
  "event": "post_tool_use",
  "role": "sentinel",
  "ticket_id": "T-42",
  "hint": "verdict_written"
}
```

**Publisher** — the hook consumer, after persisting its audit for a *durable-state-mutating*
event (`post_tool_use` that ran a harness `status set`/`review submit`/`gate approve`/
`pr opened`/`handoff write`/`plan write`, or a `stop`).

**Subscriber** — the Controller loop. In `run_controller`, before `loop`, spawn a subscription
task that pushes received kicks into a `tokio::sync::mpsc` or `watch` channel. The poll loop
becomes:
```
loop {
    flow.run(&store)   // one reconciliation pass
    // wait either for the full poll interval OR the next kick, whichever first
    tokio::select! {
        _ = tokio::time::sleep(CONTROLLER_POLL_INTERVAL) => {}
        Some(kick) = kick_rx.recv() => {
            info!(?kick, "hook kick woke the Controller early");
        }
    }
}
```

**Guarding the wake:** the pass is idempotent, so an early wake is always safe. To avoid a
livelock where a kick arrives every pass and the loop never sleeps, cap early-wake frequency
(e.g. only honor a kick if the last pass > 1s ago, else fall through to the sleep).

### SharedStore API addition
Add to `pocketflow_core::SharedStore`:
- `pub async fn publish_hook_kick(&self, payload: Value)` — no-op / local-broadcast on
  in-memory, `PUBLISH` on Redis. A `no_subs` result is fine (nobody listening → just latency).
- `pub fn subscribe_hook_kicks(&self) -> impl Stream<Item=Value>` or a
  `HookKickSubscriber` struct the Controller can `recv()` on.

This keeps the wake mechanism transport-agnostic and testable.

---

## 5. Slice A — `session_start` → `model_context` bootstrap

**Goal:** when a role's empty chat starts (or resumes), the consumer returns injected context
so the agent realizes its environment and its current state — persona, the skills/commands
paths it may rely on, and exact resume guidance from Redis.

### Resolution flow (per `session_start` dispatch)
1. Verify JWT (existing) → decode `HookPayload`.
2. Resolve role + ticket: `chat_id` → `ticket:{T}:chat:{role}` (or `create_chat` label). If
   unresolvable, return observation-only (fail open).
3. Read durable state:
   - `ticket:{T}:dispatch:{role}` → task payload (title/body/branch/prompt).
   - `ticket:{T}:status` → `.phase` (and freshness).
   - `pair:{T}:plan` → plan text (for review/implementation roles).
   - `ticket:{T}:gate:planning` → whether the gate is approved.
   - `ticket:{T}:pr` → open PR info for `review_ready`/review roles.
   - `ticket:{T}:handoff` → handoff contract for the evaluating role.
   - `ticket:{T}:hooks:last_compact` / `hooks:events` tail → "you just resumed from a
     compaction / a stop" hint.
4. Assemble `model_context` from:
   - **Persona**: `orchestration/agent/agents/{role}.agent.md` content.
   - **Skills**: role's skill list from `registry.json` (`team[].skills`) expanded to their
     markdown skill file paths under `orchestration/plugin/skills/`.
   - **Commands**: `orchestration/plugin/commands/*.md` paths (point the agent at the
     harness command docs).
   - **Resume guidance**: current phase + "you are resuming; here is what was left".
5. Trim to ≤ 16 KiB (D9); put it in `HookDecision.rewrite`-parallel `model_context`. The
   Coder schema returns it via the session-start `model_context` field (not `input_override`).

### Wiring
`start_lifecycle_hook_server` gains an optional `HookBootstrapContext` (persona dir, skills
dir, commands dir, registry) passed in by the Controller at boot from the already-resolved
`OrchestrationResolver` + `registry`. The consumer's `HookServerState` carries it; only used
for `session_start`.

---

## 6. Slice B — `post_tool_use` → reactive kick (sentinel → nexus resumes forge)

**Goal:** remove the slow nexus round-trip the user called out. After Sentinel writes its
verdict to the shared store, a `post_tool_use` fires and notifies Nexus to read the status and
**forward the routing chain immediately** instead of waiting for the next poll.

### Resolution flow (per `post_tool_use`)
1. Verify JWT → decode. Inspect `data.tool_name` + `tool_input` + `tool_response`.
2. If the tool was a **harness coordination mutation** (`openflows-harness status set`,
   `review submit`, `gate approve`, `pr opened`, `handoff write`, `plan write`, `merge done`):
   - Persist audit (existing).
   - Resolve `chat_id → role/ticket`.
   - Derive the *meaning* of the mutation (e.g. `review submit` from a sentinel chat →
     a review verdict just landed at `ticket:{T}:review:sentinel`).
   - **Publish a kick** to the channel (section 4).
3. The Controller wakes, re-runs its pass, `SentinelNode.post` consumes
   `ticket:{T}:review:sentinel`, returns `review_approve`/`review_reject`, and the flow graph
   routes nexus→forge (resume paused forge) or forge→nexus→vessel. Exactly what the 15s poll
   would do, but triggered the instant the verdict lands.

### Correctness
- The verdict is a **durable write the harness already made**; the kick only shortens latency.
  Crash between publish and pass → next poll does the same work. Idempotent.
- `stop` (slice D) uses the identical kick path for "forge paused waiting for sentinel" /
  "pending PR" reconciliation.

---

## 7. Slice C — `pre_tool_use` stateful phase guard

**Goal:** the user's "first write is to be plan" enforced centrally, plus per-role guards:
the write guard confirms the write via `post_tool_use`, and the guard reasons in all
directions (tool, role, phase, plan existence).

### Resolution flow (per `pre_tool_use`)
1. Resolve role from `chat_id`.
2. Read current phase `ticket:{T}:status.phase` + plan existence `pair:{T}:plan` +
   gate `ticket:{T}:gate:planning` (only for forge planning).
3. **Forge, phase = `planning`, tool ∈ {Write, Edit, Create, Patch}:**
   - If no plan exists → the first/next write target must be the plan path
     (`PLAN.md`, or `pair:{T}:plan` upload via harness `plan write`).
     - Write to a non-plan path → **deny** (`permission.decision=deny`,
       `user_message` = "Forge must write PLAN.md first (openflows hook policy)").
     - Write to the plan path / `plan write` harness call → **allow**; record
       `ticket:{T}:hooks:first_write` fact (audit, not durable gating).
   - If a plan already exists (gate approved, resumed) → allow normal writes.
4. **Sentinel:** deny all `Write/Edit/Create/Patch` (readonly reviewer) — this centralizes
   `pre_bash_readonly_guard.sh` + the `deny: [Write, Edit]` in `sentinel.agent.md`. Bash
   commands that are read-only (`git`, `gh`, tests, harness reads) allow; destructive shell
   stays denied via the existing `classify_command`.
5. **Any role, generic policy:** keep existing denies (rm -rf, force-push main, redis-cli,
   control-plane mutation, workspace escape).
6. Return the deny/rewrite in Coder's schema (`permission.decision` / `input_override`).

### Confirmation ("in all directions/senses")
- The denial is decided in `pre_tool_use`; the **confirmation** arrives in `post_tool_use`
  (Slice B path) — the consumer sees whether the allowed write actually hit the plan key and
  can audit "first write completed" for the resume record. Both directions of a single
  tool call are thus observed.

---

## 8. Slice D — `stop` → status-aware authorized stop

**Goal:** the user's "monitor whether it's meaningful for the agent to stop by reading its
status to see why, allowing only anticipated stops (waiting for review, PR submitted)."
Because Coder's `stop` is not mutable server-side (D5), enforcement stays at the client
(`stop_require_artifact.sh`) while the server **monitors, classifies, and accelerates
reconciliation**.

### Resolution flow (per `stop`)
1. Resolve role/ticket; read phase, PR, handoff, worker slot.
2. **Classify the stop:**
   - `planned_handoff` — phase `planning` with gate not approved (forge HALTed for
     sentinel review) OR phase `review_ready` (PR submitted) OR `blocked` with reason OR
     `fuel_exhausted` (worker fuel flag). These are **anticipated**.
   - `premature` — phase not terminal-planning/review_ready/blocked and no PR recorded.
3. **For `planned_handoff`:** record `ticket:{T}:hooks:stop { classification, phase,
   reason, ts }` and publish a **kick** so Nexus reconciles promptly (leaves the forge chat
   paused/resumed appropriately — the "resume the paused forge" lifecycle).
4. **For `premature`:** record the classification but do **not** kick; the client-side
   `stop_require_artifact.sh` (exits 2) is the enforcement, and the Controller's normal
   recovery (FlowRecovery) treats an unexpected stop as a stalled worker on its next poll.

### Interaction with the client stop hook
The client `stop_require_artifact.sh` already refuses a premature stop (exit 2, feeds stderr
back). The server-side `stop` classification is advisory + status-aware: it gives the Control
plane and the human a durable, reasoned record of *why* an agent wanted to stop, matching the
user's "stop hook can be used to monitor the status of the agent… reading its status to see
why it wants to stop."

---

## 9. Experimental hardening (mark all routes experimental)

- All new routes/handlers are behind `CoderHooksConfig::enabled()`; no change to the default
  (non-experiment) control path.
- The consumer already binds only when the experiment is on. New behavior (model_context
  injection, kick publishing, stateful guards) is further gated by per-feature env flags so
  each slice can be toggled independently:
  - `OPENFLOWS_HOOK_BOOTSTRAP=1` (slice A)
  - `OPENFLOWS_HOOK_KICK=1` (slice B)
  - `OPENFLOWS_HOOK_PHASE_GUARD=1` (slice C)
  - `OPENFLOWS_HOOK_STOP_MONITOR=1` (slice D)
- All decisions still tail `_hook_events_tail` (and, per-ticket, `ticket:{T}:hooks:events`).

---

## 10. What changes by file

| File | Change |
|------|--------|
| `crates/pocketflow-core/src/store.rs` | Add `publish_hook_kick` + `subscribe_hook_kicks` (Redis PUB/SUB + in-memory broadcast) |
| `crates/config/src/env.rs` | Add `CoderHooksConfig` feature flags (`OPENFLOWS_HOOK_BOOTSTRAP/KICK/PHASE_GUARD/STOP_MONITOR`) |
| `crates/agent-nexus/src/hooks/types.rs` | Add `HookContext` (role/ticket resolution result), `classify_stop`, new event-payload helpers |
| `crates/agent-nexus/src/hooks/server.rs` | Route `session_start`→bootstrap, `post_tool_use`→maybe-publish-kick, `pre_tool_use`→stateful guard, `stop`→classify+maybe-kick; plus kick publishing helper |
| `crates/agent-nexus/src/hooks/` (new `bootstrap.rs`, `kick.rs`, `guard.rs`, `stop.rs`) | Slice logic modules (kept testable in isolation) |
| `binary/src/bin/agentflow.rs` | Pass `HookBootstrapContext` into hook server; add kick-subscriber to poll loop (`tokio::select!`) |
| `binary/src/orchestration.rs` | Expose skills/commands/persona path helpers for the bootstrap context |
| `crates/agent-nexus/src/hooks/tests.rs` + per-module tests | Unit tests per slice; store pub/sub tests |

---

## 11. Test plan

- **Unit (in-memory store, no Coder):** each module (`bootstrap`, `kick`, `guard`, `stop`)
  decision function tested like today's `server.rs` tests (9 existing tests stay green).
- **Pub/sub:** `SharedStore` in-memory broadcast test (publish → subscriber receives);
  Redis path covered by existing integration harness where available.
- **Slice A:** given a fake `chat_id→role/ticket` resolution + a store with status/plan/gate,
  assert the assembled `model_context` contains persona + skill paths + resume phase and is
  ≤ 16 KiB.
- **Slice C:** forge `planning` with no plan + `Write` non-plan path → deny; with plan path →
  allow; sentinel `Write` → deny; `classify_command` regression stays green.
- **Slice D:** `stop` with `review_ready`+PR → `planned_handoff` + kick published; `stop` with
  mid-build and no PR → `premature`, no kick.
- **Slice B:** `post_tool_use` for `review submit` → kick published with `role=sentinel`,
  `hint=verdict_written`.

---

## 12. Open questions / risks

- **Kick livelock:** early waker needs a minimum-interval guard (see §4).
- **`model_context` 16 KiB cap (D9):** long personas may exceed it; plan to trim/summarize
  and to prefer *paths to* skill/command files over embedding full contents.
- **`pre_tool_use` deny vs flow state drift:** the phase guard reads Redis; if forge is in
  `planning` but a plan already exists on disk (not yet uploaded), the guard could
  false-deny. Mitigate by also treating a harness `plan write`/existing `pair:{T}:plan` as
  evidence of the plan existing, and by keeping the guard advisory (client backstop + reason).
- **Coder `stop` not mutable (D5):** enforcement stays client-side; server is monitor-only.
  Flag as an upstream ask.
