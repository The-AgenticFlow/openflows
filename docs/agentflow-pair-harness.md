# Building AgentFlow: How We Run Claude and Codex in the Same Pair Harness

Running a single AI agent to write production code doesn't work. Context windows fill up, the agent forgets architectural decisions from three hours ago, and there's no quality gate before changes are committed. We needed a builder and a reviewer working together — but that introduces three hard problems: isolation, state coordination, and the ability to switch between Claude Code and OpenAI Codex without rewriting everything.

This is how we solved it.

---

## The Pair Model

Every ticket in AgentFlow gets a pair: a FORGE agent that writes code, and a SENTINEL agent that reviews it. Both run on the same git worktree, but with different permissions.

```
                    PAIR N working on ticket #123

┌─────────────────────────┐         ┌──────────────────────────────┐
│         FORGE           │         │         SENTINEL            │
│       (builder)         │◄───────►│       (reviewer)            │
│                         │         │                              │
│  - Writes code          │         │  - Read-only on worktree    │
│  - Creates PLAN.md      │         │  - Writes CONTRACT.md       │
│  - Segments work        │         │  - Evaluates segments       │
│  - Pushes commits       │         │  - Gives final approval     │
│                         │         │                              │
│  Can use: Claude OR Codex          Can use: Claude OR Codex    │
└──────────┬──────────────┘         └──────────────┬───────────────┘
           │                                        │
           └────────────────┬───────────────────────┘
                            │
                   ┌─────────▼──────────┐
                   │   .pair-shared/    │
                   │   Shared state    │
                   │                    │
                   │  TICKET.md         │  ← Original issue
                   │  PLAN.md          │  ← FORGE's plan
                   │  CONTRACT.md      │  ← SENTINEL's approval
                   │  WORKLOG.md       │  ← Progress log
                   │  STATUS.json      │  ← State machine
                   │  segment-N-eval.md│  ← Evaluations
                   └────────────────────┘

┌────────────────────────────────────────────────────────────────────┐
│                    Git Worktree (Isolated Branch)                  │
│                                                                    │
│  Branch: forge-pair-N/ticket-123                                   │
│  Full repo clone with .git history                                 │
│  FORGE writes here; SENTINEL reads from here                     │
│                                                                    │
│  ├── src/           ← Source code                                  │
│  ├── tests/         ← Test files                                   │
│  ├── .pair-shared/  ← Coordination (inside worktree)              │
│  └── .git/          ← Git history                                  │
└────────────────────────────────────────────────────────────────────┘
```

The shared directory lives **inside** the worktree, not alongside it. This matters because Codex uses a filesystem sandbox, and paths outside the workspace root require special handling that had bugs in early versions. By nesting `.pair-shared/` inside the worktree, it inherits the sandbox boundary naturally. The directory is gitignored so coordination files never end up in commits.

Each pair works on its own git branch (`forge-pair-N/ticket-XXX`), so multiple pairs run concurrently without interfering. A human can `cd` into any worktree, run `git log`, and see exactly what the agent did.

---

## The State Machine

FORGE and SENTINEL coordinate through files in `.pair-shared/`. The lifecycle looks like this:

FORGE starts by reading `TICKET.md` and writes `PLAN.md` — a segmented work plan with specific files to change. SENTINEL reads the plan and writes back `CONTRACT.md`: either `status: AGREED` with acceptance criteria, or `status: ISSUES` with specific objections. This contract prevents the common failure mode where an agent writes a vague plan like "I will implement the feature" with no specifics. SENTINEL enforces specificity.

Once contracted, FORGE implements one segment at a time, writing progress to `WORKLOG.md`. After each segment, SENTINEL evaluates it and writes `segment-N-eval.md` with `APPROVED` or `NEEDS_WORK`. If rejected, FORGE revises. If approved, the next segment begins.

When all segments complete, FORGE writes `DONE.md`. SENTINEL performs a final review and writes `final-review.md` with `APPROVED` or `REJECTED`. On approval, the pair is done and a PR can be created.

`STATUS.json` tracks the current state for the orchestration layer to monitor.

---

## Why We Support Both Claude and Codex

We didn't want to lock into one CLI tool. Claude Code excels at long-context reasoning and planning. Codex is purpose-built for code generation with streaming execution and native sandboxing. Different tasks benefit from different models, and we wanted the freedom to experiment.

The challenge: Claude and Codex have completely different configuration formats, permission models, and plugin ecosystems. Claude uses `.claude/settings.json`, `.claude/skills/`, and hook-based policy enforcement. Codex uses `.codex/config.toml`, `.agents/skills/`, and an OS-level sandbox.

Our solution: abstract all backend differences into a single `BackendConfig` struct.

