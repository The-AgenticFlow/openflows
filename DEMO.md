# OpenFlows Demo Guide

This guide walks you through running a complete autonomous development cycle, from zero to a working application. OpenFlows runs in two modes — **Coder mode** (ephemeral governed workspaces, recommended) and **Local mode** (local git worktrees). Both paths are covered.

## Prerequisites

1. **API Keys Required:**
   - `GITHUB_PERSONAL_ACCESS_TOKEN` — For GitHub MCP operations (create PRs, push code)
   - A provider key — in Coder mode with AI Gateway, this lives in the Coder control plane (or `litellm_config.yaml` as fallback); in Local mode it goes in `.env`:
     - `ANTHROPIC_API_KEY` — Claude Code (forge worker)
     - `OPENAI_API_KEY` / `GEMINI_API_KEY` / `GROQ_API_KEY` — for AUX per-role routing in LiteLLM config

2. **Tools Required:**
   - Docker 24+ — only for Coder mode (`docker compose --profile coder up`)
   - Rust 1.70+ (`rustc --version`) — only for building from source / Local mode
   - Node.js 18+ (`node --version`) — for the GitHub MCP server
   - Claude Code CLI (`claude --version`) — only required on the host for Local mode; in Coder mode it installs inside the workspace via the [Coder Registry module](https://registry.coder.com)

## Setup

### 1. Clone and Configure

```bash
git clone https://github.com/The-AgenticFlow/openflows.git
cd openflows

# Copy environment template
cp .env.example .env

# Edit .env with your keys
nano .env  # or your preferred editor
```

### 2. Environment Variables

Your `.env` file should contain:

```env
# LLM Provider for Nexus orchestrator
LLM_PROVIDER=openai
OPENAI_API_KEY=sk-...
# GEMINI_API_KEY=AIza...

# For Claude Code (forge worker)
ANTHROPIC_API_KEY=sk-ant-...

# GitHub access for creating PRs and pushing code
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_...

# Target repository (where issues will be worked on)
GITHUB_REPOSITORY=your-username/your-repo
```

#### If your gateway only supports OpenAI format

Add these additional variables and start the local proxy before running:

```env
# Claude CLI sends Anthropic requests to the LOCAL proxy
PROXY_URL=http://localhost:8080/v1
PROXY_API_KEY=your-gateway-api-key

# The LOCAL proxy forwards OpenAI-format requests to the REMOTE gateway
GATEWAY_URL=https://api.ai.camer.digital/v1/
GATEWAY_API_KEY=your-gateway-api-key
```

```bash
# Terminal 1: Start the proxy (reads .env automatically)
./scripts/start_proxy.sh

# Terminal 2: Run the orchestration
cargo run --bin openflows
```

### 3. Prepare Target Repository

Create a GitHub repository where the autonomous team will work. You can use an existing repo or create a new one:

```bash
# Example: Create a new test repository
gh repo create my-calculator --public --clone
cd my-calculator
echo "# My Calculator" > README.md
git add README.md && git commit -m "Initial commit" && git push
```

Create some issues for the agents to work on:

```bash
gh issue create --title "Implement calculator core logic" --body "Create a basic calculator with add, subtract, multiply, divide operations"
gh issue create --title "Add UI styling" --body "Style the calculator with a modern look using CSS"
```

## Running the Demo

### Start the Orchestration

**Coder mode (recommended) — full ephemeral workspace stack:**

```bash
docker compose --profile coder up
```

The `CoderBootstrapper` provisions the Coder admin user and pushes the role workspace templates on startup. NEXUS then creates an ephemeral Coder workspace per agent per ticket, with the [Claude Code Coder Registry module](https://registry.coder.com/coder/claude-code/coder) and AI Gateway enabled (LLM keys stay in the control plane).

**Local mode — agents in local git worktrees:**

If you have direct Anthropic API access (or a LiteLLM proxy that supports Anthropic format):

```bash
# From OpenFlows directory
cargo run --bin openflows
```

**If your gateway only supports OpenAI format** (needs the local Anthropic-to-OpenAI proxy):

```bash
# Terminal 1: Start the protocol proxy
./scripts/start_proxy.sh

# Terminal 2: Run the orchestration
cargo run --bin openflows
```

### What Happens During Execution

```
[Step 0] NEXUS Node
    |
    |-- 1. Syncs worker slots from registry.json (reads workspace_provider per agent)
    |-- 2. Calls list_issues via GitHub MCP to discover open issues
    |-- 3. Matches issues to available workers
    |-- 4. Outputs decision: {"action": "work_assigned", "assign_to": "forge-1", ...}
    |-- 5. (Coder mode) Provisions an ephemeral Coder workspace from the
    |       openflows-forge template; the claude-code module installs the CLI
    |       and wires up AI Gateway routing + managed_settings permissions
    |
    v
[Step 1] FORGE Node (for each assigned worker)
    |
    |-- 1. (Local) creates git worktree in workspaces/<repo>/worktrees/forge-1/
    |   (Coder) boots inside the ephemeral Coder workspace — harness binary
    |       + agent CLI run inside, git pull on the persistent volume
    |-- 2. Spawns Claude Code with forge.agent.md persona (acceptEdits)
    |-- 3. Claude Code:
    |       |-- Reads the GitHub issue
    |       |-- Writes PLAN.md → CONTRACT.md (the FORGE-SENTINEL planning cycle)
    |       |-- Implements the solution against the contract
    |       |-- Writes tests
    |       |-- Creates STATUS.json
    |       |-- Opens a PR
    |-- 4. Parses STATUS.json and updates worker status in SharedStore
    |
    v
[Step 2] NEXUS Node (loop)
    |
    |-- Dispatches the PR to SENTINEL for adversarial review
    |-- Checks for completed work, open PRs, or blocked workers
    |-- NEXUS::reconcile() runs — detects orphans, stale workers,
    |   unmerged PRs, and crashed workspaces (Coder mode), recovering
    |-- Assigns more work if available
    |-- Or returns "no_work" if nothing to do
```

## Monitoring Progress

### Watch the Logs

```bash
# Main orchestration logs appear in terminal

# Worker-specific logs are saved to:
tail -f ~/.agentflow/workspaces/<owner>-<repo>/forge/workers/forge-1/worker.log
```

### Check the Worktree

```bash
# See what files the agent created/modified
ls -la ~/.agentflow/workspaces/<owner>-<repo>/worktrees/forge-1/

# Check git status in worktree
cd ~/.agentflow/workspaces/<owner>-<repo>/worktrees/forge-1/
git status
git log --oneline -5
```

### Check for STATUS.json

```bash
# After work completes, check the status file
cat ~/.agentflow/workspaces/<owner>-<repo>/worktrees/forge-1/STATUS.json
```

Example STATUS.json:
```json
{
  "ticket": "T-001",
  "status": "complete",
  "summary": "Implemented calculator core logic with all operations",
  "pr": "https://github.com/owner/repo/pull/123",
  "commits": ["abc1234 Implement calculator", "def5678 Add tests"],
  "artifacts": ["src/calculator.js", "tests/calculator.test.js"]
}
```

## Architecture Overview

```
OpenFlows/
|-- orchestration/agent/
|   |-- agents/
|   |   |-- nexus.agent.md    # Orchestrator persona (plan)
|   |   |-- forge.agent.md    # Builder persona (acceptEdits)
|   |   |-- sentinel.agent.md # Reviewer persona (plan)
|   |   |-- vessel.agent.md   # DevOps persona (acceptEdits)
|   |   `-- lore.agent.md     # Writer persona (acceptEdits, disabled by default)
|   |-- registry.json         # team + Coder modules + workspace_provider config
|
|-- crates/
|   |-- pocketflow-core/      # Flow engine, SharedStore, routing (Node trait)
|   |-- agent-nexus/          # Nexus node — reconcile() failure recovery
|   |-- agent-forge/          # Forge node — CONTRACT.md → implementation
|   |-- agent-sentinel/       # Sentinel node — adversarial review
|   |-- agent-vessel/         # Vessel node — CI merge + workspace teardown
|   |-- agent-lore/           # Lore node — documentation
|   |-- agent-client/         # LLM client + MCP integration
|   |-- pair-harness/         # Worktree/workspace management, transport, provisioner
|   |-- coder-client/         # Coder REST/SSH client + CoderBootstrapper (feature-gated)
|   `-- notifier/             # awaiting_human escalation (Slack/Discord)
|
|-- binary/src/bin/
|   |-- agentflow.rs          # Live orchestration entry point (openflows)
|   |-- demo.rs               # Mocked demo
|
|-- docker-compose.yml        # Coder + Postgres + LiteLLM + Redis + OpenFlows
|-- .env                      # Your API keys (not in git)
```

## Flow Diagram

```
                    +-----------------+
                    |    START        |
                    +--------+--------+
                             |
                             v
                    +-----------------+
                    |    NEXUS        |
                    |  (Orchestrator) |
                    +--------+--------+
                             |
            +----------------+----------------+
            |                                 |
            v                                 v
    +---------------+                 +---------------+
    | work_assigned |                 |   no_work     |
    +-------+-------+                 +-------+-------+
            |                                 |
            v                                 v
    +---------------+                 +---------------+
    |    FORGE      |                 |    END        |
    |   (Builder)   |                 +---------------+
    +-------+-------+
            |
    +-------+-------+
    |               |
    v               v
+--------+    +-----------+
| success|    |  failed   |
+---+----+    +-----+-----+
    |               |
    +-------+-------+
            |
            v
    +---------------+
    |    NEXUS      |
    |   (loop)      |
    +---------------+
```

## Troubleshooting

### "GitHub MCP server failed to initialize"
- Check `GITHUB_PERSONAL_ACCESS_TOKEN` is valid
- Ensure token has `repo` and `write:org` scopes

### "No issues found"
- Verify `GITHUB_REPOSITORY` is correct (format: `owner/repo`)
- Check that the repository has open issues (not PRs)

### "Claude Code timed out"
- Default timeout is 30 minutes
- Complex tasks may need longer - adjust in `agent-forge/src/lib.rs`

### "FORGE exited quickly without progress" or "Failed to authenticate. API Error: 403"
- Your gateway likely doesn't support the Anthropic Messages API (`/v1/messages`)
- Start the local proxy: `./scripts/start_proxy.sh`
- Ensure `GATEWAY_URL` and `GATEWAY_API_KEY` are set in `.env`
- See the [OpenAI-only gateways](#if-your-gateway-only-supports-openai-format) section

### "Worker status stuck on 'assigned'"
- Check worker logs for errors
- Verify Claude Code CLI is installed and accessible

## Example: Building a Calculator from Zero

Here's a complete example of running the autonomous team to build a calculator app:

### 1. Create Target Repository

```bash
gh repo create my-calculator --public
cd my-calculator
echo "# Calculator" > README.md
git add . && git commit -m "init" && git push
```

### 2. Create Issues

```bash
gh issue create --title "Core Logic" --body "Implement basic calculator operations: add, subtract, multiply, divide. Use React with Vite."
gh issue create --title "Modern UI" --body "Style the calculator with a modern glassmorphism design using Tailwind CSS."
```

### 3. Update .env

```env
GITHUB_REPOSITORY=your-username/my-calculator
```

### 4. Run Orchestration

```bash
cd openflows
cargo run --bin openflows
```

### 5. Watch the Magic

The agents will:
1. Discover the open issues
2. Assign issue #1 to forge-1
3. Forge-1 will:
   - Create a Vite + React project
   - Implement calculator logic
   - Add tests
   - Open a PR
4. Nexus will assign issue #2 to forge-2
5. Forge-2 will:
   - Add Tailwind CSS
   - Style the calculator
   - Open a PR

### 6. Review Results

```bash
# Check the PRs
gh pr list

# View the deployed worktree
cd ~/.agentflow/workspaces/your-username-my-calculator/worktrees/forge-1/
npm run dev
```

## Next Steps

- Read [CONTRIBUTING.md](CONTRIBUTING.md) for development guidelines
- See [docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md) for architecture details
- Customize agent personas in `orchestration/agent/agents/*.agent.md`
