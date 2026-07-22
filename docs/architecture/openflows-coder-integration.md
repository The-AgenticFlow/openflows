# OpenFlows × Coder: Integrated Architecture

## Design Document v1.0

**Status:** Draft

---

## 1. Core Thesis

**Coder governs WHERE agents run. OpenFlows governs HOW agents coordinate. Together they form a complete platform.**

This is a platform layer (Coder) + application layer (OpenFlows) relationship. Coder provides secure, governed workspace infrastructure with identity, audit, and network isolation. OpenFlows provides architectural intelligence — flow graphs, typed contracts, multi-agent coordination, and self-healing reconciliation. Neither duplicates the other's core competency.

| Layer | Coder | OpenFlows |
|---|---|---|
| What it solves | Where agents run safely | How agents coordinate intelligently |
| Core primitive | Terraform workspace templates | PocketFlow flow graph + Node trait |
| Governance model | Infrastructure-first — govern execution environment | Architecture-first — plan before execute |
| Failure handling | Workspace isolation + identity tracing | NEXUS reconcile() + flow recovery |
| Agent model | Single agent per workspace (spawn_agent for sub-tasks) | Differentiated agents: NEXUS, FORGE, SENTINEL, VESSEL, LORE |
| State management | Chat persistence in database | SharedStore (Redis/in-memory) |
| Orchestration | Sequential agent loop with tool calls | Multi-agent coordinated via action-routing flow graph |

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    CODER CONTROL PLANE                                  │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │                    LLM PROVIDERS                                   │  │
│  │   Anthropic · OpenAI · Google · Azure · AWS Bedrock · Custom     │  │
│  └───────────────────────────┬───────────────────────────────────────┘  │
│                              │ API calls only                          │
│  ┌───────────────────────────▼───────────────────────────────────────┐  │
│  │              OPENFLOWS ORCHESTRATION ENGINE                        │  │
│  │                                                                   │  │
│  │   ┌─────────┐    ┌──────────────────┐    ┌─────────┐             │  │
│  │   │  NEXUS  │───▶│  PocketFlow       │───▶│ VESSEL  │             │  │
│  │   │ (mind)  │    │  (routing table)  │    │ (merge) │             │  │
│  │   └─────────┘    └──────────────────┘    └─────────┘             │  │
│  │                           │                                       │  │
│  │          ┌────────────────┼────────────────┐                     │  │
│  │          ▼                ▼                ▼                     │  │
│  │   ┌────────────┐  ┌────────────┐  ┌────────────┐                │  │
│  │   │ FORGE-     │  │ FORGE-     │  │ FORGE-     │                │  │
│  │   │ SENTINEL   │  │ SENTINEL   │  │ SENTINEL   │                │  │
│  │   │  Pair-1    │  │  Pair-2    │  │  Pair-N    │                │  │
│  │   └─────┬──────┘  └─────┬──────┘  └─────┬──────┘                │  │
│  │         │               │               │                        │  │
│  │   ┌─────▼──────────────▼───────────────▼──────┐                 │  │
│  │   │         SharedStore (Redis)                 │                 │  │
│  │   │   tickets · workers · PRs · events         │                 │  │
│  │   └────────────────────────────────────────────┘                 │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │              CODER INFRASTRUCTURE LAYER                           │  │
│  │                                                                   │  │
│  │   Template Registry · Identity (SSO) · Audit Log · MCP Config    │  │
│  │   Git Auth · Model Governance · Usage Analytics · Cost Controls  │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                         │
│  ┌───────────────────────────┐  ┌───────────────────────────────────┐  │
│  │  CODER TAILNET            │  │  Coder Workspace Daemon           │  │
│  │  (DERP relay / P2P)       │  │  (file I/O, shell, processes)     │  │
│  └─────────────┬─────────────┘  └─────────────┬─────────────────────┘  │
│                │                              │                         │
└────────────────┼──────────────────────────────┼─────────────────────────┘
                 │                              │
    ┌────────────▼──────────────────────────────▼────────────────────┐
    │              CODER WORKSPACES (Network Isolated)              │
    │                                                              │
    │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐        │
    │  │ Workspace-1  │  │ Workspace-2  │  │ Workspace-N  │        │
    │  │ (forge-1     │  │ (forge-2     │  │ (forge-N     │        │
    │  │  + sentinel) │  │  + sentinel) │  │  + sentinel) │        │
    │  │              │  │              │  │              │        │
    │  │ git checkout │  │ git checkout │  │ git checkout │        │
    │  │ /src /tests  │  │ /src /tests  │  │ /src /tests  │        │
    │  │ No API keys │  │ No API keys │  │ No API keys │        │
    │  │ No agent SW  │  │ No agent SW  │  │ No agent SW  │        │
    │  └──────────────┘  └──────────────┘  └──────────────┘        │
    │                                                              │
    │  Egress: only git provider + control plane                   │
    └──────────────────────────────────────────────────────────────┘
