# OpenFlows: Understanding and Extending the Agent System

## What Is an Agent in OpenFlows?

In OpenFlows, an **agent** is not just an LLM model or a CLI tool. It is a complete, self-contained unit composed of two distinct halves:

```
Agent = CLI Backend + Agent Configuration (Plugin System)
```

The **CLI Backend** (e.g., `claude`, `codex`) is the executable process that runs as a subprocess, receives prompts via stdin, and produces output via stdout/stderr. It is the "muscle" that actually generates code, runs commands, and interacts with the filesystem.

The **Agent Configuration** (the plugin system) is the entire filesystem-based harness that orchestrates, constrains, and empowers that CLI. It includes persona definitions, skills, hooks, permissions, settings files, MCP configurations, and the coordination directory structure. It is the "brain" that tells the CLI *what* to do, *how* to do it, and *what* it is allowed to touch.

These two halves are **decoupled**. You can swap the CLI backend used by existing agents without touching their roles or configurations. And you can add entirely new agent roles that reuse existing CLI backends. These are two independent extension paths.

---

## Anatomy of an Agent Configuration

Every agent configuration lives under `orchestration/` and is composed of these layers in OpenFlows:

### 1. Agent Definition (`orchestration/agent/agents/*.agent.md`)

Each agent has a Markdown file with YAML frontmatter that defines its identity, role, CLI preference, permissions, and persona:

```yaml
---
id: forge
role: builder
cli: auto
active: true
github: forge-openflows
slack: "@forge"
---
```

The `cli` field controls which backend runs the agent: `claude`, `codex`, or `auto` (defer to registry default). The persona section that follows is the agent's character — the instructions, constraints, and behavioral guidelines injected into every session.

### 2. Team Registry (`orchestration/agent/registry.json`)

The registry is the single source of truth for team membership. Each entry specifies:

| Field | Purpose |
|-------|---------|
| `id` | Agent identifier (`nexus`, `forge`, `sentinel`, `vessel`, `lore`) |
| `cli` | Backend override (`claude`, `codex`, or `auto`) |
| `active` | Whether the agent participates in the flow |
| `instances` | How many parallel workers (e.g., forge-1, forge-2) |
| `model_backend` | LLM routing path (e.g., `fireworks/accounts/fireworks/models/kimi-k2p6`) |
| `routing_key` | LiteLLM proxy key for multi-model routing |
| `github_token_env` | Per-agent GitHub token environment variable |

The registry resolves CLI backends through a three-priority chain:

1. Agent-specific `cli` field (highest)
2. `DEFAULT_CLI` environment variable
3. `default_cli` in registry.json (fallback)

### 3. Plugin System (`orchestration/plugin/`)

The plugin system deploys capabilities to agents through structured directories:

| Directory | Contents |
|-----------|----------|
| `skills/` | Per-agent knowledge packs (e.g., `forge-coding/SKILL.md`, `sentinel-review/SKILL.md`) |
| `hooks/` | Lifecycle shell scripts (`session_start.sh`, `pre_bash_guard.sh`, `post_write_lint.sh`) |
| `commands/` | Slash commands (`assign.md`, `check-ci.md`, `handoff.md`) |
| `mcp/` | MCP server configuration templates (`mcp.json.template`) |
| `.codex-plugin/` | Codex-specific plugin manifest and configuration |
| `plugin.json` | Master manifest binding skills, hooks, commands, and MCP to agent roles |

### 4. Hooks System

Hooks are shell scripts executed at specific lifecycle events. They are per-agent and cover:

| Hook | When Fired | Purpose |
|------|------------|---------|
| `session_start` | Agent session begins | Initialize context, show directory structure |
| `pre_bash_guard` | Before dangerous bash | Security gate — prevent destructive commands |
| `pre_write_check` | Before file write | Ownership/locking validation |
| `post_write_lint` | After file write | Lint/format enforcement |
| `pre_compact_handoff` | Before context reset | Write `HANDOFF.md` continuation document |
| `stop_require_artifact` | Before agent stops | Ensure `STATUS.json` is written |
| `subagent_start` / `subagent_stop` | Sub-agent lifecycle | Setup and teardown |

