# OpenFlows Ephemeral Coder Workspace Integration

## Goal

Integrate OpenFlows agents as ephemeral Coder workspaces where:
- Each agent gets its own workspace, created when a ticket enters the pipeline, destroyed when the ticket is merged.
- Workspaces use a persistent volume; each new ticket assignment does a `git pull` on the existing repo rather than a fresh clone.
- Agent-to-agent communication uses the existing Redis SharedStore.
- Nexus also runs inside a Coder workspace (all 5 agents in Coder).
- Self-healing: robust failure detection, recovery, and workspace crash resilience.
- **Use Coder Registry modules** for agent installation, notifications, and workspace tooling — with a configurable module mapping so any agent CLI can be swapped in.
- **Use Coder Agents (Chats API)** for programmatic orchestration instead of the deprecated Tasks API.
- **Support multiple agent CLIs**: Claude Code, OpenAI Codex, Aider, Goose, and future agents via a module mapping system.

## Coder Platform Integration: Key Decisions

### Coder Tasks API → Coder Agents (Chats API)

**Critical finding**: The Coder Tasks API (`/api/v2/tasks`) is deprecated as of June 2, 2026 (moves to ESR for Premium customers; Community deployments lose it immediately). It will be removed from Coder v2.37+. The replacement is **Coder Agents** using the **Chats API** (`/api/experimental/chats`).

Key architectural differences:

| Aspect | Tasks API (deprecated) | Chats API (replacement) |
|---|---|---|
| Agent execution | Runs **inside workspace** (AgentAPI) | Runs in **control plane** (autonomous) or **inside workspace** (CLI) |
| LLM credentials | Injected into workspace env | Stored in control plane only |
| Template requirements | `coder_ai_task`, `coder_task`, agent modules | Autonomous: just a description. CLI: agent module + harness. |
| Workspace provisioning | You specify `template_version_id` | Autonomous: agent auto-selects. CLI: Nexus specifies template. |

**OpenFlows uses CLI mode only** — agents run inside workspaces with the harness binary. The Chats API's autonomous capabilities (control-plane agent, auto-template-selection) are not used.
| Chat state | Stored in workspace | Persisted in Coder database |
| Conversation model | Single prompt + follow-ups | Multi-turn chat with queuing |
| Real-time updates | HTTP polling | WebSocket streaming |
| Sub-agents | Not supported | Built-in delegation |
| Delete task | `DELETE /api/v2/tasks/{user}/{task}` | `PATCH /api/experimental/chats/{chat}` with `{"archived": true}` |

**Decision**: Use the **Chats API** for all programmatic agent orchestration. The Tasks API endpoints should not be implemented.

> **Supersedes prior design**: An earlier sketch proposed a Rust bridge crate (`crates/agentflow-coder-bridge`) using the `rmcp` SDK with NEXUS reporting to Coder's control plane via `coder_ai_task` — built around the AgentAPI/Tasks-style reporting protocol (`coder exp mcp server`). That design is now **obsolete**. The Chats API replaces it entirely. If any `agentflow-coder-bridge` code exists in the codebase, it should be deleted or repurposed; no new work should be invested in the Tasks/AgentAPI reporting path.

### Architectural Decision: Chats API vs AgentAPI

The Coder ecosystem has two agent-control interfaces that must not be conflated:

- **Chats API** (`/api/experimental/chats`) — The **control-plane interface**. Nexus uses this to create, message, and monitor agent chats. The Chats API manages chat lifecycle and state persistence. The agent itself runs inside the workspace (CLI mode), not in the control plane. OpenFlows uses only the Chats API for orchestration — it never calls AgentAPI.

- **AgentAPI** (`coder/agentapi`) — An **in-workspace interface**. The Coder Registry modules (claude-code, aider, etc.) use AgentAPI internally to communicate with the agent process running inside a workspace. OpenFlows does **not** call AgentAPI directly.

**Decision**: OpenFlows uses the Chats API as the sole orchestration interface. AgentAPI is used internally by Registry modules but is never called directly by OpenFlows code. All agents run inside workspaces in CLI mode, communicating via SharedStore (not via AgentAPI).

### Architectural Decision: CLI Mode Only (No Autonomous Mode)

OpenFlows uses **CLI mode exclusively** — the agent binary (installed by the Coder Registry module) runs inside the workspace. The OpenFlows harness binary coordinates via SharedStore with typed, enforced Redis schemas. Nexus creates Coder Chats bound to role-specific workspaces, and the agent processes prompts inside those workspaces.