```

---

## 3. Integration Layers

The integration happens at five distinct layers, each preserving the independence of both systems.

### Layer 1: Workspace as Execution Substrate

**What changes:** OpenFlows pair harness provisions Coder workspaces instead of local git worktrees.

**Current OpenFlows:**
- Local git worktrees for pair isolation
- File-based shared state (`pair-N/shared/`)
- Local process spawning (Claude Code / Codex CLI)

**With Coder:**
- Each FORGE-SENTINEL pair gets a Coder workspace provisioned from a template
- Templates define compute (CPU/RAM), network policies, pre-installed tooling
- Workspace daemon handles file I/O, shell execution — same path IDEs use
- No agent software in the workspace; the control plane drives everything

**Mapping:**

| OpenFlows Concept | Coder Equivalent |
|---|---|
| Git worktree `worktrees/pair-N/` | Coder workspace (template-based) |
| `git worktree add` | `create_workspace` (Coder API) |
| Local process spawn (CLI) | Agent loop → workspace daemon tool calls |
| `pair-N/shared/STATUS.json` | SharedStore (Redis) or workspace file via `write_file` |
| File lock directory | Coder workspace isolation (pair-1 can't access pair-2's workspace) |

**Key benefit:** Network isolation per workspace. Agent workspaces can be locked down to only reach the git provider. No LLM API keys ever enter the workspace.

### Layer 2: PocketFlow-in-Coder (Agent Loop Replacement)

**What changes:** The OpenFlows NEXUS agent loop replaces the Coder Agents agent loop as the routing engine, while using Coder's workspace connection infrastructure.

**Current Coder Agents:**
- Single agent loop: prompt → LLM → tool calls → workspace → repeat
- `spawn_agent` for parallel sub-tasks
- Chat-based interaction model

**With OpenFlows:**
- PocketFlow flow graph replaces the sequential agent loop
- NEXUS orchestrates routing via action strings (not chat prompts)
- FORGE-SENTINEL pairs operate as structured agents with prep/exec/post phases
- VESSEL runs deterministic merge logic (no LLM needed)
- Sub-agents are replaced by the flow graph's natural parallel execution

**The PocketFlow Node trait maps to Coder's workspace tools:**

```rust
// OpenFlows Node trait (unchanged)
#[async_trait]
pub trait Node: Send + Sync {
    fn name(&self) -> &str;
    async fn prep(&self, store: &SharedStore) -> Result<Value>;
    async fn exec(&self, prep_result: Value) -> Result<Value>;
    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action>;
}

// New: CoderTransport — executes tool calls via Coder workspace daemon
#[async_trait]
impl CoderTransport {
    async fn read_file(&self, workspace_id: &str, path: &str) -> Result<String>;
    async fn write_file(&self, workspace_id: &str, path: &str, content: &str) -> Result<()>;
    async fn edit_files(&self, workspace_id: &str, edits: Vec<FileEdit>) -> Result<()>;
    async fn execute(&self, workspace_id: &str, command: &str) -> Result<CommandOutput>;
    async fn create_workspace(&self, template_id: &str, params: Value) -> Result<Workspace>;
    async fn start_workspace(&self, workspace_id: &str) -> Result<()>;
}
```

### Layer 3: Identity and Governance Bridge

**What changes:** Every OpenFlows agent action inherits the submitting user's Coder identity.

**Current OpenFlows:**
- Actions use a GitHub Personal Access Token
- No per-user identity tracing
- No centralized audit log
- No model governance

**With Coder:**
- Each OpenFlows flow run is initiated by a Coder-authenticated user
- FORGE commits, PR opens, and code pushes all attribute to the user's identity
- LLM API calls route through Coder's model governance (approved providers, system prompts)
- Full audit trail: which user triggered which flow, what workspaces were provisioned, what actions were taken
- Administrators control available models, system prompts, and tool permissions centrally

**Security model mapping:**

| Property | OpenFlows Standalone | OpenFlows + Coder |
|---|---|---|
| API key management | Per-agent env vars, in worktree | Control plane only, zero workspace exposure |
| User identity | GitHub PAT (shared) | Coder SSO identity per action |
| Network isolation | None (agents need network) | Workspace egress restricted to git provider + control plane |
| Audit logging | Event ring buffer in SharedStore | Coder audit log + SharedStore events |
| Model governance | Per-agent .env config | Centralized Coder admin panel |
| Template governance | N/A | Admin-controlled workspace templates with scoped permissions |

### Layer 4: MCP Tool Bridge

**What changes:** Coder's workspace management becomes MCP tools available to OpenFlows agents within the flow.

**Current OpenFlows:**
- GitHub MCP server for PR/issue operations
- Filesystem MCP server scoped to worktree
- Shell MCP server with allowlist
- Custom orchestration tools via Claude Code / Codex plugins

**With Coder:**
- New `coder-workspace` MCP server provides workspace lifecycle tools
- Existing MCP servers continue to work inside workspaces
- The pair harness provisions workspace-scoped MCP configs instead of local ones

**New MCP tools provided by OpenFlows Coder integration:**

```
coder_create_workspace
  template_id: string    // Coder template name
  name: string          // Workspace name (e.g., "forge-1-T-42")
  parameters: object    // Template parameters (branch, repo, etc.)
  