The `hooks.json` manifest maps hook names to script paths for each agent role.

### 5. Coordination Directory Structure

When a pair is provisioned, each worktree receives:

```
worktree/
├── .claude/              # Claude-specific config (or .codex/ for Codex)
│   ├── settings.json     # API keys, permissions, hooks
│   ├── mcp.json          # MCP server registrations
│   ├── plugins/          # Symlink to orchestration plugin
│   ├── hooks/            # Copied hook scripts
│   └── skills/           # Symlinked skill directories
├── .agents/              # Codex-specific (when using codex)
│   ├── plugins/
│   └── skills/
├── AGENTS.md             # Combined persona instructions
├── .gitignore            # Excludes runtime directories
└── .pair-shared/         # Coordination files between agents
    ├── TICKET.md         # Work item description
    ├── PLAN.md           # Implementation plan
    ├── CONTRACT.md       # Plan approval/rejection
    ├── WORKLOG.md        # Progress log
    ├── STATUS.json       # Current state machine value
    ├── CI_FIX.md         # CI failure instructions
    ├── CONFLICT_RESOLUTION.md  # Merge conflict instructions
    └── logs/             # Process stdout/stderr capture
```

---

## How the Agent Runtime Works in OpenFlows: BackendConfig and ProcessManager

The core abstraction that makes multi-backend support possible is `BackendConfig` (`crates/pair-harness/src/process.rs`). It encapsulates everything specific to a CLI backend:

```rust
pub struct BackendConfig {
    pub binary_path: PathBuf,           // CLI executable
    pub base_flags: Vec<String>,        // Always-passed flags
    pub forge_flags: Vec<String>,       // FORGE-mode flags
    pub forge_pr_flags: Vec<String>,    // PR-creation flags
    pub sentinel_flags: Vec<String>,    // SENTINEL-mode flags
    pub api_key_env: String,            // API key environment variable name
    pub base_url_env: Option<String>,   // Proxy URL env var (e.g., OPENAI_BASE_URL)
    pub model_env: Option<String>,      // Model override env var
    pub home_env_var: Option<String>,   // Backend-specific home dir (e.g., CODEX_HOME)
    pub home_dir_suffix: String,        // Suffix for isolated config
    pub plugin_dir_rel: PathBuf,        // Plugin directory relative to worktree
    pub settings_rel: PathBuf,          // Settings file relative to worktree
    pub uses_stdin_prompt: bool,        // Whether CLI accepts stdin
    pub mcp_config_rel: PathBuf,        // MCP config path
    pub needs_extras_provisioning: bool,// Run backend-specific provisioning
    pub forge_extra_args: Vec<String>,  // Extra args for FORGE mode
    pub sentinel_extra_args: Vec<String>, // Extra args for SENTINEL mode
}
```

The `ProcessManager` holds a `HashMap<CliBackend, BackendConfig>` and provides factory methods for each backend. Currently it registers `Claude` and `Codex`:

```rust
// In ProcessManager::new()
let mut backends = HashMap::new();
backends.insert(CliBackend::Claude, BackendConfig::claude(...));
backends.insert(CliBackend::Codex, BackendConfig::codex(...));

// Adding a new backend:
// backends.insert(CliBackend::Opencode, BackendConfig::opencode(...));
```

The `Provisioner` (`crates/pair-harness/src/provision.rs`) deploys all configuration files into each agent's working directory before spawning. It generates settings, MCP configs, symlinks skills, installs hooks, and produces permission profiles — all driven by `BackendConfig` paths.

The `ProcessManager` spawns agents by:

1. Building the command from `BackendConfig` binary + flags
2. Injecting model and API environment variables
3. Setting working directory to the worktree
4. Piping stdin/stdout/stderr for process monitoring
5. Injecting coordination environment variables (`SPRINTLESS_PAIR_ID`, `SPRINTLESS_TICKET_ID`, etc.)
6. Writing the initial prompt to stdin

