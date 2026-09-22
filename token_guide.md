# Token Acquisition Guide

OpenFlows needs GitHub access for the **controller** (issue/PR sync, CI checks) and **workspace git** (clone/push). This is provided entirely by the **GitHub App external auth** — it is **required**, and it is the *sole* source of the GitHub token. Coder and the workspace templates need it to start and provision. No PAT is required.

## 1. GitHub access: GitHub App external auth (required)

The GitHub App external auth (`CODER_EXTERNAL_AUTH_0_*` in `.env`) must be configured — Coder won't start without it. See [quick_start.md](quick_start.md) Step 1 for how to create and install the App.

Each workspace owner links their GitHub account (during `tenant add`), and the controller/agents use **that linked token** for GitHub API and git ops. That is the only token acquisition step.

## 2. CODER_SESSION_TOKEN

**What it's used for:** OpenFlows provisions workspaces and creates chats for agents to work in

> **IMPORTANT: First-time Coder setup** — When Coder starts for the first time, it will
> prompt you to create your **first admin account**. This is your Coder dashboard login, separate
> from the API token below.

**How to get it:**

> **Easiest way:** Run `./scripts/start.sh` — it starts Coder first (which will prompt you
> to create your admin account), then guides you to get the token.

**Manual steps:**
1. **Start Coder**:
   ```bash
   docker compose up -d
   # Wait ~10 seconds for Coder to start
   ```

2. **Open Coder UI** in your browser at http://localhost:7080

3. **Create your first admin account** (first time only):
   - Coder will prompt you to create your admin user
   - Enter your name, email, and a secure password
   - This is your Coder dashboard login
   - ℹ️ This is separate from the API token below

4. **Sign in** with your new admin account

5. **Create an API token**:
   - Click your **username** in the top-right corner
   - Select **"Account"** from the dropdown
   - Click the **"Tokens"** tab
   - Click **"Create Token"** button
   - Copy the token immediately (format: `cdr_xxxxxxxxxxxx`)

   **Important:** This token is different from your dashboard password. The token grants
   OpenFlows API access to provision workspaces and manage chats.

6. **Paste in `.env`**: `CODER_SESSION_TOKEN=cdr_...`

**What this token grants:** Your personal permissions in Coder. OpenFlows can:
- Create ephemeral workspaces (one per agent per ticket)
- Create chats for agents to work in
- Monitor workspace status
- Read/write workspace metadata

---

## Quick Check

**Before running the script**, verify you have the Coder token and that GitHub external auth is linked:

```bash
echo $CODER_SESSION_TOKEN # Should print cdr_...
```

Or check `.env`:
```bash
grep "CODER_SESSION_TOKEN\|CODER_EXTERNAL_AUTH_0_" .env | grep -v "^#"
```

With external auth configured and each tenant user linked during `tenant add`, no GitHub PAT is required — the controller uses the tenant's linked token.

---

## Troubleshooting

**"CODER_SESSION_TOKEN not found"**
- Make sure you're in the right Coder instance (http://localhost:7080)
- Check you're clicking your username (top-right), not the menu button
- Create a new token (old ones may have expired)

**"Permission denied" errors in logs**
- GitHub: Check the workspace owner has linked the GitHub App and the App is installed on the repo (see quick_start.md Step 4).
- Coder: Check you're a member of the workspace/organization

---

## File Structure

Once you have the tokens (Coder session token; GitHub via linked external auth):

```
.env.example  ← Template (safe to commit)
.env          ← Your secrets (keep private, .gitignore'd)
```

**Never commit `.env`** — it contains your personal tokens.

---

## Next Steps

Once you have `CODER_SESSION_TOKEN` in `.env` and GitHub linked:

```bash
./scripts/start.sh --reset
```

Done! OpenFlows is running. Create a GitHub issue and watch it work.