coder_start_workspace
  workspace_id: string
  
coder_stop_workspace
  workspace_id: string
  
coder_read_file
  workspace_id: string
  path: string
  
coder_write_file
  workspace_id: string
  path: string
  content: string
  
coder_execute
  workspace_id: string
  command: string
  
coder_list_templates
  // Returns available Coder templates scoped to user's permissions
```

These are registered as MCP servers in the Coder control plane and available to OpenFlows agents via the integration layer.

### Layer 5: Hybrid Deployment Model

**What changes:** Organizations can adopt incrementally — OpenFlows standalone, Coder-only, or fully integrated.

**Three deployment modes:**

| Mode | Description | Use Case |
|---|---|---|
| **OpenFlows Standalone** | Current architecture — local worktrees, local agents, SharedStore | Individual developers, small teams, open-source contributors |
| **Coder + OpenFlows Integrated** | Coder workspaces + OpenFlows orchestration — full architecture | Enterprises, regulated industries, teams needing governance |
| **Coder Only** | Coder Agents without OpenFlows orchestration — single-agent chat | Teams wanting workspace governance without multi-agent flows |

---

## 4. Detailed Component Mapping

### 4.1 NEXUS (Orchestrator)

**Current:** Runs as an LLM-driven agent in the main OpenFlows process. Reads SharedStore, calls GitHub API, makes routing decisions.

**With Coder:** NEXUS runs inside the Coder control plane as part of the OpenFlows orchestration engine. It gains:

- `list_templates` / `read_template` to select workspace templates for each ticket type
- `create_workspace` to provision pair workspaces on-demand
- User-scoped template visibility (NEXUS can only provision templates the initiating user can access)
- Audit trail for every orchestration decision

**NEXUS prep() with Coder (pseudocode):**

```rust
async fn prep(&self, store: &SharedStore) -> Result<Value> {
    // Existing logic: sync issues, reconcile state
    let tickets = self.sync_issues(store).await?;
    let recovery = self.reconcile(store).await?;
    
    // NEW: Check available workspace templates
    let templates = self.coder.list_templates().await?;
    
    // NEW: For each assigned worker, ensure workspace exists
    for slot in &worker_slots {
        if matches!(slot.status, WorkerStatus::Assigned { .. }) {
            let ws = self.coder.get_or_create_workspace(
                &slot.id, 
                "openflows-forge",  // template name
                json!({ "branch": format!("forge-{}/{}", slot.id, ticket_id) })
            ).await?;
        }
    }
    
    Ok(json!({ "tickets": tickets, "recovery": recovery, "workspaces": workspaces }))
}
```

### 4.2 FORGE-SENTINEL Pair

**Current:** Each pair gets a local git worktree. FORGE and SENTINEL are CLI processes managed by the pair harness with skills, hooks, and MCP tools.

**With Coder:** Each pair gets a Coder workspace. The pair harness provisions the workspace instead of a local worktree.

**Provisioning sequence (Coder mode):**

```
1. NEXUS assigns ticket T-42 to worker forge-1
2. Pair harness calls create_workspace:
   - Template: "openflows-forge" (pre-configured with dev tools, restricted network)
   - Name: "forge-1-T-42"
   - Parameters: { repo: "org/repo", branch: "forge-1/T-42" }
3. Workspace provisions (Terraform template applies)
4. OpenFlows provisions workspace internals:
   - .claude/settings.json (or .codex/config.toml) → via write_file tool
   - .claude/mcp.json → via write_file tool
   - orchestration/plugin/ → via write_file tool
   - shared/ directory → via SharedStore (not filesystem)