---

## Two Independent Extension Paths

OpenFlows supports two fundamentally different ways to extend the system. They are **independent** — you can do one without the other:

| Path | What Changes | What Stays the Same |
|------|-------------|---------------------|
| **A: New CLI Backend** | Add `BackendConfig` for a new tool (e.g., OpenCode). Existing agents run on it. | Agent roles, personas, skills, hooks, registry entries untouched. |
| **B: New Agent Role** | Add a 6th team member (persona, registry entry, skills, hooks, optional Node). | Existing CLI backends (claude, codex) are reused. No Rust code changes needed if using an existing backend. |

---

## Path A: Adding a New CLI Backend (No New Agents)

This path lets you make your existing agents (nexus, forge, sentinel, vessel, lore) run on a new CLI tool. No new agent roles are created. The team structure stays the same — only the execution engine changes.

### What You Touch
- **Rust code**: `crates/config/src/registry.rs`, `crates/pair-harness/src/process.rs`, `crates/pair-harness/src/provision.rs`
- **Config files**: `orchestration/agent/registry.json` (change `cli` field on existing entries)

### What You Don't Touch
- Agent persona files (`.agent.md`)
- Skill definitions
- Hook scripts (you can reuse existing ones)
- The `AgentRole` enum
- Flow wiring in `binary/src/main.rs`

### Example: Adding OpenCode as a CLI Backend

#### Step 1: Add the Backend Variant

Add `Opencode` to the `CliBackend` enum in `crates/config/src/registry.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CliBackend {
    #[default]
    Claude,
    Codex,
    Opencode,  // NEW
}

impl std::str::FromStr for CliBackend {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "codex" => CliBackend::Codex,
            "claude" => CliBackend::Claude,
            "opencode" => CliBackend::Opencode,  // NEW
            _ => CliBackend::Claude,
        })
    }
}

impl CliBackend {
    pub fn binary_name(&self) -> &'static str {
        match self {
            CliBackend::Claude => "claude",
            CliBackend::Codex => "codex",
            CliBackend::Opencode => "opencode",  // NEW
        }
    }

    pub fn path_env_var(&self) -> &'static str {
        match self {
            CliBackend::Claude => "CLAUDE_PATH",
            CliBackend::Codex => "CODEX_PATH",
            CliBackend::Opencode => "OPENCODE_PATH",  // NEW
        }
    }
}
```

#### Step 2: Define the BackendConfig

Add `BackendConfig::opencode()` in `crates/pair-harness/src/process.rs`:

```rust
impl BackendConfig {
    /// Create an OpenCode CLI backend config.
    pub fn opencode(opencode_path: &str, worktree: &Path, shared: &Path) -> Self {
        let binary = if opencode_path.is_empty() {
            "opencode"
        } else {
            opencode_path
        };

        // Adjust all paths to match OpenCode's actual CLI interface.
        // This example mirrors the Codex pattern.
        Self {
            binary_path: PathBuf::from(binary),
            base_flags: vec![
                "--non-interactive".into(),
                "--output-format".into(),
                "json".into(),
            ],
            forge_flags: vec![],
            forge_pr_flags: vec![],
            sentinel_flags: vec!["--ephemeral".into()],
            api_key_env: "OPENCODE_API_KEY".into(),
            base_url_env: Some("OPENCODE_BASE_URL".into()),
            model_env: Some("OPENCODE_MODEL".into()),
            home_env_var: Some("OPENCODE_HOME".into()),
            home_dir_suffix: ".opencode-home".into(),
            plugin_dir_rel: PathBuf::from(".opencode")
                .join("plugins")
                .join("orchestration"),
            settings_rel: PathBuf::from(".opencode").join("config.json"),
            uses_stdin_prompt: true,
            mcp_config_rel: PathBuf::from(".opencode").join("mcp.json"),
            needs_extras_provisioning: true,
            forge_extra_args: vec![
                "--config".into(),
                worktree
                    .join(".opencode")
                    .join("config.json")
                    .to_string_lossy()
                    .to_string(),
            ],
            sentinel_extra_args: vec![
                "-C".into(),
                shared.to_string_lossy().to_string(),
            ],
        }
    }
}
```

