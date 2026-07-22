# AI Governance

OpenFlows inherits Coder's governance controls and adds typed-contract enforcement via the harness.

## Coder-Side Controls (admin-configured in dashboard)

| Control | Where | What it does |
|---------|-------|--------------|
| **Model allowlist** | AI Settings → Coder Agents → Models | Only approved LLM providers/models are available. Agents cannot use unapproved models. |
| **Plan mode** | Per-chat or per-role in registry.json | Review-only roles (SENTINEL, NEXUS) run in plan mode — `write_file`/`edit_files` blocked except for plan files. |
| **Spend limits** | AI Settings → Spend Management | Per-user and per-group LLM spend caps in a rolling period. |
| **Audit logging** | Built-in via AI Gateway | Every prompt, tool call, and response is logged with user identity. |
| **Template allowlist** | Agents → Settings → Templates | Restrict which workspace templates agents can provision. |
| **MCP servers** | AI Settings → MCP Servers | Admin-registered external MCP servers with tool allow/deny lists. |
| **Chat retention** | Agents → Settings → Lifecycle | Automatic purging of archived conversations after a retention period. |

## OpenFlows-Side Controls

| Control | Where | What it does |
|---------|-------|--------------|
| **Typed SharedStore validation** | `openflows-harness` | Every Redis write is validated against serde schemas. Malformed writes exit non-zero — agents read stderr and retry. Agents never run `redis-cli` directly. |
| **Role permission modes** | `registry.json` → `plan_mode` | SENTINEL and NEXUS run in plan mode (review-only). FORGE, VESSEL, LORE run in normal mode. |
| **Scoped tenant tokens** | Bootstrap creates per-tenant tokens | The Controller uses a scoped session token (workspace + chat CRUD only, never admin). |
| **Recovery limits** | `NexusNode::reconcile()` | Max 3 recovery attempts per ticket before `awaiting_human` escalation. |

## Network Policy

Worker workspaces have restricted egress:
- **Coder control plane** — required for workspace daemon
- **github.com** — for git push/pull
- **Redis** — for SharedStore coordination (intentional exception to Coder's recommended lockdown)

Everything else is blocked. Workspaces have **no LLM API keys** and **no agent software** — the AI loop runs in the Coder control plane.

## Deferred (not yet built)

- **Agent Firewall** (Coder Premium) — process-level network and command policies
- **Content redaction** — scanning/redacting secrets in agent outputs
- **Per-action approval gates** — requiring human approval before PR open/merge beyond `awaiting_human`