5. Agent loop drives FORGE/SENTINEL via workspace daemon tool calls
6. When pair completes, workspace is stopped (or destroyed)
```

**Key change:** The `shared/` directory (TICKET.md, PLAN.md, CONTRACT.md, STATUS.json, etc.) moves to SharedStore keys rather than filesystem files. This is more reliable than file-based coordination across workspace boundaries, and it eliminates the need for the workspace to have persistent state.

**Shared state migration:**

| File (current) | SharedStore key (Coder mode) | Notes |
|---|---|---|
| `shared/STATUS.json` | `pair:{id}:status` | Already structured JSON — natural fit |
| `shared/WORKLOG.md` | `pair:{id}:worklog` | Markdown stored as string value |
| `shared/TICKET.md` | `pair:{id}:ticket` | Written by NEXUS, read by FORGE |
| `shared/PLAN.md` | `pair:{id}:plan` | Written by FORGE, read by SENTINEL |
| `shared/CONTRACT.md` | `pair:{id}:contract` | Written by SENTINEL, read by FORGE |
| `shared/HANDOFF.md` | `pair:{id}:handoff` | Written by FORGE on context reset |
| `shared/segment-N-eval.md` | `pair:{id}:segment:{N}:eval` | Written by SENTINEL per segment |
| `shared/final-review.md` | `pair:{id}:final_review` | Written by SENTINEL at end |

### 4.3 VESSEL (Merge Gatekeeper)

**Current:** Deterministic Rust code that polls CI status via GitHub API and squash-merges PRs.

**With Coder:** VESSEL gains workspace access for conflict resolution:

- After detecting merge conflicts, VESSEL can use `coder_execute` to run `git merge origin/main` in the pair's workspace
- Conflict resolution instructions are written via Coder's `write_file` tool instead of local filesystem
- VESSEL can stop workspaces after successful merge to free compute

**No fundamental change** — VESSEL is already deterministic. The Coder integration primarily adds workspace lifecycle management.

### 4.4 LORE (Documentarian)

**Current:** Reads events from SharedStore, generates documentation.

**With Coder:** LORE can publish documentation to workspace repos via Coder's write_file tool, and can access workspace file content for generating changelogs.

### 4.5 Command Gate (Permission System)

**Current:** FORGE proposes dangerous commands via CommandGate in SharedStore. NEXUS (LLM) approves or rejects.

**With Coder:** The Coder control plane can enforce additional constraints:

- Administrators define which commands are allowed in agent workspace templates
- The control plane can enforce tool call allowlists per workspace template
- `propose_plan` and `ask_user_question` from Coder Agents map naturally to OpenFlows' existing plan review (CONTRACT.md negotiation)

---

## 5. Pair Harness Provisioning (Coder Mode)

The pair harness gains a new `CoderProvisioner` alongside the existing `LocalProvisioner`:

```rust
// crates/pair-harness/src/provision.rs

pub enum ProvisionerKind {
    Local,      // Current: git worktrees + local filesystem
    Coder,      // New: Coder workspaces via control plane API
}

pub struct CoderProvisioner {
    client: CoderClient,         // HTTP client to Coder control plane
    template_name: String,      // e.g., "openflows-forge"
    user_token: String,          // Coder session token for the initiating user
}

impl CoderProvisioner {
    async fn provision_pair(&self, pair_id: &str, config: &PairConfig) -> Result<PairWorkspace> {
        // 1. Create or reuse workspace via Coder API
        let workspace = self.client.create_workspace(CreateWorkspaceRequest {
            template_name: self.template_name.clone(),
            name: format!("{}-{}", pair_id, config.ticket_id),
            parameters: json!({
                "repo_url": config.repo_url,
                "branch": format!("forge-{}/{}", pair_id, config.ticket_id),
            }),
        }).await?;

        // 2. Wait for workspace to be ready
        self.client.wait_for_workspace_ready(&workspace.id, Duration::from_secs(120)).await?;

        // 3. Provision agent configuration inside workspace
        self.provision_agent_config(&workspace.id, pair_id, config).await?;

        // 4. Return workspace handle (not a local path)
        Ok(PairWorkspace::Coder(CoderWorkspace {
            id: workspace.id,
            pair_id: pair_id.to_string(),
            transport: CoderTransport::new(self.client.clone(), &workspace.id),
        }))
    }

    async fn provision_agent_config(&self, workspace_id: &str, pair_id: &str, config: &PairConfig) -> Result<()> {
        let transport = CoderTransport::new(self.client.clone(), workspace_id);

        // Write settings.json (FORGE or SENTINEL)
        let settings = if config.role == Role::Forge {
            create_forge_settings()
        } else {
            create_sentinel_settings()
        };
        transport.write_file(".claude/settings.json", &serde_json::to_string_pretty(&settings)?).await?;

        // Write MCP configuration
        let mcp_config = create_mcp_config(pair_id, config);
        transport.write_file(".claude/mcp.json", &serde_json::to_string_pretty(&mcp_config)?).await?;

        // Write TICKET.md, TASK.md to SharedStore instead of filesystem
        // (these are read from SharedStore by the agent in Coder mode)

        Ok(())
    }
}
```

### Template Requirements

The `openflows-forge` Coder template must include:

```hcl
# Terraform template for OpenFlows FORGE workspaces
resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"
}