#### Step 3: Register the Backend

Add OpenCode to `get_backend_config()` and register it in `ProcessManager::new()`:

```rust
pub fn get_backend_config(
    backend: CliBackend,
    worktree: &Path,
    shared: &Path,
) -> BackendConfig {
    match backend {
        CliBackend::Claude => { /* ... */ }
        CliBackend::Codex => { /* ... */ }
        CliBackend::Opencode => {
            let path = std::env::var("OPENCODE_PATH")
                .unwrap_or_else(|_| "opencode".to_string());
            BackendConfig::opencode(&path, worktree, shared)
        }
    }
}

// In ProcessManager::new() or ::with_redis():
backends.insert(
    CliBackend::Opencode,
    BackendConfig::opencode(
        &std::env::var("OPENCODE_PATH")
            .unwrap_or_else(|_| "opencode".to_string()),
        worktree,
        shared,
    ),
);
```

#### Step 4: Add Provisioning Logic

Extend `Provisioner::provision_backend_extras()` in `crates/pair-harness/src/provision.rs` to handle OpenCode-specific filesystem setup:

```rust
fn provision_backend_extras(...) -> Result<()> {
    let is_codex = backend_config.mcp_config_rel.starts_with(".codex");
    let is_opencode = backend_config.mcp_config_rel.starts_with(".opencode");

    if is_codex {
        // Existing Codex provisioning...
    } else if is_opencode {
        // Generate .opencode/config.json
        self.generate_opencode_config(worktree, shared, ...)?;
        // Install hooks into .opencode/hooks/
        self.generate_opencode_hooks_json(worktree, shared)?;
        // Deploy plugin to .opencode/plugins/
        self.deploy_opencode_plugin(worktree)?;
        self.deploy_opencode_plugin(shared)?;
        // Symlink skills to .opencode/skills/
        self.symlink_skills_to_opencode(worktree)?;
        self.symlink_skills_to_opencode_for_role(shared, "sentinel")?;
    } else {
        // Claude provisioning...
    }
    // ...
}
```

The provisioning methods mirror the existing Codex/Claude patterns but target `.opencode/` paths instead.

#### Step 5: Activate on Existing Agents

Now your existing agents can use OpenCode. Change their `cli` field in `orchestration/agent/registry.json`:

```json
{
  "team": [
    { "id": "nexus",    "cli": "opencode", "active": true, "instances": 1, ... },
    { "id": "forge",    "cli": "opencode", "active": true, "instances": 2, ... },
    { "id": "sentinel", "cli": "opencode", "active": true, "instances": 1, ... },
    { "id": "vessel",   "cli": "opencode", "active": true, "instances": 1, ... },
    { "id": "lore",     "cli": "opencode", "active": false, "instances": 1, ... }
  ]
}
```

Or use per-agent overrides in their `.agent.md` files:

```yaml
---
id: forge
role: builder
cli: opencode    # was "auto" or "claude"
active: true
---
```

That's it. The same 5 agents now run on OpenCode. No new roles, no new personas, no new skills. Just a different execution engine.

---

## Path B: Adding a New Agent Role (Optionally with a New Backend)

This path extends the team by adding a 6th agent type. You can back it with any CLI — an existing one (`claude`, `codex`), or a newly added one from Path A.

### What You Touch
- **Config files**: `orchestration/agent/registry.json` (new entry), `orchestration/agent/agents/*.agent.md` (new persona), `orchestration/plugin/plugin.json` (new role mapping)
- **Plugin assets**: `orchestration/plugin/skills/{newagent}-*/`, `orchestration/plugin/hooks/{newagent}/`
- **Optional Rust**: `crates/config/src/identity.rs` (add `AgentRole` variant if hardcoding), `crates/agent-{newagent}/` (Node trait if behavioral integration), `binary/src/main.rs` (flow wiring)

