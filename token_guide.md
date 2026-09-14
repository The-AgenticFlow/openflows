# Token Acquisition Guide

OpenFlows requires **two tokens** to operate. Both are personal to you and grant OpenFlows the permissions you have.

## 1. GITHUB_TOKEN

**What it's used for:** OpenFlows reads issues from GitHub and opens/manages PRs

**How to get it:**
1. Go to https://github.com/settings/tokens
2. Click **"Generate new token"** → **"Generate new token (classic)"**
3. Under **"Select scopes"**, check ✓ **`repo`** (all options)
   - This grants access to public and private repositories
4. Click **"Generate token"** at the bottom
5. **Copy the token immediately** (you won't see it again)
   - Format: `ghp_xxxxxxxxxxxxxxxxxxxx`
6. Paste in `.env`: `GITHUB_TOKEN=ghp_...`

**Security:** This token is personal to your account. Keep it private. It's in `.gitignore` so it won't be committed to git.

---

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

**Before running the script**, verify you have both tokens:

```bash
echo $GITHUB_TOKEN       # Should print ghp_...
echo $CODER_SESSION_TOKEN # Should print cdr_...
```

Or check `.env`:
```bash
grep "GITHUB_TOKEN\|CODER_SESSION_TOKEN" .env | grep -v "^#"
```

---

## Troubleshooting

**"CODER_SESSION_TOKEN not found"**
- Make sure you're in the right Coder instance (http://localhost:7080)
- Check you're clicking your username (top-right), not the menu button
- Create a new token (old ones may have expired)

**"GITHUB_TOKEN not working"**
- Token needs `repo` scope
- If you changed scopes, regenerate a new token
- Make sure you copied the full token (it's long)

**"Permission denied" errors in logs**
- GitHub token: Check you have access to the repository
- Coder token: Check you're a member of the workspace/organization

---

## File Structure

Once you have both tokens:

```
.env.example  ← Template (safe to commit)
.env          ← Your tokens (keep private, .gitignore'd)
```

**Never commit `.env`** — it contains your personal tokens.

---

## Next Steps

Once you have both tokens in `.env`:

```bash
./scripts/start.sh --reset
```

Done! OpenFlows is running. Create a GitHub issue and watch it work.