resource "coder_git_auth" "github" {
  # Uses Coder's built-in GitHub external auth
  # No PAT needed in workspace
}

# Network policy: allow only git provider and control plane
resource "coder_network_policy" "forge" {
  egress_rules = [
    { destination = "github.com", ports = [22, 443] },
    { destination = "coder-control-plane", ports = [443] },
  ]
  # All other egress blocked
}
```

---

## 6. Security Model Enhancement

### Current OpenFlows Security

- GitHub PAT in workspace environment (`.claude/mcp.json` or `.env`)
- Local file locking for multi-pair isolation
- Hook scripts enforce guard rails (`pre_bash_guard.sh`, `pre_write_check.sh`)
- Pair worktrees isolated by Git branch semantics
- No centralized audit trail
- No user identity per action

### Enhanced Security with Coder

```
┌──────────────────────────────────────────────────────────────────┐
│                    THREAT MODEL COMPARISON                      │
├──────────────────────┬───────────────────┬──────────────────────┤
│ Threat               │ OpenFlows Only    │ OpenFlows + Coder    │
├──────────────────────┼───────────────────┼──────────────────────┤
│ API key exfiltration  │ PAT in workspace  │ Keys in control     │
│                      │ (extractable)      │ plane only           │
├──────────────────────┼───────────────────┼──────────────────────┤
│ Cross-pair access    │ File locks (flock) │ Workspace isolation  │
│                      │                   │ (kernel-level)        │
├──────────────────────┼───────────────────┼──────────────────────┤
│ Unauthorized commands│ Hook scripts      │ Coder template policy │
│                      │ (bypassable)       │ + hooks (belt+braces)│
├──────────────────────┼───────────────────┼──────────────────────┤
│ Network exfiltration │ No restriction    │ Egress firewall per   │
│                      │                   │ template              │
├──────────────────────┼───────────────────┼──────────────────────┤
│ Audit trail          │ SharedStore events│ Coder audit log       │
│                      │ (volatile)         │ + SharedStore events  │
├──────────────────────┼───────────────────┼──────────────────────┤
│ User attribution     │ GitHub PAT per bot│ Per-user Coder SSO    │
│                      │ (single identity)  │ identity              │
├──────────────────────┼───────────────────┼──────────────────────┤
│ Secret scanning      │ Git hooks on push │ Coder template policy │
│                      │ (push rejection)   │ + git hooks            │
└──────────────────────┴───────────────────┴──────────────────────┘
```

### Network Isolation Per Pair

Each FORGE workspace gets strict egress rules:

```
workspace-1 egress:
  ALLOW tcp/443 → github.com (git push/pull)
  ALLOW tcp/443 → coder-control-plane (workspace daemon heartbeat)
  DENY all other outbound
  
workspace-2 egress:
  (same rules — identical isolation per pair)
```

No workspace needs access to LLM providers. All model inference happens in the control plane. This eliminates an entire class of data exfiltration vectors.

### Identity Inheritance

```
User (SSO) → Coder session → OpenFlows flow run → workspace
  │              │                    │                │
  │              │                    │                ├─ FORGE commits as user
  │              │                    │                ├─ PR attributed to user
  │              │                    │                └─ Audit log entries as user
  │              │                    │
  │              │                    └─ NEXUS decisions logged as user
  │              │
  │              └─ Model calls billed to user's team
  │
  └─ Cannot access other users' workspaces or templates
```

---

## 7. Flow Graph (Updated for Coder)

The PocketFlow flow graph remains the core routing mechanism. The only change is that workspace lifecycle actions are now Coder API calls instead of local git operations.

### Updated Routing Table

```
┌──────────┐  work_assigned  ┌──────────────────┐  pr_opened  ┌─────────┐
│          │ ──────────────> │                  │ ──────────> │         │
│  NEXUS   │                 │ FORGE-SENTINEL    │             │ VESSEL  │
│          │ <────────────── │                  │ <────────── │         │
└──────────┘  failed/         └──────────────────┘  deployed/  └─────────┘
    │         suspended         │                    deploy_failed
    │                            │                    merge_blocked
    │         merge_prs         │  conflicts_        no_work
    │ ──────────────────────────+  detected            │
    │                            │                     │
    │         no_work           +─────────────────────+
    │ ──────────> (loop)       │
    │                            │
    │         create_workspace   │  (NEW: provision workspace)
    │ ──────────────────────────+
    │         stop_workspace     │  (NEW: cleanup after merge)
    │ ──────────────────────────+