### What You Don't Touch (If Reusing Existing Backend)
- `CliBackend` enum
- `BackendConfig` struct
- `ProcessManager` registration
- `Provisioner` provisioning logic (existing code paths handle the new `.agent.md`)

### Example: Adding an "Analyst" Agent Role Using Existing Claude Backend

#### Step 1: Create Agent Definition

Add `orchestration/agent/agents/analyst.agent.md`:

```yaml
---
id: analyst
role: researcher
cli: claude
active: true
github: analyst-openflows
slack: "@analyst"
---

# Persona
You are ANALYST, a research assistant who investigates codebases, identifies
architectural patterns, and produces technical reports. You read deeply,
synthesize findings across multiple files, and produce structured output.

## Capabilities
- Architecture analysis and dependency mapping
- Security audit pattern recognition
- Performance bottleneck identification
- Technical debt assessment

## Permissions
allow: [Read, Bash, WebFetch, MCP_Github]
deny: [Write, Edit, GitPush, Slack]
```

#### Step 2: Register in Team Registry

Add to `orchestration/agent/registry.json`:

```json
{
  "team": [
    { "id": "nexus",     "cli": "codex", "active": true,  "instances": 1, ... },
    { "id": "forge",     "cli": "codex", "active": true,  "instances": 2, ... },
    { "id": "sentinel",  "cli": "codex", "active": true,  "instances": 1, ... },
    { "id": "vessel",    "cli": "codex", "active": true,  "instances": 1, ... },
    { "id": "lore",      "cli": "codex", "active": false, "instances": 1, ... },
    { "id": "analyst",   "cli": "claude", "active": true,  "instances": 1,
      "model_backend": "anthropic/claude-sonnet-4-5",
      "routing_key": "analyst-key",
      "github_token_env": "AGENT_ANALYST_GITHUB_TOKEN" }
  ]
}
```

#### Step 3: Update Plugin Manifest

Add analyst skills and hooks to `orchestration/plugin/plugin.json`:

```json
{
  "name": "orchestration",
  "version": "3.0.0",
  "skills": {
    "forge":    [/* existing */],
    "sentinel": [/* existing */],
    "nexus":    [/* existing */],
    "vessel":   [/* existing */],
    "lore":     [/* existing */],
    "analyst":  [
      "skills/analyst-architecture.md",
      "skills/analyst-security.md",
      "skills/shared-claude-api.md"
    ]
  },
  "hooks": {
    "forge":    "hooks/forge/",
    "sentinel": "hooks/sentinel/",
    "nexus":    "hooks/nexus/",
    "vessel":   "hooks/vessel/",
    "lore":     "hooks/lore/",
    "analyst":  "hooks/analyst/"
  },
  "commands": "commands/"
}
```

#### Step 4: Create Skills and Hooks

Create `orchestration/plugin/skills/analyst-architecture/SKILL.md`:

```yaml
---
name: Architecture Analysis
description: Systematic codebase architecture review methodology
---

When performing architecture analysis, follow these steps:
1. Map the dependency graph from entry points
2. Identify abstraction layers and their boundaries
3. Flag circular dependencies and tight coupling
4. Document interface contracts between modules
5. Produce a summary with risk ratings (High/Medium/Low)
```

Create `orchestration/plugin/hooks/analyst/session_start.sh`:

```bash
#!/bin/bash
echo "ANALYST session initialized. Research mode active. Write-only operations disabled."
```

#### Step 5: (Optional) Add AgentRole Variant and Flow Wiring

Only needed if the analyst integrates directly into the PocketFlow state machine. If it just runs as a CLI subprocess on file events, skip this step.

Add to `AgentRole` in `crates/config/src/identity.rs`:

