# OpenFlows: One-Command Startup

## Recommended: Use `.env` File (Persistent)

### Step 1: Create `.env` file (One-time setup — 5 minutes)

```bash
cp .env.example .env
```

Then edit `.env` and fill in three values:

```bash
# Your GitHub personal access token
# Get from: https://github.com/settings/tokens
GITHUB_TOKEN=ghp_...

# Your GitHub repository
GITHUB_REPOSITORY=owner/repo

# Your Coder session token
# Get from: http://localhost:7080 → Account → Tokens
CODER_SESSION_TOKEN=cdr_...
```

### Step 2: Start OpenFlows (Reusable — 30 seconds)

```bash
./scripts/start.sh --reset
```

Done! The script automatically loads variables from `.env`.

---

## Alternative: Export Variables (Temporary)

If you prefer not to use `.env`, you can export directly:

```bash
export GITHUB_TOKEN="ghp_..."
export GITHUB_REPOSITORY="owner/repo"
export CODER_SESSION_TOKEN="cdr_..."

./scripts/start.sh --reset
```

---

## How to Get Each Token

For detailed token acquisition guide, see [`TOKEN_GUIDE.md`](TOKEN_GUIDE.md).

### GITHUB_TOKEN
1. Go to https://github.com/settings/tokens
2. Click "Generate new token" → "Generate new token (classic)"
3. Select scope: ☑️ `repo` (all options under repo)
4. Click "Generate token"
5. **Copy the token immediately** (you won't see it again)
6. Paste in `.env` as `GITHUB_TOKEN=ghp_...`

### CODER_SESSION_TOKEN
**What it's for:** OpenFlows uses this to authenticate with Coder and perform operations on your behalf:
  - Provision new agent workspaces
  - Create chats for agents to work in
  - Monitor workspace status and health
  - Manage workspace lifecycle

**Where to get it:**

> **⚠️ First time setup:** When Coder starts for the first time, it will prompt you to create
> your **first admin account** (your Coder dashboard login). This is separate from the API token.

**Step-by-step:**
1. **Run the startup script** — starts Coder first:
   ```bash
   ./scripts/start.sh --reset
   ```
2. **Create your admin account** when Coder prompts (first time only):
   - This is your Coder dashboard login (email + password)
3. **Open Coder UI**: http://localhost:7080
4. **Sign in** with your admin account
5. **Get API token**:
   - Click your **username** in the top-right corner
   - Select **Account** from the dropdown menu
   - Click **Tokens** tab
   - Click **Create Token** button
   - Copy the token (format: `cdr_xxxxxxxxxxxx`)
6. **Add to `.env`**:
   ```bash
   echo 'CODER_SESSION_TOKEN=cdr_...' >> .env
   ```
7. **Run again**:
   ```bash
   ./scripts/start.sh --reset
   ```

**Note:** Two separate things are created: (1) admin account = dashboard login, (2) API token = OpenFlows access. Keep both private.

### GITHUB_REPOSITORY
Simply your GitHub repo path (e.g., `my-org/my-repo`):
```bash
GITHUB_REPOSITORY=my-org/my-repo
```

---

## What Happens After You Run `./scripts/start.sh --reset`

The script automatically:
1. ✅ Loads variables from `.env`
2. ✅ Checks your tokens
3. ✅ Resets Redis to clean state (removes 60+ zombie keys)
4. ✅ Starts Docker containers (Coder, Redis, etc.)
5. ✅ Builds OpenFlows binaries
6. ✅ Starts the controller daemon
7. ✅ Outputs: "✅ OpenFlows Ready to Work"

Then:
- **Create a GitHub issue** in your repo (or via API)
- **Monitor progress**: `tail -f /tmp/openflows-controller.log`
- Watch: issue detected → assigned → workspace provisioned → work starts

---

## Common Commands

| Task | Command |
|------|---------|
| **Start (fresh)** | `./scripts/start.sh --reset` |
| **Start (keep state)** | `./scripts/start.sh` |
| **View logs** | `tail -f /tmp/openflows-controller.log` |
| **Check workers** | `docker exec openflows-redis-1 redis-cli GET worker_slots \| jq .` |
| **Check tickets** | `docker exec openflows-redis-1 redis-cli GET tickets \| jq .` |
| **Reset Redis only** | `./scripts/reset-controller-state.sh --confirm` |
| **Show help** | `./scripts/start.sh --help` |

---

## Troubleshooting

### "Missing required environment variables"
Make sure `.env` is in the project root:
```bash
cp .env.example .env
# Edit .env with your tokens
```

Or export them:
```bash
export GITHUB_TOKEN="ghp_..."
export GITHUB_REPOSITORY="owner/repo"
export CODER_SESSION_TOKEN="cdr_..."
```

### "Redis container not responding"
```bash
docker ps | grep redis
docker compose up -d   # Restart if needed
```

### "Failed to start Docker containers"
```bash
# Ensure Docker is running
docker ps

# Check docker-compose.yml exists
ls docker-compose.yml
```

### "Controller failed to start"
```bash
# Check logs for errors
tail -50 /tmp/openflows-controller.log

# Check port 7080 isn't already in use
lsof -i :7080
```

### "Workspace not provisioning"
```bash
# Check controller logs for provision errors
tail -f /tmp/openflows-controller.log | grep -i provision

# Verify Coder is accessible
curl http://localhost:7080/api/v2/buildinfo
```

---

## Workflow Example

```bash
# 1. Setup (first time only)
cp .env.example .env
# Edit .env with tokens

# 2. Start
./scripts/start.sh --reset
# Output: ✅ OpenFlows Ready to Work

# 3. Create an issue (in GitHub web UI or via CLI)
# GitHub issue is created

# 4. Watch the logs
tail -f /tmp/openflows-controller.log

# Expected output:
# - "sync_issues: found new issue T-001"
# - "Nexus: dispatching assignable ticket to an idle forge worker"
# - "Provisioning Coder workspace for worker"
# - "Coder workspace provisioned"
# - "Created Chat for ticket assignment"
# - Later: "flow recovery: inconsistencies detected"

# 5. Monitor workspace startup
# In Coder UI (localhost:7080), you'll see a chat session start
# The agent begins working on the issue

# 6. Agent completes work
# Opens PR → Sentinel reviews → PR merged → Ticket done
```

---

## For More Details

- **Full Documentation**: See [`README.md`](README.md)
- **Testing & Debugging**: See [`TESTING_QUICK_START.md`](TESTING_QUICK_START.md)
- **Technical Deep-Dive**: See [`FIXES_SUMMARY.md`](FIXES_SUMMARY.md)