```

### New Actions

| Action | Source | Target | Meaning |
|---|---|---|---|
| `create_workspace` | NEXUS | Coder API | Provision workspace for assigned pair |
| `stop_workspace` | VESSEL | Coder API | Stop workspace after PR merged |
| `destroy_workspace` | NEXUS (recovery) | Coder API | Destroy workspace for exhausted/failed ticket |

These are Coder API calls made during the Node's `exec()` phase, not routing actions. They don't change the flow graph topology — they change the implementation of existing nodes.

---

## 8. Context Compaction and Chat Persistence

### Current OpenFlows

OpenFlows has a built-in context reset mechanism: the `pre_compact_handoff` hook intercepts context compaction, writes a HANDOFF.md, and the pair harness spawns a fresh session that reads the handoff. This is a hard reset — graceful but coarse.

### With Coder

Coder Agents provides automatic context compaction: when token usage exceeds a threshold, the model generates a compressed summary and inserts it as a new message. Earlier messages remain in the database for audit but are excluded from the model's context window.

**Integration approach:**

- The existing HANDOFF.md mechanism continues to work for hard resets (session kills, process restarts)
- Coder's automatic compaction serves as a softer, more granular context management layer
- Chat persistence in Coder's database means full conversation history survives workspace stops and rebuilds
- The pair harness can detect a resumed Coder chat and skip handoff reading in favor of the existing Coder conversation context

---

## 9. Plan Mode Integration

Coder Agents offers a Plan Mode where the agent inspects the workspace and presents a plan before implementation. This maps directly to the existing FORGE-SENTINEL plan review cycle:

| Coder Plan Mode | OpenFlows Equivalent |
|---|---|
| `propose_plan` | FORGE writes PLAN.md |
| `ask_user_question` | FORGE asks SENTINEL clarification questions |
| Plan review | SENTINEL writes CONTRACT.md (APPROVED or ISSUES) |
| "Implement plan" | FORGE begins segment implementation |

**Synergy:** Coder's plan mode adds user visibility and intervention capability. When a user sees a Coder chat associated with their OpenFlows flow, they can review the plan before implementation starts — a human-in-the-loop checkpoint that the current system doesn't offer outside of BLOCKED status.

---

## 10. Implementation Roadmap

### Phase 1: CoderTransport (Non-Breaking)

**Goal:** Add a `CoderTransport` abstraction that can execute workspace operations via Coder's API, parallel to the existing local transport.

```rust
// crates/pair-harness/src/workspace.rs

#[async_trait]
pub trait WorkspaceTransport: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    async fn execute(&self, command: &str) -> Result<CommandOutput>;
    async fn list_directory(&self, path: &str) -> Result<Vec<DirEntry>>;
}