```rust
pub enum AgentRole {
    Nexus, Forge, Sentinel, Vessel, Lore, Analyst,  // NEW
}
```

If behavioral integration is needed, create `crates/agent-analyst/` implementing the `Node` trait:

```rust
pub struct AnalystNode { /* ... */ }

#[async_trait]
impl Node for AnalystNode {
    fn name(&self) -> &str { "analyst" }

    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        // Read tickets, PRs, or research targets from store
    }

    async fn exec(&self, prep_result: Value) -> Result<Value> {
        // Run analysis LLM call or subprocess
    }

    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        // Write report to store, route to next node
    }
}
```

Wire it into `binary/src/main.rs`:

```rust
let analyst = Arc::new(AnalystNode::new(...));
flow = flow.add_node("analyst", analyst, vec![
    (ACTION_RESEARCH_REQUESTED, "analyst"),
    (ACTION_ANALYSIS_COMPLETE, "nexus"),
]);
```

---

## Combined Path: New Backend + New Agent

When you need both — a new CLI tool AND a new agent role — follow Path A first, then Path B. The OpenCode backend becomes available to all agents (existing and new). The analyst role from Path B can use `cli: "opencode"` in its registry entry.

---

## Design Principles in OpenFlows

The OpenFlows architecture follows several key principles:

1. **Separation of Concerns**: The CLI backend only knows how to run a process. The configuration knows how to orchestrate it. The Node knows the behavioral flow.

2. **Configuration-as-Code**: Every agent capability (skills, hooks, permissions) is defined in files, not hardcoded. Adding a capability means adding a file, not changing Rust code.

3. **Backend Agnostic**: The `BackendConfig` abstraction means any CLI that accepts prompts via stdin, produces output via stdout, and respects a settings file can be integrated.

4. **Zero-Downtime Team Changes**: The registry is reloaded on every poll cycle. Adding/removing agents, changing models, or toggling active status requires only editing `registry.json`.

5. **Isolated Workspaces**: Each agent pair gets its own worktree and shared directory with backend-specific config paths. Agents cannot interfere with each other's filesystem state.

6. **Lifecycle Hooks**: The hook system provides extension points at every critical moment. Security gates, lint enforcement, context management, and artifact validation all hook through this system.

7. **Decoupled Extension Paths**: Adding a CLI backend is independent from adding an agent role. You can swap out the engine without touching the team, or grow the team without touching the engine.

---

## Environment Variables for New Backends

Each backend follows a consistent env var convention:

| Variable | Purpose | Default |
|----------|---------|---------|
| `{BACKEND}_PATH` | CLI binary location | Binary name in PATH |
| `{BACKEND}_API_KEY` | Authentication | — |
| `{BACKEND}_BASE_URL` | API endpoint/proxy | Vendor default |
| `{BACKEND}_MODEL` | Model name override | Vendor default |
| `{BACKEND}_HOME` | Isolated config directory | System home |
| `DEFAULT_CLI` | Global CLI fallback | `claude` |

For OpenCode, this means: `OPENCODE_PATH`, `OPENCODE_API_KEY`, `OPENCODE_BASE_URL`, `OPENCODE_MODEL`, `OPENCODE_HOME`.

---

## Summary

An agent in OpenFlows is a **CLI process wrapped in a complete configuration harness**. The CLI provides the execution engine, while the configuration provides identity, persona, skills, hooks, permissions, and coordination protocols.

**Extending the system has two independent paths:**

| Path | Goal | Requires Rust Changes? |
|------|------|----------------------|
| **A: New CLI Backend** | Make existing agents run on a new tool | Yes (`BackendConfig` + `CliBackend` enum + `Provisioner`) |
| **B: New Agent Role** | Add a new team member | Only if behavioral `Node` integration needed; otherwise config-only |

The architecture is designed so that Path B is often configuration-only (just `.agent.md`, `registry.json`, skills, hooks), while Path A is the only one requiring Rust code. Once a backend is added via Path A, any number of agent roles can use it via Path B with zero additional Rust changes.