**Autonomous mode** (where Coder's built-in Go agent processes prompts in the control plane without a workspace-local binary) is **not used** because:
1. Autonomous agents have no knowledge of OpenFlows SharedStore contracts — they would need to redis-cli with hand-crafted commands, which is unreliable.
2. The OpenFlows architecture thesis is "architecture is the product" — typed contracts enforced by harness binaries are core to how agents coordinate.
3. The `post_install_script` in each template sets up the harness environment, ensuring deterministic behavior regardless of which agent CLI is installed.

All production orchestration goes through CLI mode. There is no exploratory/non-coordinated path in the current design.

### Coder Registry Modules: Extensible Agent Module System

The Coder Registry provides Terraform modules that compose into workspace templates. Instead of hardcoding a specific agent module, OpenFlows uses a **module mapping** configured in `registry.json` that maps each agent's `cli` field to a Coder Registry module source.

#### Available agent modules (current)

> **⚠️ Module version verification required**: Only `claude-code` v5.2.0 has been verified against the live Coder Registry. The `codex` module was listed at v1.0.0 in early docs but the current live version is v5.2.1. Aider, Goose, and other module versions must be confirmed against the live registry at `registry.coder.com` before applying Terraform. Task 1.2b adds an explicit verification step.

> **⚠️ Module–Tasks coupling risk**: The `claude-code` v5 module explicitly dropped Coder Tasks/AgentAPI coupling and works cleanly as a standalone CLI installer. However, modules like `aider` are documented as installing Aider *with AgentAPI for Coder Tasks support*, and similar `coder_ai_task` coupling may exist in `amazon-q`, `gemini`, etc. Each module must be individually verified for Chats API compatibility before inclusion. The module-mapping abstraction (1.1–1.2) treats all CLIs as interchangeable plug-ins; in practice each one needs a per-module compatibility check.

| Module | Source | CLI | Description |
|---|---|---|---|
| `claude-code` | `registry.coder.com/coder/claude-code/coder` | `claude` | Anthropic Claude Code CLI. Supports AI Gateway, MCP, managed settings, telemetry. |
| `codex` | `registry.coder.com/coder-labs/codex/coder` | `codex` | OpenAI Codex CLI. Supports model selection, MCP, sandbox modes. |
| `aider` | `registry.coder.com/coder/aider/coder` | `aider` | Aider AI coding assistant. Multi-model support. |
| `goose` | `registry.coder.com/coder/goose/coder` | `goose` | Block Goose AI agent. |
| `amazon-q` | `registry.coder.com/coder/amazon-q/coder` | `amazon-q` | Amazon Q Developer agent. |
| `gemini` | `registry.coder.com/coder-labs/gemini/coder` | `gemini` | Google Gemini CLI. |
| `copilot` | `registry.coder.com/coder-labs/copilot/coder` | `copilot` | GitHub Copilot CLI. |
| `cursor-cli` | `registry.coder.com/coder-labs/cursor-cli/coder` | `cursor` | Cursor CLI agent. |

#### Utility modules

| Module | Source | Purpose |
|---|---|---|
| `slackme` | `registry.coder.com/coder/slackme/coder` | Slack DM on command completion via Coder external auth |
| `git-clone` | `registry.coder.com/coder/git-clone/coder` | Clone repos into workspace with auth |
| `git-config` | `registry.coder.com/coder/git-config/coder` | Configure git user, signing |
| `coder-login` | `registry.coder.com/coder/coder-login/coder` | Authenticate coder CLI in workspace |
| `agentapi` | `registry.coder.com/coder/agentapi/coder` | Agent API runtime for custom agents |
| `agent-firewall` | `registry.coder.com/coder/agent-firewall/coder` | Process-level firewall for agents (Premium) |
| `aibridge-proxy` | `registry.coder.com/coder/aibridge-proxy/coder` | Proxy for AI Gateway |
| `vscode-web` | `registry.coder.com/coder/vscode-web/coder` | VS Code in browser |
| `code-server` | `registry.coder.com/coder/code-server/coder` | Code Server (open-source VS Code) |

#### Module mapping in registry.json

The `registry.json` `cli` field maps to Coder Registry modules. A new `coder_module` field specifies the Terraform module source:

```json
{
  "id": "forge",
  "cli": "claude",
  "coder_module": {
    "source": "registry.coder.com/coder/claude-code/coder",
    "version": "5.2.0",
    "params": {
      "workdir": "/home/coder/workspace",
      "permission_mode": "acceptEdits",
      "enable_ai_gateway": true
    }
  }
}
```

When `coder_module` is not specified, the system falls back to a default mapping based on `cli`:

| `cli` value | Default module | Module version |
|---|---|---|
| `claude` | `registry.coder.com/coder/claude-code/coder` | `5.2.0` (verified) |
| `codex` | `registry.coder.com/coder-labs/codex/coder` | **verify on live registry** |
| `aider` | `registry.coder.com/coder/aider/coder` | **verify on live registry** |
| `goose` | `registry.coder.com/coder/goose/coder` | **verify on live registry** |

This mapping is stored in the Provisioner config and can be extended without code changes. New agent modules from the Coder Registry are adopted by adding a row to the mapping.

#### Common module features used by OpenFlows

All agent modules share these patterns that OpenFlows leverages:

1. **`coder_env` for environment injection** — `REDIS_URL`, `ORCHESTRATOR_DIR`, `CODER_URL`, `CODER_SESSION_TOKEN` are set as env vars. `PROXY_URL`/`LITELLM_PROXY_URL` set only for LiteLLM fallback. When AI Gateway is enabled, no LLM API keys are injected — the session token authenticates through AI Gateway.

2. **`workdir` for project directory** — Set to `/home/coder/workspace` to match our persistent volume mount point. Also pre-accepts the agent's trust/onboarding dialog.

3. **`mcp` for MCP servers** — Injected as JSON config. OpenFlows provides GitHub MCP server config and any custom MCP servers.

4. **`managed_settings` for permissions** — Role-specific: `acceptEdits` for forge, `plan` for sentinel, `plan` for nexus, `acceptEdits` for vessel.

5. **`enable_ai_gateway`** — Enabled by default (Premium license available). Agents route through Coder's AI Gateway. LLM credentials stay in the Coder control plane, never injected into workspace env.

6. **`pre_install_script` / `post_install_script`** — Used for git pull, SharedStore registration, and heartbeat writer.

7. **`telemetry`** — OpenTelemetry export with workspace tagging for observability.

### AI Gateway vs LiteLLM

Coder's AI Gateway (Premium, v2.30+) provides centralized LLM proxy management:
- Sets `ANTHROPIC_BASE_URL` to `${data.coder_workspace.me.access_url}/api/v2/aibridge/anthropic`
- Authenticates via the workspace owner's Coder session token
- Provides audit logging, token tracking, and cost management
- ✅ **We have a Premium license** — AI Gateway is the primary model routing mechanism

**Decision**: Use **AI Gateway as the primary** model routing mechanism. `enable_ai_gateway = true` is the default for all `claude-code` modules. LLM credentials stay in the Coder control plane — no `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` env vars leak into workspaces.

Non-Anthropic providers (Codex requires `OPENAI_API_KEY`, etc.) are routed through AI Gateway's multi-provider support. For any provider not yet supported by AI Gateway, LiteLLM remains available as a fallback proxy at `http://proxy:4000`.

Templates default to AI Gateway. LiteLLM is retained for:
- Local mode (`WorkspaceProvider::Local`) where there's no Coder server
- Non-Anthropic providers that AI Gateway doesn't yet proxy
- Development/testing without a Coder Premium license

## Current Implementation State

### Implemented and working

| Component | Location | Status |
|---|---|---|
| **CoderTransport** | `crates/pair-harness/src/transport.rs` | Complete — `WorkspaceTransport` trait with `LocalTransport` + `CoderTransport` (feature-gated behind `coder`). |
| **CoderClient** | `crates/coder-client/src/lib.rs` | Complete (784 lines) — REST + SSH client: workspace lifecycle, command exec, file I/O. No Chats API. |
| **CoderBootstrapper** | `crates/coder-client/src/bootstrap.rs` | Complete — idempotent bootstrap, pushes forge/sentinel templates. |
| **Coder Types** | `crates/coder-client/src/types.rs` | Complete — workspace, template, user, API key types. No Chat/Task types. |
| **CoderProcess** | `crates/pair-harness/src/coder_process.rs` | Complete (feature-gated) — spawn forge/sentinel in Coder workspaces. |
| **Provisioner** | `crates/pair-harness/src/provision.rs` | Complete (~1864 lines) — settings, MCP, plugins, hooks, permissions. Transport-agnostic. |
| **WorkspaceProvider** | `crates/config/src/state.rs` | Complete — `enum WorkspaceProvider { Local, Coder }`. `TicketStatus::AwaitingHuman` exists. |
| **Registry** | `crates/config/src/registry.rs` + TUI `write_registry_file()` | `RegistryEntry.workspace_provider: Option<WorkspaceProvider>` exists. Static `orchestration/agent/registry.json` not in repo — generated at runtime by TUI setup wizard. Test fixture at `tests/e2e/registry.json`. |
| **Docker Compose** | `docker-compose.yml` | Services: LiteLLM, Redis, Coder PostgreSQL + Coder server, OpenFlows app. |
| **LiteLLM Config** | `litellm_config.yaml` | Per-role routing with `routing_key` dispatch. |
| **FallbackClient** | `crates/agent-client/src/fallback.rs` | Proxy mode via `PROXY_URL`. Direct mode with per-provider API key fallback. |
| **Nexus Coder integration** | `crates/agent-nexus/src/lib.rs` | `coder_client_from_store()`, `provision_coder_workspace()`. Only forge template. |
| **Vessel Coder integration** | `crates/agent-vessel/src/node.rs` | Coder transport, workspace stopping, conflict resolution. |
| **NexusNode::reconcile()** | `crates/agent-nexus/src/lib.rs:1084` | Detects orphaned tickets, stale workers, unmerged PRs. |
| **Forge/Sentinel templates** | `crates/coder-client/templates/` | Terraform templates with Docker volume, `coder_agent`, `repo_url` variable. Archives exist. |

### Not yet implemented

1. **Templates for nexus, vessel, lore** — only forge/sentinel exist
2. **Ephemeral workspace lifecycle** — no destroy-on-merge orchestration
3. **Chats API client** — `CoderClient` has no Chats API methods (`/api/experimental/chats`)
4. **Cross-agent SharedStore coordination** — only forge-sentinel pair state exists
5. **Persistent volume with git pull** — templates clone fresh; no init script for incremental pull
6. **Workspace teardown on merge** — stop but no delete
7. **Self-healing** — reconcile handles ticket/worker states but not workspace infrastructure
8. **Nexus inside Coder** — no nexus template
9. **Heartbeat mechanism** — no background heartbeat writer
10. **Notification modules** — no `crates/notifier/` crate
11. **Standards file provisioning** — provisioner doesn't copy CODING.md/SECURITY.md/REVIEW.md
12. **Registry provisioning from SharedStore** — no SharedStore-based registry fallback
13. **Coder Registry module integration** — templates don't use `claude-code` or `slackme` modules
14. **AI Gateway awareness** — no support for Coder's AI Gateway as an alternative to LiteLLM

## Coder as Selected Provider: Implications

The `registry.json` already has `"workspace_provider": "coder"` for all 5 agents. Docker Compose has the Coder profile wired. This means:

1. **Coder mode is the default** — `WorkspaceProvider::Coder` is the active path; `Local` is fallback.
2. **Template bootstrapping must be idempotent** — `CoderBootstrapper::bootstrap()` runs on every start; all 5 templates must be pushed.
3. **Nexus must be Coder-aware for all roles** — `provision_coder_workspace()` currently hardcodes `"openflows-forge"`. Must route to the correct template per role.
4. **Vessel must delete (not just stop) workspaces** on merge.
5. **Templates must use Coder Registry modules** — `claude-code` for agent installation with `enable_ai_gateway = true`, `slackme` for notifications (plus our NotificationService).
6. **The `coder` feature flag on `pair-harness` must be enabled** in Coder-mode deployments.
7. **Chats API replaces Tasks API** — all programmatic agent orchestration uses `/api/experimental/chats`, not the deprecated Tasks API (`/api/v2/tasks`).

## Architecture

### Coder Agents (Chats API) for Orchestration

Instead of the deprecated Tasks API, Nexus orchestrates agents through the **Chats API**:

```
Nexus workspace (long-lived, Coder-managed)
   │
   ├── Detects new GitHub issue
   │
   ├── Create workspace + send chat prompt (CLI mode):
   │   POST /api/v2/users/{user}/workspaces  → create workspace from role template
   │   POST /api/experimental/chats          → create chat with workspace_id
   │
   ├── Follow-up messages via:
   │   POST /api/experimental/chats/{chat_id}/messages
   │
   ├── Stream events via:
   │   GET wss://.../{chat_id}/stream          → WebSocket for real-time status
   │
   ├── Archive completed chats:
   │   PATCH /api/experimental/chats/{chat_id}  {"archived": true}
   │
   ▼
Workspaces boot → git pull → agent module installs CLI → harness binary + agent execute
   │
   ├── Nexus polls chat status or receives streaming events
   │
   ▼
Agents coordinate via SharedStore (Redis):
   nexus creates chats in Coder      →  forge reads dispatch from SharedStore
   forge writes PR/status keys       →  sentinel reads and reviews
   sentinel writes review keys       →  nexus reads and routes
   vessel monitors CI & merges       →  nexus updates ticket status
```

### Orchestration Mode: CLI Mode Only

OpenFlows uses **CLI mode exclusively** — the OpenFlows harness binary runs inside each workspace and enforces typed SharedStore contracts. The Coder Registry agent module (e.g., `claude-code`) installs the agent CLI in the workspace. Nexus creates a Coder Chat bound to that workspace, and the agent processes the prompt inside the workspace with access to the repo, Redis, and the full tool chain.

**Why CLI mode only**:
- The OpenFlows harness writes structured keys to Redis with enforced schemas (forge→sentinel handoffs, vessel CI monitoring, nexus state coordination).
- Autonomous mode agents (control-plane prompt-driven) have no knowledge of SharedStore contracts — they would need to shell out to `redis-cli` with hand-crafted commands, which trades a typed, enforced contract for "hope the LLM remembers the right key format."
- CLI mode gives deterministic behavior: the harness binary, not the LLM, controls what gets written to SharedStore and when.

**No autonomous mode**: The "Option B: let Coder Agents auto-select template" path is not used. Nexus always creates a workspace from a specific template (`openflows-{role}`) bound to a specific role, then sends a Chat prompt to that workspace. Template auto-selection by Coder Agents is not part of the OpenFlows architecture.

**Chat sub-agents (parent/child)**: The Chats API supports `parent_chat_id` and `children` for delegation. OpenFlows does **not** use this feature. All cross-agent orchestration goes through SharedStore. This avoids coupling OpenFlows coordination to Coder's chat hierarchy.

**Standards file provisioning**: OpenFlows provisions `CODING.md`, `SECURITY.md`, and `REVIEW.md` via the Provisioner (Task 1.9) writing them into the workspace via `CoderTransport`. The Chats API's `context.resources` field for instruction files is not used — the agent prompt references these files by path.

### Workspace Template Structure with Configurable Agent Modules

Each template (`openflows-{role}`) includes the agent module specified in `registry.json` via the `coder_module` field. The module is selected at template creation time by the Provisioner based on the role's `cli` and `coder_module` configuration.

**Example: forge template with `claude` (Claude Code)**:

```hcl
module "agent" {
  source            = "registry.coder.com/coder/claude-code/coder"
  version           = "5.2.0"
  agent_id          = coder_agent.main.id
  workdir           = "/home/coder/workspace"
  enable_ai_gateway = var.use_ai_gateway  # defaults to true
  
  mcp = var.mcp_config
  managed_settings = var.managed_settings
  post_install_script = local.init_script
  
  # No anthropic_api_key needed — AI Gateway authenticates via Coder session token
}
```

**Example: vessel template with `codex` (OpenAI Codex)**:

```hcl
module "agent" {
  source            = "registry.coder.com/coder-labs/codex/coder"
  version           = "VERIFY_ON_REGISTRY"  // verify against live registry before applying
  agent_id          = coder_agent.main.id
  workdir           = "/home/coder/workspace"
  # openai_api_key not needed if routing through AI Gateway
  # For AI Gateway fallback to LiteLLM for non-Anthropic providers:
  # openai_api_key = var.openai_api_key  # only needed if AI Gateway doesn't proxy this provider
  
  post_install_script = local.init_script
}
```

> **Note on non-Anthropic providers**: The `codex`, `aider`, and `goose` modules may require `openai_api_key` or equivalent even with AI Gateway enabled, depending on whether AI Gateway supports their provider. Check Coder's AI Gateway documentation for the current provider support matrix. If AI Gateway proxies the provider, the API key stays in the Coder control plane. If not, the key passes through LiteLLM as a fallback.

**Shared template infrastructure** (common to all role templates):

```hcl
locals {
  init_script = <<-EOT
    #!/bin/bash
    set -e
    # git pull or clone
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull origin main || true
    else
      git clone ${var.repo_url} /home/coder/workspace
    fi
    # SharedStore heartbeat
    nohup bash -c 'while true; do
      redis-cli -u ${var.redis_url} SET "heartbeat:${var.role}-T-${var.ticket_id}" \
        "{\"ts\":$(date +%s),\"ws_id\":\"${data.coder_workspace.me.id}\",\"status\":\"running\"}"
      sleep 30
    done' &
  EOT
}

# Git configuration module
module "git-config" {
  source   = "registry.coder.com/coder/git-config/coder"
  version  = "1.0.0"
  agent_id = coder_agent.main.id
}

# Slack notification module (conditional)
module "slackme" {
  count            = var.enable_slackme ? 1 : 0
  source           = "registry.coder.com/coder/slackme/coder"
  version          = "1.0.33"
  agent_id         = coder_agent.main.id
  auth_provider_id = "slack"
}
```

**Module selection logic**: The Provisioner reads `registry.json` and generates the appropriate Terraform module block based on the `coder_module.source` field. The default mapping:

| `cli` in registry | Default module source | Default version |
|---|---|---|
| `claude` | `registry.coder.com/coder/claude-code/coder` | `5.2.0` |
| `codex` | `registry.coder.com/coder-labs/codex/coder` | `1.0.0` |
| `aider` | `registry.coder.com/coder/aider/coder` | `1.0.0` |
| `goose` | `registry.coder.com/coder/goose/coder` | `1.0.0` |

Adding a new agent CLI means adding one row to the mapping and optionally creating a template variant. No code changes needed.

**Role-specific permissions via `managed_settings`**:

| Role | `permission_mode` | Rationale |
|---|---|---|
| `forge` | `acceptEdits` | Builds code, needs write access |
| `sentinel` | `plan` | Reviews code, should not auto-edit |
| `nexus` | `plan` | Orchestrates, should not auto-edit |
| `vessel` | `acceptEdits` | Merges PRs, needs write access |
| `lore` | `acceptEdits` | Writes documentation, needs write access |

These map to the `managed_settings.permissions.defaultMode` field in the `claude-code` module, or equivalent settings in other agent modules.

### Persistent Volume + Git Pull

Unchanged from previous plan — init scripts in `post_install_script` handle git pull on persistent volumes.

### Model Routing

**Primary: Coder AI Gateway** (Premium license available)

All Anthropic model calls route through AI Gateway by default:
- `ANTHROPIC_BASE_URL` is set to `${data.coder_workspace.me.access_url}/api/v2/aibridge/anthropic` by the `claude-code` module when `enable_ai_gateway = true`
- Authentication uses the workspace owner's Coder session token — no API keys in workspace env
- Audit logging, token tracking, and cost management are built-in

**Fallback: LiteLLM proxy** (for non-Anthropic providers and Local mode)

Non-Anthropic providers (Codex/`OPENAI_API_KEY`, etc.) are routed through AI Gateway if supported, otherwise through LiteLLM at `http://proxy:4000`. In Local mode (`WorkspaceProvider::Local`), all LLM calls go through LiteLLM since there's no Coder server.

Templates default to AI Gateway enabled. The `USE_AI_GATEWAY` template parameter defaults to `true`.

### Chats API Endpoints (Replacing Tasks API)

| Operation | Method | Endpoint |
|---|---|---|
| Create chat | `POST` | `/api/experimental/chats` |
| List chats | `GET` | `/api/experimental/chats` |
| Get chat | `GET` | `/api/experimental/chats/{chat}` |
| Send message | `POST` | `/api/experimental/chats/{chat}/messages` |
| Edit message | `PATCH` | `/api/experimental/chats/{chat}/messages/{message}` |
| Stream events | `GET` | WebSocket `/api/experimental/chats/{chat}/stream` |
| Interrupt | `POST` | `/api/experimental/chats/{chat}/interrupt` |
| Archive | `PATCH` | `/api/experimental/chats/{chat}` `{"archived": true}` |
| List models | `GET` | `/api/experimental/chats/models` |
| Upload file | `POST` | `/api/experimental/chats/files` |
| Watch all chats | WebSocket | `/api/experimental/chats/watch` |

Chat status values: `pending`, `running`, `waiting`, `error`, `requires_action`

### Human Notification: `slackme` Module + Custom NotificationService

The `coder/slackme` module handles **command-completion notifications** (DMs when a command finishes). This covers the "forge finished coding" use case.

For **`awaiting_human` escalation** (event-oriented notifications when agents are stuck), we still need a custom `NotificationService` that supports:
- Slack webhook (for channel-based alerts, not just DMs)
- Discord webhook
- WhatsApp (Twilio)
- These are triggered by Nexus/Vessel when `awaiting_human` status is written to SharedStore

### SharedStore Cross-Agent Coordination

Unchanged — extend key schema with ticket-scoped keys.

### Self-Healing Architecture

Unchanged — three layers (Nexus reconciliation, workspace crash recovery, SharedStore state recovery). Heartbeat monitoring integrates with the `post_install_script` of the `claude-code` module.

## Task List

### Phase 1: Template Expansion + Agent Module System + Persistent Volumes

**1.1** Extend `registry.json` with `coder_module` field per agent
- Add `coder_module` object: `{ "source": "registry.coder.com/coder/claude-code/coder", "version": "5.2.0", "params": { "workdir": "/home/coder/workspace", "permission_mode": "acceptEdits" } }`
- Default mapping based on `cli` field: `claude` → claude-code module, `codex` → codex module, etc.
- When `coder_module` is absent, use the default mapping

**1.2** Add default module mapping to Provisioner config
- In `crates/pair-harness/src/provision.rs` or a new config file, define `DEFAULT_AGENT_MODULES`:
  ```rust
  static DEFAULT_AGENT_MODULES: &[(&str, &str, &str)] = &[
      ("claude", "registry.coder.com/coder/claude-code/coder", "5.2.0"),
      // NOTE: codex/aider/goose versions must be verified against live registry before use
      ("codex", "registry.coder.com/coder-labs/codex/coder", "VERIFY_ON_REGISTRY"),
      ("aider", "registry.coder.com/coder/aider/coder", "VERIFY_ON_REGISTRY"),
      ("goose", "registry.coder.com/coder/goose/coder", "VERIFY_ON_REGISTRY"),
  ];
  ```

**1.2b** Verify all module versions against the live Coder Registry
- Query `registry.coder.com` for each module's latest stable version
- Confirm each module works with Chats API (not just Tasks/AgentAPI)
- Update `DEFAULT_AGENT_MODULES` with verified versions
- Document any modules that still couple to AgentAPI/Tasks

**1.3** Create Terraform template `crates/coder-client/templates/openflows-nexus/main.tf`
- Use the agent module specified by `registry.json` (via variable `agent_module_source`, `agent_module_version`)
- Include shared infrastructure: persistent volume, git-config module, heartbeat init script
- Nexus is long-lived; add `redis_url`, `litellm_proxy_url`, `coder_url` variables

**1.4** Create Terraform template `crates/coder-client/templates/openflows-vessel/main.tf`
- Same structure; vessel needs `github_token` for merge operations
- Use `acceptEdits` permission mode for vessel

**1.5** Create Terraform template `crates/coder-client/templates/openflows-lore/main.tf`
  - Same structure; lore needs documentation tooling in init script: `pandoc`, `mdbook`, and a markdown linter (`markdownlint-cli`)
  - Use `acceptEdits` permission mode for lore

**1.6** Update `openflows-forge/main.tf` and `openflows-sentinel/main.tf`
- Replace hardcoded agent config with configurable module block
- Add `agent_module_source`, `agent_module_version`, `agent_module_params` variables
- Add shared modules: `git-config`, `slackme` (conditional), `coder-login`
- Add `managed_settings` with role-specific permissions
- Add `mcp` variable for MCP server config (GitHub, Redis)
- Add `redis_url`, `litellm_proxy_url`, `ticket_id`, `role` variables
- Add `enable_ai_gateway = true` as default for `claude-code` module
- Add heartbeat writer in `post_install_script`

**1.7** Package all templates as `.tar.gz` archives
- Verify/update existing forge/sentinel archives
- Create nexus, vessel, lore archives

**1.8** Update `CoderBootstrapper::bootstrap()` in `crates/coder-client/src/bootstrap.rs`
- Push all 5 templates
- Lines 125-147: expand the `include_bytes!` + `push_template` block

**1.9** Update `Provisioner` in `crates/pair-harness/src/provision.rs`
- Add `coder_module` field resolution: read from `registry.json`, fall back to `DEFAULT_AGENT_MODULES`
- Add standards file copy: `CODING.md`, `SECURITY.md`, `REVIEW.md`
- When `WorkspaceProvider::Coder`, write `registry.json` into workspace via `CoderTransport`
- Generate Terraform module block based on the resolved `coder_module`

**1.10** Update init scripts to download orchestration config from SharedStore when `ORCHESTRATOR_DIR` is not set

### Phase 2: AI Gateway Primary + LiteLLM Fallback

**2.1** Enable AI Gateway by default in all `claude-code` module configurations
- `enable_ai_gateway = true` is the default in all templates
- Remove `anthropic_api_key` from template variables — AI Gateway authenticates via Coder session token
- Keep `anthropic_api_key` as an可选 override for Local mode or when AI Gateway is unavailable

**2.2** Update `litellm_config.yaml` to use per-role model aliases (fallback role)
- Change `model_name` to role-specific: `"openflows-nexus"`, `"openflows-forge"`, etc.
- Keep `routing_key` dispatch
- LiteLLM is now fallback-only: used in Local mode and for providers AI Gateway doesn't proxy

**2.3** Update `registry.json` `model_backend` fields
- When `workspace_provider` is `"coder"` and AI Gateway is enabled: model routing goes through AI Gateway, `model_backend` references AI Gateway model config IDs
- When `workspace_provider` is `"coder"` and AI Gateway is disabled: fall back to LiteLLM aliases
- When `workspace_provider` is `"local"`: keep direct provider names

**2.4** Verify `FallbackClient` proxy mode works end-to-end (still needed for Local-mode)
- When `PROXY_URL=http://proxy:4000`, all LLM calls route through LiteLLM
- This path is only used in Local mode or when AI Gateway is unavailable

**2.5** Add `USE_AI_GATEWAY` (default `true`) and `LITELLM_PROXY_URL` parameters to all workspace templates
- `USE_AI_GATEWAY` defaults to `"true"` — AI Gateway is the primary path
- `LITELLM_PROXY_URL` defaults to `"http://proxy:4000"` — fallback for non-Anthropic providers
- When `USE_AI_GATEWAY` is `true`, the `claude-code` module gets `enable_ai_gateway = true`; no `anthropic_api_key` is passed
- When `USE_AI_GATEWAY` is `false` (fallback), templates fall back to LiteLLM proxy mode with direct API keys

**2.6** Add `LITELLM_PROXY_URL` fallback in `FallbackClient`
- When `PROXY_URL` not set but `LITELLM_PROXY_URL` is, use `LITELLM_PROXY_URL`

**2.7** Template-level AI Gateway integration for non-Anthropic providers
- When `USE_AI_GATEWAY = true` and the provider is Anthropic: AI Gateway handles routing, no API key in workspace
- When `USE_AI_GATEWAY = true` and the provider is OpenAI/etc.: check if AI Gateway supports that provider
  - If yes: route through AI Gateway (e.g., OpenAI models via AI Gateway's multi-provider support)
  - If no: fall back to LiteLLM proxy for that provider, passing the API key through template variables
- The `aibridge-proxy` module can be used alongside `claude-code` to route all providers through AI Gateway

**2.8** Configure Coder AI Gateway model access
- In Coder server config, enable AI Gateway and configure model access per provider
- Ensure Anthropic models are available through AI Gateway for all 5 agent workspace owners
- Verify that `GET /api/experimental/chats/models` returns available models with correct `model_config_id` values

### Phase 3: Chats API Client (Replacing Tasks API)

**3.1** Add Chat types to `crates/coder-client/src/types.rs`
- `CreateChatRequest { organization_id, workspace_id, model_config_id, content: Vec<ChatInputPart>, labels }`
- `ChatInputPart { r#type: "text"|"file"|"file-reference", text, file_id }`
- `Chat { id, owner_id, workspace_id, status: ChatStatus, title, created_at, updated_at }`
- `ChatStatus { Pending, Running, Waiting, Error, RequiresAction }`
- `ChatMessage { id, chat_id, role, content, created_at }`

**3.2** Add Chat methods to `CoderClient`
- `create_chat(req: &CreateChatRequest) -> Result<Chat>` — `POST /api/experimental/chats`
- `get_chat(id: &str) -> Result<Chat>` — `GET /api/experimental/chats/{chat}`
- `list_chats() -> Result<Vec<Chat>>` — `GET /api/experimental/chats`
- `send_chat_message(chat_id: &str, content: Vec<ChatInputPart>) -> Result<ChatMessage>` — `POST /api/experimental/chats/{chat_id}/messages`
- `archive_chat(id: &str) -> Result<()>` — `PATCH /api/experimental/chats/{id}` with `{"archived": true}`
- `interrupt_chat(id: &str) -> Result<()>` — `POST /api/experimental/chats/{id}/interrupt`
- `list_models() -> Result<Vec<ModelInfo>>` — `GET /api/experimental/chats/models`

**3.3** Add convenience method `create_ticket_chat(ticket_id, role, prompt)`
- Creates a workspace first (or reuses existing) via `POST /api/v2/users/{user}/workspaces`
- Then creates a Chat bound to that workspace via `POST /api/experimental/chats` with `workspace_id`
- Returns `(Chat, CoderWorkspace)` tuple

**3.4** Add `archive_ticket_chats(ticket_id)` to clean up chats when ticket merges/closes
  - Finds chats matching the ticket label
  - Archives all matching chats

**3.5** Define Chat label schema for ticket correlation
  - Every chat created by Nexus must include labels: `{"ticket": "{ticket_number}", "role": "{role}", "flow": "openflows"}`
  - This enables `archive_ticket_chats` to find chats by label, and distinguishes OpenFlows chats from manual Coder Agent usage
  - `list_chats()` returns all chats — labels are the only way to filter. Without them, `archive_ticket_chats` cannot find the right chats

**3.6** Add model config caching to Nexus startup
  - On startup, Nexus calls `GET /api/experimental/chats/models` and caches available model config IDs per provider
  - Maps from the `model_backend` field in `registry.json` to the correct `model_config_id` for chat creation
  - `CreateChatRequest.model_config_id` is required — it references a model config from this endpoint, not a raw model name
  - Cache is refreshed periodically (every 10 min) or on `Error` from chat creation

**3.7** Define Nexus message-sending protocol for Chats API
  - Nexus sends follow-up messages only when `chat.status == waiting` and `ticket:{id}:chat_action:{role} == "completed"` or `nil` (new chat)
  - Nexus reads `queue_update` stream events to detect message backlog before sending
  - If Nexus needs to send a message while the agent is `running`, it queues the message locally and sends when status transitions to `waiting`

**3.8** Add `tokio-tungstenite` dependency and `ChatStream` type for WebSocket streaming
  - Add `tokio-tungstenite` to `crates/coder-client/Cargo.toml`
  - Create `crates/coder-client/src/chat_stream.rs` with `ChatStream` type that wraps the WebSocket connection to `/api/experimental/chats/{chat}/stream`
  - Parse stream events: `message_part`, `message`, `status`, `error`, `queue_update`, `action_required`, `retry`, `preview_reset`, `history_reset`

**3.9** Add test harness for Chats API integration
  - Create `crates/coder-client/src/mock_chat_server.rs` — a mock HTTP server that responds to Chats API endpoints for unit tests
  - Feature-gate all Chats API code behind a `chats-api` feature flag in `Cargo.toml`
  - Define fallback behavior: if Chats API calls return 404 or are disabled, fall back to direct workspace exec mode (`CoderTransport` + SSH)

### Phase 4: Ephemeral Workspace + Chat Lifecycle

**4.1** Add `destroy_coder_workspace()` to Nexus
- Add `destroy_coder_workspace(store, worker_id, ticket_id)` that deletes workspace + archives associated chats

**4.2** Add lifecycle methods to `CoderClient`
- `create_workspace_for_chat(template_name, workspace_name, parameters)` — creates workspace for Coder Agents
- `delete_workspace_and_archive_chats(workspace_name_pattern)` — deletes workspace and archives chats

**4.3** Update `NexusNode::post()` to archive chats and destroy workspaces when a PR is merged
- When `TicketStatus::Merged` transitions: archive chat → destroy workspace

**4.4** Update `NexusNode::prep()` to create a Chat for each agent assignment
- Creates a Coder Chat bound to the agent's workspace with the ticket prompt
- Stores chat ID in SharedStore under `ticket:{id}:chat:{role}`

**4.5** Update `NexusNode` to check chat status via `get_chat()` and sync to SharedStore
  - On each `prep()` cycle, check chat status for active tickets
  - Map `ChatStatus` to ticket phase:
    - `Running` → in_progress
    - `Waiting` → **ambiguous** — Nexus must disambiguate using a SharedStore-side state flag (see below)
    - `RequiresAction` → `AwaitingHuman` (agent needs human approval, e.g. a tool-call permission prompt)
    - `Error` → failed
  - **`Waiting` disambiguation**: Coder's `waiting` status fires both when a chat is freshly created *and* whenever a run finishes or is interrupted. Nexus cannot tell "forge finished and is ready for sentinel" from "forge just got interrupted because its workspace crashed" using `waiting` alone. Nexus must track its own `last_action` per chat in SharedStore (`ticket:{id}:chat_action:{role}`) so it can distinguish:
    - `waiting` + `last_action == "completed"` → agent finished successfully, ready for next phase
    - `waiting` + `last_action == "interrupted"` → workspace crash, trigger recovery
    - `waiting` + `last_action == nil` → newly created, send initial prompt

**4.6** Workspace naming convention: `{role}-T-{ticket_number}`
- Update `provision_coder_workspace()` to use this convention
- Currently uses `{worker_id}-{ticket_id}` at line 634

**4.7** Update `VesselNode::stop_coder_workspace_for_worker()` to archive chats and delete workspace (not just stop)
- Current: stops workspace, clears `workspace_id`
- New: archives associated chats, stops workspace, waits, deletes workspace, clears `workspace_id`

### Phase 5: SharedStore Cross-Agent Coordination

**5.1** Extend `SharedStore` key schema in `crates/config/src/state.rs`
- Add constants: `KEY_TICKET_WORKSPACE`, `KEY_TICKET_DISPATCH`, `KEY_TICKET_CHAT`, `KEY_TICKET_REVIEW`, `KEY_TICKET_DEPLOYMENT`, `KEY_TICKET_STATUS`
- Pattern: `ticket:{id}:{subkey}`

**5.2** Add heartbeat key writing via `post_install_script` in templates
- Background process writes `heartbeat:{role}-T-{ticket}` to SharedStore every 30s
- JSON value: `{timestamp, workspace_id, status}`

**5.3** When chat status changes, Nexus updates SharedStore `ticket:{id}:status`

**5.4** Update Forge to read task dispatch from SharedStore and write PR/status back

**5.5** Update Sentinel to read PR status from SharedStore and write review results

**5.6** Update Vessel to read pending PRs from SharedStore and write merge/deployment status

**5.7** Use Coder Chat `diff_status` as primary PR state source for Vessel
  - Each forge chat exposes `diff_status` in the Chats API response: `pr_number`, `head_branch`, `changed_files`, `pull_request_state`, `pull_request_title`, `approved`, `changes_requested`
  - Vessel reads `diff_status` from the forge chat as the primary PR status source, falling back to GitHub API for chats without `diff_status`
  - This reduces GitHub API calls and provides real-time PR review status without polling

**5.8** SharedStore is the source of truth for ticket/workflow state
  - Chat status and `diff_status` are **input signals** that update SharedStore, but never override manual state transitions
  - If chat shows `status: error` but SharedStore shows `in_progress`, Nexus logs the discrepancy and uses SharedStore as authoritative
  - This prevents Coder API inconsistencies from corrupting workflow state

### Phase 6: Self-Healing

**6.1** Add workspace liveness checks to `NexusNode::reconcile()`
- Iterate `WorkerSlot`s with `workspace_id`; call `CoderClient.get_workspace()`

**6.2** Add chat status checks in reconciliation
- If chat shows `error` status, add to recovery list

**6.3** Add heartbeat staleness detection
- If `heartbeat:{role}-T-{ticket}` is >90s old, trigger recovery

**6.4** Implement workspace recovery
- Stopped → `start_workspace()` + re-inject context
- Deleted → `create_workspace()` from template + `git pull`
- Stuck → `workspace_exec()` to check/restart process

**6.5** Add max recovery attempts (3) per ticket
- Track in SharedStore: `ticket:{id}:recovery_attempts`

**6.6** Add chat interruption on workspace crash
  - If `CoderProcess.is_running()` returns false, call `interrupt_chat()` and update SharedStore
  - **Critical**: Calling `interrupt_chat()` lands the chat in `waiting` state — Nexus must set `ticket:{id}:chat_action:{role} = "interrupted"` in SharedStore *before* calling `interrupt_chat()`, so that the `waiting` disambiguation logic in 4.5 correctly identifies this as a crash rather than a normal completion

**6.7** Extend `FlowRecovery` with `crashed_workspaces: Vec<CrashedWorkspace>`

### Phase 7: Nexus in Coder + Chat-Driven Orchestration

**7.1** Create `openflows-nexus` template (Phase 1.1)
- Must be long-lived; include GitHub CLI, Redis client, Coder CLI
- Must have `CODER_URL` and `CODER_API_TOKEN` as template parameters

**7.2** Update Nexus to bootstrap inside a Coder workspace
- Load `registry.json` from SharedStore or persistent volume
- Read `CODER_URL` and `CODER_API_TOKEN` from environment

**7.3** Update Nexus to use Chats API for orchestrating agents
- Instead of spawning CLI processes, create Coder Chats
- Nexus creates a chat for each role assignment via `POST /api/experimental/chats`
- Sends follow-up messages to running chats to inject context

**7.4** Update `openflows-setup` binary / `CoderBootstrapper`
  - Explicit bootstrap sequence:
    1. `CoderBootstrapper::bootstrap()` — wait for Coder healthy → create admin user → login → push all 5 templates
    2. Create Nexus workspace from `openflows-nexus` template
    3. Nexus workspace starts → `openflows` binary reads config and begins the orchestration loop
    4. Nexus creates workspaces for other roles as tickets arrive
  - **First-mover problem**: `CoderBootstrapper` (the setup binary) must run **outside** Coder to create the initial admin user, push templates, and create the Nexus workspace. This binary cannot itself be a Coder workspace. It runs on the Docker host or in the `openflows` container alongside `docker-compose.yml`.

**7.5** Ensure Nexus can call Coder API from within a Coder workspace
  - `CODER_URL` and `CODER_API_TOKEN` must be template parameters
  - **Token scoping**: Nexus uses a scoped Coder service account (not the admin token) with RBAC restricted to workspace CRUD and chat operations. Other workspaces must NOT have access to this token
  - The admin token is only used by `CoderBootstrapper` for initial setup; Nexus gets its own service account

**7.6** Nexus workspace self-healing: who heals Nexus?
  - Nexus is long-lived and **not** inside the ticket lifecycle — `reconcile()` doesn't apply to it
  - If Nexus's workspace crashes: Coder's workspace auto-start policy restarts it (configured in the `openflows-nexus` template via `coder_agent.main.start = true`)
  - If Nexus's workspace is deleted: the `openflows-setup` binary or a systemd unit detects Nexus is unreachable and re-creates it via `CoderBootstrapper::bootstrap()`
  - Nexus stores its state in `SharedStore` (Redis), so a fresh Nexus workspace can resume from where it left off

### Phase 8: Human Notification (Module-based + Custom Service)

**8.1** Use `coder/slackme` module in templates for command-completion notifications
- Conditional module inclusion based on `enable_slackme` template parameter
- Requires `CODER_EXTERNAL_AUTH_1_TYPE=slack` + client ID/secret in Coder server config
- This handles per-command DMs ("forge-T-42 completed `npm run build` in 4.2s")

**8.2** Use other notification/utility modules as they become available
- `coder/git-config` for consistent git identity across workspaces
- `coder/git-commit-signing` for verified commits (optional per role)
- `coder/github-upload-public-key` for SSH key management
- Module inclusion is controlled by template parameters, not hardcoded

**8.3** Create `crates/notifier/` crate with `NotificationService`
- For `awaiting_human` escalation (event-oriented, not command-completion)
- Supports: Slack webhook (channel alerts), Discord webhook, WhatsApp (Twilio)
- `NotificationMessage` struct: `{ticket_id, role, reason, workspace_link, github_link}`

**8.4** Implement Slack notification (channel-based, complements `slackme` DMs)
- POST to webhook with Slack Block Kit message
- Include ticket details, Coder Chat link, GitHub issue link

**8.5** Implement Discord notification
- POST to webhook with rich embed

**8.6** Implement WhatsApp notification via Twilio/Business API

**8.7** Add environment variable configuration
- `SLACK_WEBHOOK_URL`, `DISCORD_WEBHOOK_URL`, `WHATSAPP_API_KEY`, `WHATSAPP_PHONE_NUMBER`

**8.8** Add Terraform template variables for notification webhooks

**8.9** Add `awaiting_human` notification triggers in Nexus and Vessel
- Fire-and-forget: non-blocking, log errors but don't fail main loop

**8.10** Add notification batching: max 1 per channel per 5 minutes per ticket

**8.11** Create `nexus-human-escalation` skill file in `orchestration/plugin/skills/`

### Phase 9: Configuration & Migration

**9.1** Add automatic `WorkspaceProvider::Coder` when `CODER_URL` is set

**9.2** Ensure `REDIS_URL`, `LITELLM_PROXY_URL` (fallback), `USE_AI_GATEWAY=true` are propagated to all workspaces

**9.3** Verify LiteLLM config aliases match registry `model_backend` values

**9.4** Keep `WorkspaceProvider::Local` fully functional (no Chats, no modules, process spawning)

**9.5** Ignore `instances` field when `workspace_provider` is `Coder`

**9.6** Enable `coder` feature flag by default in Coder-mode deployments

**9.7** Add Coder server configuration for Slack external auth (for `slackme` module)
  - `CODER_EXTERNAL_AUTH_1_TYPE=slack`, `CODER_EXTERNAL_AUTH_1_CLIENT_ID`, `CODER_EXTERNAL_AUTH_1_CLIENT_SECRET`
  - Update `docker-compose.yml` if needed

**9.8** Pin Coder server version in `docker-compose.yml`
  - Keep `ghcr.io/coder/coder` configurable via `CODER_IMAGE_TAG`
  - Default to `latest` until a tested pinned version is available in GHCR
  - Document any pinned version and the validation steps in `docs/coder-compatibility.md`

**9.9** Configure Coder AI Gateway model access per provider
  - In Coder server config, enable AI Governance Add-On with model access per provider
  - Ensure Anthropic models (claude-sonnet-4-5, etc.) are available through AI Gateway for all workspace owners
  - Verify `GET /api/experimental/chats/models` returns available models with `model_config_id` values
  - For non-Anthropic providers (OpenAI, Google): configure AI Gateway multi-provider support or document LiteLLM fallback path

**9.10** Add Coder workspace network policy exception for Redis
  - In all `openflows-*` templates, add a `coder_env` or security group rule allowing egress to `REDIS_URL`
  - Document this as an intentional exception to Coder's recommended network lockdown (control-plane + git provider only)

**9.10** End-to-end test: create issue → Coder Chats + workspaces provision → agents coordinate → PR created → merge → chats archived → workspaces destroyed. Test with `cli: "claude"` (claude-code module) and `cli: "codex"` (codex module). Verify Local mode works (no Chats, no modules, process spawning)

### Phase 10: TUI Configuration for Coder Modules and Notifications

The TUI (`crates/agentflow-tui/src/setup/`) currently has steps for provider selection, agent config, and Coder mode. It needs new/extended steps to configure Coder modules, AI Gateway, and notifications.

**10.1** Extend `SetupConfig` in `crates/agentflow-tui/src/setup/mod.rs`
- Add `agent_modules: Vec<AgentModuleConfig>` field
- Add `enable_ai_gateway: bool` field
- Add `slack_webhook_url: Option<String>` field
- Add `discord_webhook_url: Option<String>` field
- Add `notification_channels: Vec<NotificationChannel>` field

```rust
#[derive(Debug, Clone)]
pub struct AgentModuleConfig {
    pub agent_id: String,
    pub cli: String,
    pub module_source: String,   // e.g., "registry.coder.com/coder/claude-code/coder"
    pub module_version: String,  // e.g., "5.2.0"
    pub permission_mode: String,  // "acceptEdits" or "plan"
    pub enable_ai_gateway: bool,
}

#[derive(Debug, Clone)]
pub enum NotificationChannel {
    SlackWebhook { url: String },
    DiscordWebhook { url: String },
}
```

**10.2** Create `crates/agentflow-tui/src/setup/step_module.rs` — Agent Module Selection Step
- Shown after Agent Config step, only when `workspace_provider == Coder`
- Lists each agent with its current CLI and resolved Coder Registry module
- Allows switching CLI (claude → codex → aider) which auto-resolves the module source
- Shows module version (editable)
- Shows permission mode per role (forge=acceptEdits, sentinel=plan, nexus=plan, vessel=acceptEdits, lore=acceptEdits)
- Shows AI Gateway toggle (affects all agents that support it)
- Default mapping: `cli` field → module source from `DEFAULT_AGENT_MODULES`

**10.3** Create `crates/agentflow-tui/src/setup/step_notifications.rs` — Notification Configuration Step
- Shown in the wizard flow, after Coder step when in Coder mode
- Options: Slack webhook URL, Discord webhook URL
- Slack webhook is a URL input (not OAuth — that's handled by the `slackme` Coder module)
- Discord webhook is a URL input
- Both optional — if empty, that channel is disabled
- Explains that `slackme` module handles command-completion DMs (via Coder external auth), while these webhooks handle `awaiting_human` escalation alerts

**10.4** Extend `step_coder.rs` — Add AI Gateway and Slackme toggles
- When Coder mode is selected, show additional options:
   - "Enable Coder AI Gateway" (selected by default — Premium license available)
   - "Enable Slack command notifications (requires Coder external auth for Slack)"
- Store in `SetupConfig.enable_ai_gateway` (defaults to `true`) and a boolean for slackme

**10.5** Extend `step_agents.rs` — Pass module config through
- After the user configures agent instances and model backends, the module resolution happens
- Each agent's `cli` field determines the default module
- The user can override via step_module (only in Coder mode)

**10.6** Update `write_registry_file()` in `crates/agentflow-tui/src/setup/mod.rs`
- Write `coder_module` field per agent in `registry.json`:
  ```json
  {
    "id": "forge",
    "cli": "claude",
    "coder_module": {
      "source": "registry.coder.com/coder/claude-code/coder",
      "version": "5.2.0",
      "params": { "workdir": "/home/coder/workspace", "permission_mode": "acceptEdits" }
    }
  }
  ```
- Only write `coder_module` when `workspace_provider == Coder`

**10.7** Update `write_env_file()` in `crates/agentflow-tui/src/setup/mod.rs`
- When Coder mode: write `CODER_URL`, `CODER_ADMIN_PASSWORD`, `USE_AI_GATEWAY=true` (default)
- Write `LITELLM_PROXY_URL` as fallback (always present for Local mode compatibility)
- Write `SLACK_WEBHOOK_URL` and `DISCORD_WEBHOOK_URL` if provided
- Write `ENABLE_SLACKME=true/false` based on Coder module selection

**10.8** Update `write_env_file()` for docker-compose Coder profile
- When Coder mode is selected and slackme is enabled: write Coder server env vars for Slack external auth (`CODER_EXTERNAL_AUTH_1_TYPE=slack`, `CODER_EXTERNAL_AUTH_1_CLIENT_ID`, `CODER_EXTERNAL_AUTH_1_CLIENT_SECRET`)
- Template the `docker-compose.yml` or use `.env` overrides

**10.9** Default module resolution logic
- When `workspace_provider == Local`, no `coder_module` fields are written (modules are irrelevant for local mode)
- When `workspace_provider == Coder`, the `cli` field maps to `DEFAULT_AGENT_MODULES`
- User can change `cli` per agent in step_module, which auto-resolves the module
- The `step_module` step only appears when `workspace_provider == Coder`

## Risks

| Risk | Mitigation |
|---|---|
| Chats API is experimental | Wrap all Chats API calls in feature-flagged code; fall back to direct workspace exec mode if unavailable. The Chats API is the supported path going forward per Coder's migration guide. |
| AI Gateway is Premium — now our primary | We have a Premium license and are using AI Gateway as primary model routing. LiteLLM is retained as fallback for Local mode and providers not yet supported by AI Gateway. `FallbackClient` has proxy support for both paths. Templates default to `USE_AI_GATEWAY = true`. |
| TUI configuration complexity | The setup wizard gains 2 new steps (module selection, notifications). Keep steps incremental and skippable. Module selection only appears in Coder mode. Notifications are optional. |
| Coder Registry module compatibility | Pin module versions in `registry.json` `coder_module.version`; test new versions before upgrading. Module interface changes could break template composition. |
| Agent CLI module availability | If a registry module is unavailable, fall back to direct CLI installation via `post_install_script`. The module mapping is decoupled from core orchestration logic. |
| Multiple agent modules have different parameters | Each module has its own parameters (e.g., `claude-code` has `enable_ai_gateway`, Codex has `openai_api_key`). The Provisioner must handle per-module parameter generation. This is addressed by the `coder_module.params` field in `registry.json`. |
| `slackme` requires Coder external auth for Slack | `slackme` is optional (conditional `count`); NotificationService provides webhook-based Slack alerts without OAuth. |
| LiteLLM proxy as fallback | LiteLLM is fallback-only (Local mode and non-Anthropic providers). If LiteLLM is down, AI Gateway still handles Anthropic traffic. Deploy LiteLLM with health checks for when it's needed. |
| Workspace creation latency | Persistent volumes + git pull (~1-3s) vs fresh clone (~10-30s). `claude-code` module pre-accepts trust dialog. |
| SharedStore key conflicts | Ticket-scoped key prefixes; Redis keyspace isolation. |
| Coder API rate limits | Batch workspace operations; stagger creation across roles. |
| Template drift across 5 roles | Use shared Terraform modules for common config; `claude-code` and `slackme` modules reduce per-template code. |
| Tasks API deprecation (v2.37+) | Use Chats API exclusively. No implementation of Tasks API endpoints. |
| `agentflow-coder-bridge` design is obsolete | The prior `rmcp`-based bridge crate (AgentAPI/Tasks reporting) is superseded by the Chats API. Any existing `agentflow-coder-bridge` code must be deleted or repurposed. No new work on the Tasks/AgentAPI path. |
| `waiting` status ambiguity in self-healing | Coder's `waiting` state fires on both completion and interruption. Nexus must track its own `last_action` side-channel in SharedStore (not just rely on chat status) to distinguish "agent finished" from "workspace crashed." See Task 4.5. |
| CLI mode enforces SharedStore contracts | All agents run in CLI mode inside workspaces. The harness binary enforces typed Redis schemas. Chats API provides lifecycle management (create, monitor, archive); SharedStore provides coordination state. This two-surface architecture is explicit and deterministic. |
| Who heals Nexus? | Nexus is long-lived; `reconcile()` doesn't apply to it. Coder workspace auto-start restarts crashed Nexus. The `openflows-setup` binary (outside Coder) handles full re-creation. Nexus state lives in SharedStore (Redis), so a fresh workspace resumes where it left off. |
| Network lockdown vs Redis dependency | Coder templates recommend restricting workspace network access to control-plane + git provider. OpenFlows requires all workspaces to reach Redis directly. This must be documented as an intentional exception in the network/governance config: allow egress to `REDIS_URL` from all `openflows-*` workspaces. |
| Coder server image availability | The Chats API is experimental and "may change without notice until GA." `latest` is the practical default while GHCR tag publication can lag; if you pin a version, verify the tag exists before relying on it. |

## Validation Plan

1. **Phase 1**: Run `openflows-setup` → verify all 5 templates appear in Coder UI with the correct agent module (claude-code, codex, etc.) → verify persistent volumes and git pull work → verify `git-config` and `slackme` modules load correctly → test switching `cli` field in `registry.json` from `claude` to `codex` and verify the codex module loads
2. **Phase 2**: Send LLM requests through AI Gateway → verify Anthropic routing via `enable_ai_gateway`. Verify AI Gateway dashboard shows token tracking and cost. Test fallback to LiteLLM for non-Anthropic providers. Verify Local mode works without AI Gateway (no Coder server).
3. **Phase 3**: Use `CoderClient.create_chat()` → verify it appears in Coder Agents dashboard → send follow-up message → verify WebSocket streaming
4. **Phase 4**: Create GitHub issue → verify Coder Chats + workspaces provision → archive chat → verify workspace destroyed on merge
5. **Phase 5**: Verify SharedStore keys appear in Redis → verify agents read/write cross-agent keys
6. **Phase 6**: Kill forge workspace mid-task → verify Nexus detects stale heartbeat → verify workspace restarted → verify chat continues. Kill 3 times → verify `awaiting_human`
7. **Phase 7**: Verify Nexus runs inside Coder workspace and orchestrates via Chats API → verify orchestration files provisioned correctly
8. **Phase 8**: Trigger `awaiting_human` → verify Slack webhook + Discord + `slackme` DM → verify WhatsApp (if configured)
9. **Phase 9**: Full E2E test: create issue → Chats + workspaces provision → git pull → agents coordinate → PR → merge → chats archived → workspaces destroyed. Verify Local mode works (no Chats, no modules, process spawning)
10. **Phase 10**: Run `openflows-setup` TUI → verify module selection step appears in Coder mode → verify switching `cli` from `claude` to `codex` changes the resolved module → verify notification step saves webhook URLs → verify `registry.json` contains `coder_module` fields → verify `.env` contains `USE_AI_GATEWAY`, `SLACK_WEBHOOK_URL`, `DISCORD_WEBHOOK_URL`