pub struct LocalTransport { /* current git worktree + local fs */ }
pub struct CoderTransport { /* HTTP client to Coder control plane */ }
```

This phase changes nothing for existing users. The `LocalTransport` continues to work exactly as before. `CoderTransport` is a new addition that enables Coder integration.

**Files to modify:**
- `crates/pair-harness/src/workspace.rs` — add trait + CoderTransport
- `crates/pair-harness/src/provision.rs` — add CoderProvisioner alongside LocalProvisioner
- `crates/pair-harness/src/pair.rs` — parameterize transport

### Phase 2: SharedStore Migration for Pair State

**Goal:** Move pair communication artifacts from filesystem (`shared/`) to SharedStore (Redis), making them accessible from any workspace.

```rust
// New keys in SharedStore
const PAIR_STATUS: &str = "pair:{pair_id}:status";       // was: shared/STATUS.json
const PAIR_WORKLOG: &str = "pair:{pair_id}:worklog";     // was: shared/WORKLOG.md
const PAIR_TICKET: &str = "pair:{pair_id}:ticket";      // was: shared/TICKET.md
const PAIR_PLAN: &str = "pair:{pair_id}:plan";           // was: shared/PLAN.md
const PAIR_CONTRACT: &str = "pair:{pair_id}:contract";   // was: shared/CONTRACT.md
const PAIR_HANDOFF: &str = "pair:{pair_id}:handoff";     // was: shared/HANDOFF.md
const PAIR_EVAL: &str = "pair:{pair_id}:segment:{n}:eval"; // was: shared/segment-N-eval.md
const PAIR_FINAL: &str = "pair:{pair_id}:final_review";   // was: shared/final-review.md
```

This is a prerequisite for Coder mode because workspaces can't share a local filesystem.

**Files to modify:**
- `crates/pair-harness/src/pair.rs` — read/write from SharedStore instead of local `shared/`
- `crates/pair-harness/src/isolation.rs` — remove file-based locking (workspace isolation replaces it)
- All hooks — read from SharedStore instead of filesystem

### Phase 3: Coder Provisioner

**Goal:** Full Coder workspace lifecycle management.

- `CoderProvisioner::provision_pair()` creates Coder workspaces
- `CoderProvisioner::cleanup_pair()` stops/destroys workspaces after merge
- NEXUS gains `list_templates` / `create_workspace` capabilities
- VESSEL gains `stop_workspace` after successful merge

**Files to modify:**
- `crates/pair-harness/src/provision.rs` — add `CoderProvisioner`
- `crates/config/src/state.rs` — add workspace ID to `WorkerSlot`
- `crates/agent-nexus/src/lib.rs` — add template selection logic
- `crates/agent-vessel/src/node.rs` — add workspace cleanup on merge

### Phase 4: Governance Integration

**Goal:** Coder's admin panel governs OpenFlows agent behavior.

- Model selection: Administrators choose available LLM providers
- System prompt governance: Central system prompts enforced per Coder config
- Tool permissions: Per-template tool allowlists
- Audit log: All OpenFlows actions flow through Coder audit
- Cost controls: Per-user/per-team spending limits on LLM inference

### Phase 5: MCP Bridge

**Goal:** Register OpenFlows orchestration tools as Coder MCP servers.

- `flow_status` MCP tool: Query SharedStore for current flow state
- `list_workers` MCP tool: Show active pair workspaces
- `approve_command` MCP tool: Human-in-the-loop command approval
- These tools appear in the Coder Agents chat UI, allowing users to interact with running OpenFlows flows

---

## 11. Failure Recovery Under Coder

### Current OpenFlows Recovery

NEXUS `reconcile()` detects inconsistencies:
- Orphaned tickets (Assigned but worker is Idle)
- Stale workers (Working but ticket is Open)
- Unmerged PRs (pending_prs entries not processed)
- Completed without PR

### Enhanced Recovery with Coder

| Scenario | OpenFlows Only | OpenFlows + Coder |
|---|---|---|
| Pair workspace crashes | Harness detects process exit, NEXUS recycles worker | Coder detects workspace health, NEXUS recycles worker + restarts workspace |
| Network failure mid-flow | SharedStore state survives, NEXUS reconciles on restart | Coder database persists all chat history, NEXUS reconciles on restart |
| Context window exhaustion | `pre_compact_handoff` hook writes HANDOFF.md | Coder auto-compaction + HANDOFF.md for hard resets |
| Workspace becomes unreachable | Manual cleanup needed | NEXUS calls `stop_workspace` + `create_workspace` for fresh workspace |
| Merge conflict | VESSEL writes CONFLICT_RESOLUTION.md to local filesystem | VESSEL writes via Coder `write_file` to workspace, FORGE reads it |
| Stalled workspace | Watchdog timer in pair harness | Coder workspace timeout + pair harness watchdog |

### Workspace Recovery Flow

```
NEXUS reconcile() detects:
  - Worker W assigned to ticket T
  - Workspace WS for W is stopped/crashed
  - No STATUS.json or HANDOFF.md written
  
Action:
  1. NEXUS calls Coder API: start_workspace(WS)
     ┌──── If workspace starts ─────────────────────────────────┐
     │  2a. Resume pair in existing workspace                    │
     │  3a. FORGE reads HANDOFF.md from last checkpoint         │
     │  4a. Continue from exact step                            │
     └────────────────────────────────────────────────────────────┘
     
     ┌──── If workspace cannot start ────────────────────────┐
     │  2b. NEXUS creates new workspace from same template    │
     │  3b. Git checkout of same branch (forge-W/T branch)    │
     │  4b. FORGE reads HANDOFF.md from SharedStore           │
     │  5b. Continue from exact step in fresh workspace       │
     └────────────────────────────────────────────────────────┘
