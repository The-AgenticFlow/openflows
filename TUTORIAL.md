# AgentFlow Complete Tutorial: Build an App from Zero

This tutorial walks you through running AgentFlow to autonomously build a web application from scratch. You'll see exactly what logs to expect, which files are created, and where everything happens.

## Table of Contents

1. [Prerequisites Setup](#prerequisites-setup)
2. [Environment Configuration](#environment-configuration)
3. [Creating a Target Project](#creating-a-target-project)
4. [Running the Orchestration](#running-the-orchestration)
5. [Understanding the Logs](#understanding-the-logs)
6. [Inspecting Generated Files](#inspecting-generated-files)
7. [Troubleshooting](#troubleshooting)

---

## Prerequisites Setup

### 1. Install Required Tools

```bash
# Check Rust version (need 1.70+)
rustc --version
# If not installed: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Check Node.js version (need 18+)
node --version
# If not installed: https://nodejs.org/

# Install GitHub CLI (optional but helpful)
gh --version
# If not installed: https://cli.github.com/

# Install Claude Code CLI (REQUIRED for FORGE agent)
claude --version
# If not installed: https://www.anthropic.com/claude-code
```

### 2. Get API Keys

You'll need:

| Key | Purpose | Where to Get |
|-----|---------|--------------|
| `ANTHROPIC_API_KEY` | Powers Claude Code (FORGE agent) | https://console.anthropic.com/ |
| `OPENAI_API_KEY` | Powers NEXUS orchestrator | https://platform.openai.com/api-keys |
| `GEMINI_API_KEY` | Powers NEXUS orchestrator when using Gemini | https://aistudio.google.com/app/apikey |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | GitHub operations (issues, PRs) | https://github.com/settings/tokens |

For the GitHub token, ensure these scopes:
- ✅ `repo` (full control of private repositories)
- ✅ `workflow` (update GitHub Action workflows)
- ✅ `write:packages` (upload packages to GitHub Package Registry)

---

## Environment Configuration

### 1. Clone AgentFlow

```bash
git clone https://github.com/The-AgenticFlow/AgentFlow.git
cd AgentFlow
```

### 2. Create `.env` File

```bash
cp .env.example .env
nano .env  # or use your preferred editor
```

### 3. Configure Your `.env`

```env
# LLM Provider for NEXUS orchestrator
LLM_PROVIDER=openai
OPENAI_API_KEY=sk-proj-xxxxxxxxxxxxx
OPENAI_MODEL=gpt-4o-mini

# Alternative: Use Gemini for NEXUS
# LLM_PROVIDER=gemini
# GEMINI_API_KEY=AIzaSyxxxxxxxxxxxxx
# GEMINI_MODEL=gemini-2.5-flash

# Alternative: Use Anthropic for NEXUS as well
# LLM_PROVIDER=anthropic
# ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxxx

# Claude Code (FORGE agent) - REQUIRED
ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxxx

# GitHub Personal Access Token
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxxxxxxxxxx

# Target repository (format: owner/repo)
GITHUB_REPOSITORY=your-username/test-calculator
```

**⚠️ Important**: NEXUS can use `openai`, `gemini`, or `anthropic`. FORGE still requires `ANTHROPIC_API_KEY` for Claude Code.

#### If your gateway only supports OpenAI format

If you're using a third-party gateway that doesn't support the Anthropic Messages API, you need the local protocol proxy:

```env
# Claude CLI and Nexus send Anthropic requests to the LOCAL proxy
PROXY_URL=http://localhost:8080/v1
PROXY_API_KEY=your-gateway-api-key

# The LOCAL proxy forwards OpenAI-format requests to the REMOTE gateway
GATEWAY_URL=https://api.ai.camer.digital/v1/
GATEWAY_API_KEY=your-gateway-api-key

# Route non-Anthropic models through OpenAI client format
MODEL_PROVIDER_MAP=glm=openai,deepseek=openai,gpt=openai
```

Then start the proxy before running the orchestration:

```bash
# Terminal 1: Start proxy (reads .env automatically)
./scripts/start_proxy.sh

# Terminal 2: Run orchestration
cargo run --bin real_test
```

When your provider adds native Anthropic support, just change `PROXY_URL` to point directly to the gateway and remove `GATEWAY_*`.

### 4. Verify Your Setup

Run the setup checker to ensure everything is configured correctly:

```bash
./scripts/check_setup.sh
```

**Expected output:**

```
🔍 AgentFlow Setup Checker
=============================

1. Checking System Requirements...
-----------------------------------
✓ Rust 1.75.0 is installed
✓ Node.js v20.11.0 is installed
✓ Claude Code CLI is installed (v1.0.0)
✓ Git 2.43.0 is installed
✓ GitHub CLI 2.42.0 is installed (optional)

2. Checking Environment Configuration...
----------------------------------------
✓ .env file exists
✓ ANTHROPIC_API_KEY is set
✓ LLM_PROVIDER is set to: openai
✓ OPENAI_API_KEY is set
✓ GITHUB_PERSONAL_ACCESS_TOKEN is set
✓ GITHUB_REPOSITORY is set to: your-username/test-calculator

3. Checking Project Build...
----------------------------
✓ Cargo.toml found
✓ Project compiles successfully

4. Checking AgentFlow Configuration...
--------------------------------------
✓ NEXUS persona found
✓ FORGE persona found
✓ Worker registry found
✓ Registry has 3 worker slots configured

5. Checking Workspace Directory...
-----------------------------------
⚠ Workspace directory will be created at: /home/christian/.agentflow/workspaces

=============================
✓ All checks passed!

You're ready to run AgentFlow:
  cargo run --bin real_test
```

If any checks fail, follow the error messages to fix the issues.

---

## Creating a Target Project

AgentFlow needs a GitHub repository with issues to work on. Let's create a simple calculator project.

### Option A: Using GitHub CLI

```bash
# Create a new public repository
gh repo create test-calculator --public --clone

cd test-calculator

# Initialize with README
echo "# Calculator App" > README.md
echo "An autonomous AI-built calculator" >> README.md
git add README.md
git commit -m "Initial commit"
git push origin main

# Create issues for the agents to work on
gh issue create \
  --title "Implement calculator core logic" \
  --body "Create a basic calculator web app with HTML/CSS/JavaScript. Support add, subtract, multiply, divide operations. Use a clean, modern design."

gh issue create \
  --title "Add scientific calculator features" \
  --body "Extend the calculator to support scientific operations: sin, cos, tan, sqrt, power, log. Add a toggle to switch between basic and scientific mode."

# Verify issues were created
gh issue list
```

### Option B: Using GitHub Web UI

1. Go to https://github.com/new
2. Create a repository named `test-calculator`
3. Make it public
4. Initialize with a README
5. Go to Issues tab
6. Create 2 issues with the titles and descriptions from Option A

### 3. Update AgentFlow `.env`

```bash
cd /path/to/AgentFlow
nano .env
```

Update the `GITHUB_REPOSITORY` line:
```env
GITHUB_REPOSITORY=your-username/test-calculator
```

---

## Running the Orchestration

### 1. Build and Run

```bash
cd /path/to/AgentFlow

# Build the project (first time only)
cargo build --release --bin real_test

# Run the orchestration
cargo run --bin real_test
```

**Expected output on startup:**

```
2026-03-31T00:00:01.234Z  INFO real_test: Starting REAL End-to-End Orchestration (No Mocks)
2026-03-31T00:00:02.456Z  INFO real_test: Target repository workspace ready workspace=/home/christian/.agentflow/workspaces/your-username-test-calculator
2026-03-31T00:00:02.789Z  INFO real_test: Running orchestration loop for repository: your-username/test-calculator
```

### 2. Understanding the Workspace

AgentFlow creates an isolated workspace structure:

```
~/.agentflow/
└── workspaces/
    └── your-username-test-calculator/
        ├── main/                    # Main repository clone
        ├── worktrees/               # Isolated work areas for each agent
        │   ├── forge-1/             # FORGE worker #1 workspace
        │   ├── forge-2/             # FORGE worker #2 workspace
        │   └── ...
        └── forge/
            └── workers/
                ├── forge-1/
                │   ├── worker.log   # Detailed Claude Code logs
                │   └── STATUS.json  # Work completion status
                └── forge-2/
                    ├── worker.log
                    └── STATUS.json
```

---

## Understanding the Logs

### Step 1: NEXUS Discovers Issues

**You'll see:**

```
2026-03-31T00:00:05.123Z  INFO agent_nexus: Syncing worker slots from registry
2026-03-31T00:00:05.234Z  INFO agent_nexus: Loaded 3 worker slots: ["forge-1", "forge-2", "forge-3"]
2026-03-31T00:00:06.345Z  INFO agent_client::mcp: Initializing GitHub MCP server
2026-03-31T00:00:07.456Z  INFO agent_nexus: Fetching open issues from your-username/test-calculator
2026-03-31T00:00:08.567Z  INFO agent_nexus: Found 2 open issues
2026-03-31T00:00:08.678Z  INFO agent_nexus: Assigning issue #1 "Implement calculator core logic" to forge-1
```

**What's happening:**
1. NEXUS loads available worker slots from [`registry.json`](orchestration/agent/registry.json:1)
2. Connects to GitHub via MCP server
3. Fetches open issues from your repository
4. Assigns first issue to `forge-1`

**Output format:**
```json
{
  "action": "work_assigned",
  "assign_to": "forge-1",
  "ticket": "T-001",
  "issue_number": 1,
  "title": "Implement calculator core logic",
  "description": "Create a basic calculator..."
}
```

### Step 2: FORGE Creates Worktree

**You'll see:**

```
2026-03-31T00:00:10.123Z  INFO agent_forge: Processing work_assigned for worker forge-1
2026-03-31T00:00:10.234Z  INFO pair_harness::worktree: Creating worktree for forge-1
2026-03-31T00:00:11.345Z  INFO pair_harness::worktree: Worktree created at /home/christian/.agentflow/workspaces/your-username-test-calculator/worktrees/forge-1
2026-03-31T00:00:11.456Z  INFO pair_harness::worktree: Checked out new branch: forge-1/T-001
```

**What's happening:**
1. FORGE receives work assignment
2. Creates an isolated Git worktree for this task
3. Creates a new branch named after the worker and ticket

### Step 3: FORGE Spawns Claude Code

**You'll see:**

```
2026-03-31T00:00:12.567Z  INFO agent_forge: Spawning Claude Code for worker forge-1
2026-03-31T00:00:12.678Z  INFO pair_harness::process: Running: claude run --persona /path/to/orchestration/agent/agents/forge.agent.md
2026-03-31T00:00:13.789Z  INFO agent_forge: Claude Code process started (PID: 12345)
2026-03-31T00:00:13.890Z  INFO agent_forge: Worker forge-1 is now working on T-001
```

**What's happening:**
1. Spawns Claude Code CLI with FORGE persona
2. Provides the issue context
3. Claude Code starts autonomous development

**⏰ This step takes 5-15 minutes** depending on task complexity.

### Step 4: Claude Code Works

While Claude Code is working, you can monitor its progress:

```bash
# Watch the worker log in real-time
tail -f ~/.agentflow/workspaces/your-username-test-calculator/forge/workers/forge-1/worker.log
```

**Example log snippets:**

```
[Claude Code] Reading issue #1: Implement calculator core logic
[Claude Code] Planning implementation...
[Claude Code] Creating index.html with calculator UI
[Claude Code] Writing calculator.js with operation logic
[Claude Code] Adding styles.css for modern design
[Claude Code] Running tests...
[Claude Code] All tests passed
[Claude Code] Committing changes...
[Claude Code] Creating STATUS.json...
```

### Step 5: Work Completion

**You'll see:**

```
2026-03-31T00:15:45.123Z  INFO agent_forge: Worker forge-1 completed work on T-001
2026-03-31T00:15:45.234Z  INFO agent_forge: STATUS.json found at /home/christian/.agentflow/workspaces/your-username-test-calculator/worktrees/forge-1/STATUS.json
2026-03-31T00:15:45.345Z  INFO agent_forge: Work result: success, PR: https://github.com/your-username/test-calculator/pull/1
```

**Output format:**
```json
{
  "action": "pr_opened",
  "worker": "forge-1",
  "ticket": "T-001",
  "pr_url": "https://github.com/your-username/test-calculator/pull/1",
  "status": "complete"
}
```

### Step 6: NEXUS Assigns More Work

**You'll see:**

```
2026-03-31T00:15:46.456Z  INFO agent_nexus: Worker forge-1 marked as available
2026-03-31T00:15:46.567Z  INFO agent_nexus: Assigning issue #2 "Add scientific calculator features" to forge-2
```

The cycle repeats for each issue!

### Step 7: All Work Complete

**You'll see:**

```
2026-03-31T00:30:12.123Z  INFO agent_nexus: No more open issues
2026-03-31T00:30:12.234Z  INFO agent_nexus: All workers idle
2026-03-31T00:30:12.345Z  INFO real_test: Orchestration flow halted with action: no_work
```

---

## Inspecting Generated Files

### 1. Understanding the File Structure

AgentFlow uses a specific directory structure for work completion:

```bash
~/.agentflow/workspaces/your-username-test-calculator/
├── main/                    # Main repository clone
├── worktrees/              # Agent work areas
│   └── forge-1/            # Files created by agent
│       ├── index.html      # Calculator UI
│       ├── calculator.js   # Core logic
│       ├── styles.css      # Styling
│       └── README.md       # Documentation
└── forge/
    └── workers/
        └── forge-1/
            ├── worker.log          # Detailed logs
            └── T-005/             # Ticket-scoped artifacts
                └── shared/             # Status and evaluation files
                    ├── PLAN.md         # Initial implementation plan
                    ├── WORKLOG.md      # Progress tracking
                    ├── CONTRACT.md     # SENTINEL-approved contract (if enabled)
                    ├── segment-N-eval.md # SENTINEL segment reviews (if enabled)
                    ├── final-review.md # SENTINEL final review (if enabled)
                    └── STATUS.json     # Final work status
```

### 2. Check the Code Files

```bash
# View the generated code
cd ~/.agentflow/workspaces/your-username-test-calculator/worktrees/forge-1

# List all files
ls -la

# View specific files
cat index.html
cat calculator.js
cat styles.css
```

### 3. View STATUS.json (Work Completion)

```bash
# STATUS.json is in the shared directory
cat ~/.agentflow/workspaces/your-username-test-calculator/forge/workers/forge-1/shared/STATUS.json
```

**Example content:**

```json
{
  "pair": "forge-1",
  "ticket_id": "T-001",
  "status": "PR_OPENED",
  "pr_url": "https://github.com/your-username/test-calculator/pull/1",
  "pr_number": 1,
  "files_changed": [
    "index.html",
    "calculator.js",
    "styles.css",
    "README.md"
  ],
  "commits": [
    "abc1234 Create calculator UI structure",
    "def5678 Implement calculator logic",
    "ghi9012 Add modern styling",
    "jkl3456 Add README documentation"
  ],
  "summary": "Implemented basic calculator with HTML/CSS/JavaScript. Supports add, subtract, multiply, divide. Modern glassmorphism design."
}
```

### 4. View SENTINEL Evaluation Files (if SENTINEL is enabled)

**Note**: In the current simplified configuration, SENTINEL may not be active. When enabled, you'll see these files:

```bash
cd ~/.agentflow/workspaces/your-username-test-calculator/forge/workers/forge-1/shared

# View the implementation plan
cat PLAN.md

# View SENTINEL's contract approval (if SENTINEL reviewed the plan)
cat CONTRACT.md
```

**Example CONTRACT.md (when SENTINEL is enabled):**

```markdown
# Contract for T-001: Implement calculator core logic

status: AGREED

## Acceptance Criteria

1. ✅ Basic operations: add, subtract, multiply, divide
2. ✅ Clean, modern UI
3. ✅ Responsive design
4. ✅ Error handling for division by zero
5. ✅ Clear documentation in README

## Definition of Done

- All operations working correctly
- UI passes visual inspection
- No console errors
- README includes usage instructions
```

**Example segment-1-eval.md (when SENTINEL reviews segments):**

```markdown
# Segment 1 Evaluation

verdict: APPROVED

## Correctness
✅ All calculator operations implemented correctly
✅ Division by zero handled properly

## Test Coverage
✅ Manual testing shows all operations work

## Standards Compliance
✅ Clean HTML structure
✅ Proper CSS organization
✅ JavaScript follows modern practices

## Code Quality
✅ Well-organized code
✅ Good variable naming
✅ Comments where needed

## No Regressions
✅ No existing functionality affected (new project)
```

### 3. Check Git History

```bash
cd ~/.agentflow/workspaces/your-username-test-calculator/worktrees/forge-1

# View commits
git log --oneline -5

# Check git status
git status

# View changes
git diff origin/main
```

### 4. Test the App Locally

```bash
cd ~/.agentflow/workspaces/your-username-test-calculator/worktrees/forge-1

# For HTML/CSS/JS projects
python3 -m http.server 8000
# Open http://localhost:8000 in your browser

# For Node.js projects (if package.json exists)
npm install
npm run dev

# For React/Vite projects
npm install
npm run dev
```

### 5. Review the Pull Request

```bash
# List all PRs
gh pr list --repo your-username/test-calculator

# View PR details
gh pr view 1 --repo your-username/test-calculator

# Review the code changes
gh pr diff 1 --repo your-username/test-calculator

# Merge the PR (when ready)
gh pr merge 1 --repo your-username/test-calculator --squash
```

---

## Troubleshooting

### Issue: "GITHUB_PERSONAL_ACCESS_TOKEN must be set"

**Cause:** Missing or incorrectly named environment variable.

**Fix:**
```bash
# Check if .env file exists
ls -la .env

# Verify the variable is set
cat .env | grep GITHUB_PERSONAL_ACCESS_TOKEN

# Ensure no extra spaces
# Wrong: GITHUB_PERSONAL_ACCESS_TOKEN = ghp_xxx
# Right: GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx
```

### Issue: "No issues found"

**Cause:** Repository has no open issues or `GITHUB_REPOSITORY` is incorrect.

**Fix:**
```bash
# Verify repository format (must be: owner/repo)
echo $GITHUB_REPOSITORY

# Check issues exist
gh issue list --repo your-username/test-calculator

# Create an issue manually
gh issue create --repo your-username/test-calculator --title "Test Issue" --body "Test description"
```

### Issue: "Claude Code CLI not found"

**Cause:** Claude Code CLI is not installed or not in PATH.

**Fix:**
```bash
# Check if installed
which claude

# If not found, download from:
# https://www.anthropic.com/claude-code

# After installation, verify
claude --version
```

### Issue: "Worker timed out"

**Cause:** Task is too complex or Claude Code encountered an error.

**Check the logs:**
```bash
tail -100 ~/.agentflow/workspaces/your-username-test-calculator/forge/workers/forge-1/worker.log
```

**Common causes:**
- API rate limits
- Complex task requiring longer timeout
- Missing dependencies in target repository

**Fix:**
```rust
// In crates/agent-forge/src/lib.rs
// Increase timeout from default (30 min) to 60 min
const WORK_TIMEOUT: Duration = Duration::from_secs(3600);
```

### Issue: "Permission denied" when creating worktree

**Cause:** File permissions or disk space.

**Fix:**
```bash
# Check disk space
df -h ~/.agentflow

# Check permissions
ls -la ~/.agentflow/workspaces/

# Fix permissions
chmod -R u+w ~/.agentflow/workspaces/
```

### Issue: "FORGE exited quickly without progress" or "Failed to authenticate. API Error: 403"

**Cause:** Claude CLI can't authenticate through your gateway because it only supports OpenAI format, not the Anthropic Messages API.

**Fix:** Start the local Anthropic-to-OpenAI proxy before running the orchestration:
```bash
# Terminal 1
./scripts/start_proxy.sh

# Terminal 2
cargo run --bin real_test
```

Ensure `.env` has `GATEWAY_URL` and `GATEWAY_API_KEY` set. See the [OpenAI-only gateways](#if-your-gateway-only-supports-openai-format) section above.

### Issue: "GitHub MCP server fails to start"

**Cause:** Missing Node.js or incorrect GitHub token permissions.

**Fix:**
```bash
# Check Node.js
node --version

# Test GitHub token manually
curl -H "Authorization: token $GITHUB_PERSONAL_ACCESS_TOKEN" \
  https://api.github.com/user

# Ensure token has correct scopes
# Go to: https://github.com/settings/tokens
# Token needs: repo, workflow, write:packages
```

---

## Directory Structure Reference

```
AgentFlow/                                    # Orchestrator project
├── .env                                      # Your API keys (DO NOT COMMIT)
├── orchestration/agent/
│   ├── agents/
│   │   ├── nexus.agent.md                   # Orchestrator persona
│   │   └── forge.agent.md                   # Builder persona
│   └── registry.json                         # Worker slot definitions
├── binary/src/bin/
│   └── real_test.rs                          # Main entry point
└── crates/                                   # Implementation crates

~/.agentflow/                                 # AgentFlow runtime directory
└── workspaces/
    └── your-username-test-calculator/        # Target project workspace
        ├── main/                             # Main repository clone
        │   ├── .git/
        │   └── README.md
        ├── worktrees/                        # Agent work areas (CODE FILES)
        │   ├── forge-1/                      # Worker #1 isolated workspace
        │   │   ├── index.html                # Generated code files
        │   │   ├── calculator.js
        │   │   ├── styles.css
        │   │   └── README.md
        │   └── forge-2/                      # Worker #2 isolated workspace
        └── forge/                            # Worker management directory
            └── workers/
                ├── forge-1/
                │   ├── worker.log            # Detailed Claude Code logs
                │   └── T-005/                 # Ticket-scoped artifacts
                │       └── shared/               # Status & evaluation files
                │           ├── PLAN.md           # Implementation plan
                │           ├── WORKLOG.md        # Progress tracking
                │           ├── CONTRACT.md       # SENTINEL approval (if enabled)
                │           ├── segment-N-eval.md # SENTINEL reviews (if enabled)
                │           ├── final-review.md   # Final review (if enabled)
                │       └── STATUS.json       # ⭐ Work completion status
                └── forge-2/
                    ├── worker.log
                    └── shared/
                        └── STATUS.json
```

---

## Next Steps

1. **Customize Agent Personas**: Edit [`orchestration/agent/agents/forge.agent.md`](orchestration/agent/agents/forge.agent.md:1) to change how the builder agent works
2. **Add More Workers**: Edit [`orchestration/agent/registry.json`](orchestration/agent/registry.json:1) to add more parallel workers
3. **Integrate SENTINEL**: Enable code review by uncommenting SENTINEL node in flow
4. **Production Deployment**: Use `cargo build --release` and deploy with systemd or Docker

---

## Video Walkthrough

🎥 **Want to see it in action?** Watch our video tutorial: [TODO: Add video link]

---

## Additional Resources

- [DEMO.md](DEMO.md) - Quick demo guide
- [CONTRIBUTING.md](CONTRIBUTING.md) - Development guidelines  
- [docs/forge-sentinel-arch.md](docs/forge-sentinel-arch.md) - Architecture deep dive
- [GitHub Discussions](https://github.com/The-AgenticFlow/AgentFlow/discussions) - Ask questions

---

**Happy Building! 🚀**

*Created by [The-AgenticFlow](https://github.com/The-AgenticFlow)*
