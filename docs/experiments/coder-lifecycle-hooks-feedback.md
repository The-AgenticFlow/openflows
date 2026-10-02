# Coder Agent Lifecycle Hooks — OpenFlows Feedback & Discrepancy Report

**Status:** Experimental evaluation
**Relates to:** Coder `agent-lifecycle-hooks` experiment ([chat-lifecycle-hooks.md](https://github.com/coder/coder/blob/main/docs/admin/setup/chat-lifecycle-hooks.md)), OpenFlows `orchestration/plugin/hooks/`, and the OpenFlows Coder Chats API orchestration.
**Branch:** `experimental/agent-lifecycle-hooks`

This document records how Coder's experimental, server-side lifecycle hooks compare to OpenFlows' existing (client-side, in-workspace) hook model, and lists the concrete discrepancies and feedback items to raise with Coder. It is the working file for the *"observe behaviour so we can give feedback"* goal.

---

## 1. The two hook models

### 1.1 OpenFlows' model (what we already have)

OpenFlows ships role-specific, **client-side shell hooks** in `orchestration/plugin/hooks/{role}/` (declare in `hooks.json`; per-role `session_start.sh`, `pre_bash_guard.sh`, `pre_write_check.sh`, `post_write_lint.sh`, `pre_compact_handoff.sh`, `stop_require_artifact.sh`, `subagent_start.sh`, `subagent_stop.sh`). These were authored for a **CLI-agent backend** (Claude Code / Codex): they read `tool_input` from stdin and `exit 2` to **block** the tool / stop, or emit context to seed the session.

Key properties:
- **Where they run:** inside the worker workspace, executed by the agent binary.
- **Transport:** files + `settings.json` wiring (Claude Code plugin manifest).
- **Per-role:** each role gets its own policy scripts.
- **Execution engine coupling:** only meaningful if OpenFlows runs a CLI agent (deprecated path per §8.4/§11.3 of the system architecture). The default **Coder Chats API** agent does **not** execute these scripts.

### 1.2 Coder's experimental model (what we evaluated)

Coder's `chatd` (the agent-loop process inside `coderd`) emits **server-side, deployment-wide HTTP webhook events** at 7 lifecycle points: `session_start`, `user_prompt_submit`, `pre_tool_use`, `post_tool_use`, `pre_compact`, `post_compact`, `stop`.

Key properties:
- **Where they run:** a single shared consumer endpoint (`CODER_CHAT_HOOK_URL`) owned by the operator. Coder does **not** run a separate hook server; the emitter lives in the loop process, the receiver is yours.
- **Transport:** JWT-signed `HTTP POST` (HS256, shared secret `CODER_CHAT_HOOK_SECRET` ≥ 32 bytes). Fail closed on timeout / non-2xx / malformed / bad decision.
- **Not per-role:** one webhook for the whole deployment. Role/ticket context is **not** in the event — the consumer must correlate via `meta.chat_id` → OpenFlows chat labels (role, ticket).
- **Mutable events:** only `user_prompt_submit` and `pre_tool_use` can be **denied** or **rewritten** (`allow` + `input_override`).

---

## 2. What we implemented on this branch

1. **Config (`crates/config/src/env.rs`)** — `CoderHooksConfig` + `EnvConfig.hooks`, all driven by env at startup:
   - `CODER_CHAT_HOOK_URL` (audience for JWT `aud` check)
   - `CODER_CHAT_HOOK_SECRET`
   - `CODER_CHAT_HOOK_TIMEOUT` / `_ENABLED` / `_ALLOW_INSECURE`
   - `OPENFLOWS_HOOK_ADDR` (where the consumer binds)
   - `enabled()` also requires `CODER_EXPERIMENTS=agent-lifecycle-hooks`.

2. **Consumer (`crates/agent-nexus/src/hooks/`)** — the OpenFlows-side receiver, mirroring Coder's `codersdk/x/agenthooks`:
   - `types.rs`: real Coder wire envelope `{type, meta{dispatch_id,schema_version,chat_id,owner_id,workspace_id,turn_id,parent/root}, data}` + deny/rewrite decision.
   - `jwt.rs`: verifies HS256, `iss`, `aud`==URL, `exp`, `nbf`, `jti`==dispatch_id, `type`==body type, `sub`==`coder:chat:<chat_id>`, `body_sha256`.
   - `server.rs`: Axum `POST /hooks/chat`; verifies, observes, persists a tenant-namespaced audit tail (`ns:{tenant}:_hook_events_tail`), and exposes `apply_policy()` as the centralized policy seam.

3. **Controller wiring (`binary/src/bin/agentflow.rs`)** — `start_lifecycle_hook_server()` on boot; no-op unless the experiment + URL are configured. `openflows hooks simulate` signs + POSTs a sample event for offline testing.

4. **Client-side migration (`crates/provisioner/src/provision.rs`)** — `provision_role()` now also materializes `orchestration/plugin/hooks/{role}/*.sh` into the workspace (`.agents/hooks/{role}/` and `.claude/hooks/{role}/`) and writes a Claude Code style `settings.json` mapping the OpenFlows hook stems to canonical event names. This restores the Codex/Claude-Code-style plugin loading for a CLI backend.

All config is read from environment at startup — no code change needed to enable/disable.

---

### Consumer timeout containment

After JWT verification, the consumer limits event processing to half of
`CODER_CHAT_HOOK_TIMEOUT` (milliseconds, default 1500), leaving the remaining
budget for transport. Configure this value to match the Coder dispatch deadline.
Audit writes run separately with the same bounded lifetime and log failures.
Slow observation hooks return HTTP 200 with an empty decision. Slow mutable hooks
return HTTP 200 with a policy denial that asks the agent to retry; they never
implicitly authorize a tool because a dependency is unavailable. Authentication
and payload errors still fail closed.

Timed-out work is cancelled, so advisory feedback, audit entries, or reactive kicks
may be lost; durable lifecycle state and controller polling remain authoritative.
This contains dependency stalls inside a reachable consumer. Consumer outages,
network failures, and runtime starvation can still produce a Coder dispatch error;
the controller's existing same-chat recovery must resume those sessions. A command
timeout is separate from the hook deadline and is never successful test evidence.


## 3. Discrepancies found (feedback to Coder / internal decisions)

### D1 — Role/ticket context is missing from hook events
Coder's event body carries `chat_id`, `owner_id`, workspace/turn ids, but **no role or ticket**. OpenFlows has to re-derive role/ticket from `chat_id` via chat labels — an extra lookup per event. If Coder's event `meta` could **namespace by deployment/user context** (or at least echo chat labels), the consumer could route to per-role policy without a store round-trip.
> **Ask Coder:** surface the chat's labels/role in `meta`, or document that consumers must resolve them (acceptable for now — we use labels).

### D2 — Single deployment-wide webhook, no per-role routing
Coder sends everything to **one** URL. OpenFlows policy is authored **per role** (`forge` vs `sentinel` have different guards). We currently host one consumer and route by resolving role from chat labels (D1). A consumer that needs to fan out to per-role policy must implement that fan-out itself.
> **Internal:** OK — the consumer already segments by role via `apply_policy()`; the client-side model (per-role scripts) stays as the CLI backstop.

### D3 — `session_start` data differs from our bootstrap expectation
Our `session_start.sh` expects to *read* dispatch/phase from Redis and emit it as context. Coder's `session_start` only sends `data.source` (startup/resume/clear) and expects the consumer to return `model_context` **into** the session. That is actually a **cleaner** bootstrap: we can keep the "empty chat" flow and inject live dispatch/phase from the consumer. This is the natural migration of `session_start.sh` → `model_context`.
> **Internal:** adopt Coder's `session_start -> model_context` as the new bootstrap; retire the client-side per-workspace `session_start.sh` for the Coder-agent path.

### D4 — `pre_tool_use` deny/rewrite is the server-side twin of `pre_bash_guard.sh` / `pre_write_check.sh`
The in-workspace scripts `exit 2` to block a Bash command or a write. Coder's `pre_tool_use` achieves the same **centrally** via `permission.decision=deny` or `allow`+`input_override`. The guard logic (deny `rm -rf /`, force-push to `main`, `redis-cli`, control-plane mutation) should migrate into `apply_policy()`.
> **Status:** implemented on this branch. `apply_policy()` now inspects `tool_name` + `tool_input` for `Bash`/`Write`/etc. and returns deny or an `input_override` rewrite, serialized into Coder's response schema (`permission.decision` / `permission.input_override`, `user_message` for the reason). Covered by 9 unit tests.
> **Discrepancy:** Coder's `input_override` JSON-spelling rules (reject repeated/capitalized keys) are Go-side; the consumer must be careful when rewriting to keep key spelling canonical. Also, **MCP/dynamic tools are not covered** by Coder's built-in duplicate-key precheck — the consumer must validate those itself.

### D5 — `stop` vs `stop_require_artifact.sh`
Our `Stop` hook refuses to end the session until a PR/artifact exists. Coder's `stop` is **not mutable** (cannot deny). So the "don't let the agent claim done without a PR" invariant cannot be enforced server-side via Coder hooks today.
> **Ask Coder:** consider making `stop` auditable-and-respondable (even read-only) or add an advisory/deny option, so a gatekeeper can refuse a premature stop.

### D6 — No `SubagentStart`/`SubagentStop` in Coder's event set
Our hooks include `subagent_start`/`subagent_stop`; Coder only correlates subagent chats via `parent_chat_id`/`root_chat_id` and does not emit dedicated subagent hooks.
> **Internal:** correlate via `parent/root` ids; keep client-side subagent hooks for CLI backends.

### D7 — Delivery semantics: best-effort + duplicates vs our Redis-persisted state
Coder is **best-effort** (no queue; one connection retry with the same JWT; logical re-dispatch on retry/recovery). OpenFlows' orchestration is **stateful and idempotent** (Redis reconciliation every pass). So:
- The consumer must **dedupe** (on `dispatch_id` for transport retries, and `(chat_id, event, tool_use_id)` for logical duplicates).
- Events must be treated as **attempt notifications**, not commit proofs — never drive durable state from a single event; always reconcile against SharedStore.
> **Internal:** matches our existing "events are hints, state lives in Redis" design (already how we treat the A2A relay and ticket status).

### D8 — Observability/audit lives only in the consumer
Coder stores no dispatch/decision history. OpenFlows already keeps a typed audit trail (store events + `audit:a2a:*`); the hook consumer must extend that (we persist to `_hook_events_tail`). No discrepancy, but a requirement to note.

### D9 — `model_context` limit and transcript semantics
`model_context` is capped at 16 KiB and is model-only (absent from the user-visible transcript); `user_message` is user-visible. OpenFlows bootstrap context (dispatch + persona) can exceed 16 KiB for large tickets. Plan to **trim/summarize** injected context or split across multiple context-friendly mechanisms.
> **Ask Coder / internal:** confirm limits; we may need to cap `session_start` `model_context`.

### D10 — Deployment-wide single secret, hard-cutover rotation
Coder signs with exactly one `CODER_CHAT_HOOK_SECRET`; rotation is a hard cutover and a failed dispatch fails the chat closed. OpenFlows multi-tenancy would benefit from per-tenant secrets, which Coder does not support for hooks.
> **Ask Coder:** consider per-deployment-namespace signing keys or signed workspace identity, so hooks on a shared Coder deployment can be scoped per tenant.

---

## 4. How to test it end-to-end

Because all config is env-driven, you can exercise the whole path on a dev box without a full Coder deployment:

```bash
# 1) Generate a shared secret
openssl rand -hex 32

# 2) Run the OpenFlows consumer inside the Controller. Point Coder at it:
#    CODER_EXPERIMENTS=agent-lifecycle-hooks
#    CODER_CHAT_HOOK_URL=http://127.0.0.1:3001/hooks/chat
#    CODER_CHAT_HOOK_SECRET=<same secret>

# 3) From another shell, simulate a Coder dispatch (no Coder server needed):
export CODER_CHAT_HOOK_URL=http://127.0.0.1:3001/hooks/chat
export CODER_CHAT_HOOK_SECRET=<same secret>
openflows hooks simulate --event session_start --chat-id chat-abc
openflows hooks simulate --event pre_tool_use --chat-id chat-abc
```

The consumer verifies the JWT (audience == URL, jti == dispatch_id, sub == `coder:chat:...`, body_sha256), logs the event, and appends it to `ns:{tenant}:_hook_events_tail` in Redis. `session_start` and `pre_tool_use` are the two to watch for policy behaviour.

> Note: the Controller consumer only starts when `CODER_EXPERIMENTS=agent-lifecycle-hooks` **and** `CODER_CHAT_HOOK_URL` are present at boot. Without it, `openflows hooks simulate` still works only if a consumer endpoint is listening.

---

## 4.5 Where the consumer fits vs the Controller (pure function, not an agent)

The consumer does **not** run an agent and does **not** orchestrate. It is a pure, stateless
decision function — `apply_policy(&payload) -> HookDecision` — that gates a single lifecycle
event and persists an audit record. The LLM-powered work happens in the worker agents; the
"who works next" ordering is the **Controller's flow graph** (`NexusNode` + PocketFlow), which
owns all agent-to-agent routing and already runs on a 15s poll loop.

| Component | Role in OpenFlows | Runs an LLM? |
|-----------|-------------------|--------------|
| Worker agents (Forge/Sentinel/…) | Do the real work in their workspaces | Yes |
| Hook consumer (`hooks/`) | Pure policy gate + audit on lifecycle events | **No** |
| Controller (Nexus + flow graph) | Orders the agents: who, when, what next | No (rule-based) |

### Worked example — "Forge finished planning; notify Sentinel"

Hooks do **not** notify Sentinel. The flow graph does. Hooks enforce the path and boot the next agent:

1. FORGE model runs `openflows-harness status set planning` → Coder fires `pre_tool_use`.
2. Consumer observes (command is safe → 200). If the model tried `redis-cli` or `git push --force main`, the consumer **denies** — the only legal way to advance the phase is via the harness.
3. The harness writes `ticket:{T}:status = {phase: planning}` to Redis.
4. Next Controller poll: `ForgeNode.post_batch` reads `phase==planning` → returns `ACTION_PLANNING_GATE` → flow graph routes **nexus → sentinel**.
5. `NexusNode` calls `poll_harness_status_and_spawn_agents` → creates a SENTINEL chat.
6. Coder fires `session_start` for Sentinel → consumer returns **`model_context`** (Sentinel persona + dispatch + "review this plan") so Sentinel boots fully briefed.
7. Sentinel reviews, records a verdict; Controller picks it up next poll and routes onward.

So: **the flow graph owns notification/coordination; the consumer owns enforcement and deterministic bootstrap context.**

---

## 5. Verdict / recommended ownership split

| Concern | Owner |
|---------|-------|
| Where agents run (governed, ephemeral workspaces) | Coder |
| Agent execution loop + lifecycle events + signed webhook | Coder (`chatd`) |
| Centralized policy on `user_prompt_submit` / `pre_tool_use` | OpenFlows consumer (`apply_policy`) via Coder webhook |
| Centralized audit trail of decisions | OpenFlows (Redis) |
| Client-side per-role hooks (CLI-agent backstop) | OpenFlows (`orchestration/plugin/hooks/`, now provisioned) |
| `stop`-as-gatekeeper, subagent hooks, per-tenant secrets | **Gaps to raise with Coder** (D5, D6, D10) |

OpenFlows' design ("Coder governs *where*, OpenFlows governs *how*") aligns with Coder's hook model: Coder delivers lifecycle events, OpenFlows owns policy and state. The main gaps to feed back are D5 (`stop` not gateable), D6 (no subagent hooks), and D10 (no per-tenant signing).