```

This is a significant improvement over the current model where a crashed local process requires manual intervention.

---

## 12. Comparison: Standalone vs Integrated

| Feature | OpenFlows Standalone | OpenFlows + Coder |
|---|---|---|
| **Workspace provisioning** | Local git worktrees | Coder workspace templates |
| **Agent execution** | CLI processes (Claude/Codex) | Coder workspace daemon tool calls |
| **State coordination** | Filesystem (shared/) | SharedStore (Redis) + Coder DB |
| **Isolation** | File locking (flock) | Kernel-level workspace isolation |
| **API key security** | PAT in workspace env | Control plane only (zero workspace exposure) |
| **User identity** | Shared GitHub PAT | Per-user Coder SSO |
| **Audit** | SharedStore events | Coder audit log + SharedStore events |
| **Network isolation** | None | Per-template egress rules |
| **Model governance** | Per-agent .env | Centralized admin panel |
| **Cost control** | None | Per-user/per-team spending limits |
| **Context management** | HANDOFF.md (hard reset) | Auto-compaction + HANDOFF.md |
| **Chat persistence** | Process-local | Coder database |
| **Plan review** | SENTINEL + CONTRACT.md | SENTINEL + Coder plan mode |
| **Workspace recovery** | Manual | Automatic via Coder API |
| **Scalability** | Limited by local resources | Terraform-defined compute per workspace |
| **Template governance** | N/A | Admin-controlled templates per role |
| **Air-gap capable** | Yes (local) | Yes (Coder runs on-prem/air-gapped) |

---

## 13. Decision Record

### DR-001: SharedStore over filesystem for pair state

**Context:** In Coder mode, workspaces don't share a local filesystem. Pair communication artifacts (STATUS.json, PLAN.md, etc.) must be accessible across workspaces.

**Decision:** Migrate `shared/` artifacts to SharedStore (Redis) keys with namespace `pair:{id}:{artifact}`. Keep filesystem-based `shared/` for standalone mode.

**Consequences:**
- Both modes work; no breaking change
- Redis becomes required for Coder mode (already optional for standalone)
- Pair harness reads from SharedStore in Coder mode, local filesystem in standalone mode

### DR-002: CoderTransport as an abstraction, not a replacement

**Context:** The existing pair harness uses local filesystem operations. Coder mode needs HTTP API calls to workspace daemons.

**Decision:** Introduce `WorkspaceTransport` trait with `LocalTransport` (existing) and `CoderTransport` (new) implementations. The harness selects based on configuration.

**Consequences:**
- Zero breaking changes to existing users
- New `CoderTransport` implementation is additive
- Tests can verify both transports

### DR-003: Workspace lifecycle in NEXUS, not a separate agent

**Context:** Coder workspace lifecycle (create, start, stop) needs a driver. This could be a new agent or an existing one.

**Decision:** Add workspace lifecycle calls to NEXUS's `post()` method. NEXUS already orchestrates the flow, so workspace provisioning is a natural extension of ticket assignment.

**Consequences:**
- No new agent to maintain
- NEXUS gains `list_templates` and `create_workspace` tools
- Workspace cleanup is added to VESSEL's post-merge flow

### DR-004: Keep PocketFlow as the routing engine

**Context:** Coder Agents has its own agent loop (prompt → LLM → tool calls → repeat). OpenFlows has PocketFlow (Node trait → action routing → next node). These are fundamentally different orchestration models.

**Decision:** OpenFlows uses PocketFlow for orchestration routing. Coder's agent loop is used for FORGE and SENTINEL's individual workspace interactions (file reads, shell commands, code edits), not for flow control.

**Consequences:**
- OpenFlows retains its multi-agent coordination advantage
- Coder's single-agent loop is used for what it's good at: workspace tool execution
- NEXUS, VESSEL, and LORE continue running as Rust code (not LLM-driven for NEXUS, not at all for VESSEL)
- Sub-agent delegation (Coder's `spawn_agent`) is replaced by PocketFlow's action routing

---

## 14. Open Questions

1. **MCP tool registration:** Should OpenFlows register its orchestration tools as Coder MCP servers, or should the integration be purely through the control plane API? (Leaning: both — Coder MCP for user-facing interactions, control plane API for agent-internal operations.)

2. **Chat model for FORGE/SENTINEL:** In Coder mode, should FORGE and SENTINEL use Coder's built-in agent loop for individual workspace operations, or should OpenFlows continue driving them via its own prompt construction? (Leaning: OpenFlows drives prompts, but uses Coder's tool execution layer for workspace operations.)

3. **Billing and cost attribution:** How should LLM costs be attributed when a user's OpenFlows flow invokes multiple agents across multiple workspaces? (Leaning: Per-flow cost with breakdown per pair, attributed to the initiating user.)

4. **Workspace template selection:** Should the user choose the template, or should NEXUS select based on ticket type? (Leaning: NEXUS selects based on ticket labels/repository, with user override available.)

5. **Multi-tenant isolation:** If multiple users run OpenFlows flows simultaneously, should each flow get isolated SharedStore namespaces? (Leaning: Yes — `flow:{user_id}:{flow_id}:` prefix on all SharedStore keys.)

---

## 15. Summary

OpenFlows and Coder are complementary systems. Coder governs **where** agents run — providing workspace infrastructure, identity, audit, and network isolation. OpenFlows governs **how** agents coordinate — providing the flow graph, typed contracts, multi-agent orchestration, and self-healing reconciliation.

The integration preserves both systems' independence while combining their strengths:

- **Coder gets orchestration intelligence** — the Coder agent loop is augmented by PocketFlow's action-routing, enabling multi-agent coordinated workflows that Coder's single-agent chat model cannot express
- **OpenFlows gets governance** — every agent action inherits user identity, API keys leave the workspace, network egress is controlled, templates govern compute, and a full audit trail exists
- **Together they form a complete enterprise AI development platform** — architecture-first orchestration running on governed, isolated, auditable infrastructure

The five-phase implementation roadmap adds Coder integration incrementally without breaking existing OpenFlows deployments. Each phase is independently valuable and can be deployed separately.