---

## The BackendConfig Abstraction

Instead of sprinkling `if backend == "claude"` throughout spawn logic, we captured everything backend-specific in one place:

- Binary name and path
- Flags for base, FORGE, and SENTINEL modes
- Environment variable names for API key, base URL, and model override
- Plugin directory path (`.claude/plugins/` vs `.agents/plugins/`)
- Settings file path (`.claude/settings.json` vs `.codex/config.toml`)
- Whether the backend needs a home directory (Codex uses `CODEX_HOME`)
- Whether extras provisioning is needed

When spawning a process, the orchestration layer calls `build_cli_command(backend)`, which uses the registered `BackendConfig` for that backend. Adding a new CLI tool means adding one constructor. The spawn logic, provision logic, process management, and state machine don't change.

This is what the two backends look like at spawn time:

**Claude Code FORGE:**
```
claude --print --dangerously-skip-permissions --output-format stream-json \
       --settings .claude/settings.json --plugin-dir .claude/plugins/orchestration
```

**OpenAI Codex FORGE:**
```
codex exec --sandbox workspace-write \
       -c model_provider="fireworks" \
       -c model_providers.fireworks.base_url="https://api.fireworks.ai/inference/v1" \
       ...
```

Claude skips all permission prompts with `--dangerously-skip-permissions` and enforces safety through hooks. Codex uses `--sandbox workspace-write` for native filesystem isolation. The orchestration layer doesn't care which approach is used — it just gets the right command.

---

## The Provisioning Pipeline

When a new pair is created, the `Provisioner` runs a single pipeline that generates backend-specific files from the same source of truth.

The pipeline has five steps:

1. **Settings/config files** — `.claude/settings.json` for Claude (JSON with hooks and permissions), `.codex/config.toml` for Codex (TOML with sandbox profiles and agent personas).

2. **MCP server configuration** — Claude gets a separate `.claude/mcp.json` file. Codex embeds MCP servers inline in its `config.toml`. Same servers (GitHub API, filesystem, shell with allowlist), different formats.

3. **Plugin directory symlink** — The `orchestration/plugin/` directory (containing skills, hooks, and commands) is symlinked into each backend's plugin path. Claude sees it at `.claude/plugins/orchestration/`. Codex sees it at `.agents/plugins/orchestration/`.

4. **Backend-specific extras** — This is where the two paths diverge, controlled by a single `if is_codex` branch:
   - Claude: Install hooks to `.claude/hooks/{role}/`, add them to `settings.json`, symlink skills to `.claude/skills/`, enhance permissions in settings.
   - Codex: Generate `.codex/agents/*.toml` personas, generate `.codex/hooks.json`, install hooks to `.codex/hooks/{role}/`, symlink skills to `.agents/skills/`, deploy `.codex-plugin/`, generate `permissions.toml`.

5. **AGENTS.md for both** — Role-specific instructions written to `AGENTS.md` at the worktree root (FORGE) and in `.pair-shared/` (SENTINEL). Both backends read this as their system prompt layer.

The entire provisioner is backend-agnostic except for that one branch in step 4. Everything else (worktree creation, shared directory setup, gitignore management, ticket writing) is identical regardless of backend.

---

## Skill Discovery: One Source, Two Paths

We maintain 37 skills in `orchestration/plugin/skills/` — organized by role:

- **Forge skills**: `forge-coding`, `forge-planning`, `forge-web-artifacts-builder` — how to write idiomatic code, create segmented plans, and build web components.
- **Sentinel skills**: `sentinel-review`, `sentinel-criteria`, `sentinel-webapp-testing` — how to review changes, check acceptance criteria, and test applications.
- **Shared skills**: `shared-claude-api`, `lore-documentation`, `lore-changelog` — patterns both agents need.

The problem: Claude and Codex discover skills from different paths. Claude looks in `.claude/skills/<skill>/SKILL.md` or `<plugin>/skills/<skill>/SKILL.md`. Codex looks in `.agents/skills/<skill>/SKILL.md`.

Our solution: symlink to both known paths. For Claude, we create direct symlinks in `.claude/skills/` and rely on the plugin symlink for the plugin path. For Codex, we symlink to `.agents/skills/`. Both point to the same source directory. If one discovery mechanism breaks in a future CLI update, the other still works.

For SENTINEL, we only symlink role-relevant skills (sentinel-* and shared-*) to reduce context window usage.

---

## Safety: Two Different Approaches

FORGE needs write access. SENTINEL must stay read-only. Both must be blocked from dangerous operations. But Claude and Codex enforce this completely differently.

**Claude Code** has no native sandbox. We use `--dangerously-skip-permissions` to avoid interactive approval prompts (essential for autonomous operation), then enforce everything through lifecycle hooks. The `pre_bash_guard.sh` hook runs before every Bash command, checking against a denylist: rm -rf, sudo, network commands (curl, wget, ssh), package installation, and access to other pairs' worktrees. It exits with code 2 to block the command. The `pre_write_check.sh` hook validates file writes — don't overwrite `CONTRACT.md`, stay within the worktree, follow naming conventions.

**OpenAI Codex** has a real OS sandbox using filesystem bind mounts. We configure it declaratively: FORGE gets `sandbox_mode = "workspace-write"` with network access enabled for GitHub API calls. SENTINEL gets `sandbox_mode = "read-only"` with network disabled. We also install the same hook scripts for additional policy enforcement — belt and suspenders.

Both approaches achieve the same outcome from the orchestration layer's perspective. The provisioning code generates the right config for each backend from the same policy definitions.

---

## AGENTS.md: The Universal System Prompt

Both backends read `AGENTS.md` — a markdown file with role-specific instructions.

The source personas live in `orchestration/agent/agents/`:
- `forge.agent.md` — Builder persona: read standards first, write `STATUS.json` when done, push after each commit, don't modify files outside the worktree.
- `sentinel.agent.md` — Reviewer persona: evaluate against `CONTRACT.md`, check test coverage, flag security issues, be specific in rejections.

At provision time, these are extracted and written to `AGENTS.md` at the worktree root (FORGE) and in `.pair-shared/` (SENTINEL). Claude reads it automatically when entering the directory. Codex loads it through its agent persona system.

This is backend-agnostic institutional knowledge. Whether you're running Claude or Codex, the agent gets the same instructions about code standards, workflow, and safety policies.

---

## Validation

Our test suite validates the entire provisioning and lifecycle system:

- **64 unit tests**: Settings generation for both backends, hook path resolution (relative, never absolute), symlink creation, Codex `exec --json` output parsing, state machine transitions, watchdog stall detection, file locking.
- **5 integration tests**: Full pair lifecycle from assignment to PR creation, watchdog behavior, context reset with handoff generation, file locking under concurrency, worktree provisioning structure.
- **6 proxy routing tests**: Backend config resolution, pair config with and without proxy, registry backward compatibility, per-agent backend override.

Total: 75 tests, all passing. Zero backend-specific conditional logic in tests. If a test validates that settings were generated, it asserts the file exists and has the right structure — whether that file is `.claude/settings.json` or `.codex/config.toml` doesn't matter to the test.

---

## What We Learned

**Abstract early and abstract deep.** The `BackendConfig` struct was the best architectural decision. When we added Codex support, we didn't touch spawn logic, process management, or the state machine. We added one constructor and one branch in the provisioning layer. About 150 lines of new code.

**Copy, don't symlink, for runtime assets.** Hook scripts are copied into each pair's directory rather than symlinked. This makes pairs self-contained. If the source repository moves or is deleted, running pairs continue to work. This matters for long-running pairs that might outlast a deployment.

**Put shared state inside the sandbox boundary.** The shared directory lives inside the worktree rather than alongside it. This eliminated an entire class of Codex sandbox bugs where `--add-dir` reported paths as writable but didn't create the bind mount, causing EROFS errors. No workaround needed.

**Dual-path discovery for reliability.** For Claude skills, we symlink to both `.claude/skills/` and `.claude/plugins/orchestration/skills/`. If Claude's plugin resolution changes in a future version, the direct symlinks still work.

**Shell hooks are the universal policy language.** Both backends can run shell scripts at lifecycle events. The same `pre_bash_guard.sh` enforces the same rules whether it's triggered by Claude's PreToolUse hook or Codex's PreToolUse hook. The orchestration layer doesn't enforce policy — the hooks do.

---

## Try It

The full implementation is at [github.com/christ/agentflow](https://github.com/christ/agentflow). The pair harness is in `crates/pair-harness/`, with the `BackendConfig` abstraction in `src/process.rs` and the provisioning pipeline in `src/provision.rs`.

Claude:
```
ANTHROPIC_API_KEY=... cargo run --bin agentflow
```

Codex:
```
DEFAULT_CLI=codex FIREWORKS_API_KEY=... cargo run --bin agentflow
```

Mixed pair (FORGE=Codex for coding speed, SENTINEL=Claude for review quality):
```
# Edit orchestration/agent/registry.json:
# { "id": "forge", "cli": "codex", ... }
# { "id": "sentinel", "cli": "claude", ... }
ANTHROPIC_API_KEY=... FIREWORKS_API_KEY=... cargo run --bin agentflow
```

The orchestration layer doesn't care which backend you choose. It just works.
