terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

# TEMPORARY: Host path to the .dev-binaries directory on the Docker host.
# Set via TF_VAR_dev_binary_host_path before running `coder templates push`.
# (Remove when switching to GitHub releases for the openflows binaries.)
variable "docker_network" {
  description = "Docker network shared by the control plane and tenant workspaces"
  type        = string
  default     = "openflows_default"
}

variable "workspace_image" {
  description = "Workspace image containing the tools used by the production bootstrap"
  type        = string
  default     = "codercom/enterprise-base:ubuntu"
}

variable "github_api_base" {
  description = "GitHub REST API origin, including any enterprise API path"
  type        = string
  default     = "https://api.github.com"
}

variable "github_git_base" {
  description = "Git transport origin used by the controller to provision repository checkouts"
  type        = string
  default     = "https://github.com"
}

variable "dev_binary_host_path" {
  description = "Absolute host path to the .dev-binaries directory"
  type        = string
  default     = ""
}

variable "harness_version" {
  type        = string
  default     = "harness-edge"
  description = "openflows-harness binary version to download. Use 'harness-edge' for the latest main-branch build, or a specific version tag (e.g. 'v1.2.0')."
}

variable "a2a_relay_addr" {
  type        = string
  default     = "openflows-nexus:3000"
  description = "Address of the nexus A2A relay (JSON-RPC verify transport, issue #143). Forge resolves the nexus container over the shared docker network by its service name to claim and execute verify tasks."
}

# Workspace-level parameters (set per-workspace via Coder API rich_parameter_values)
data "coder_parameter" "role" {
  name        = "role"
  description  = "Agent role name"
  default     = "forge"
  type        = "string"
}

data "coder_parameter" "ticket_id" {
  name        = "ticket_id"
  description  = "Ticket identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "redis_url" {
  name        = "redis_url"
  description  = "Redis SharedStore URL"
  default     = "redis://redis:6379"
  type        = "string"
}

data "coder_parameter" "repo_url" {
  name        = "repo_url"
  description  = "Git repository URL to clone into the workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "branch" {
  name        = "branch"
  description  = "Remote branch to check out (the PR branch for an existing PR). Empty = create or resume the worker/ticket branch."
  default     = ""
  type        = "string"
}

# The requested branch is interpolated directly into the startup bash script.
# A branch name containing shell metacharacters (e.g. `$(...)`) would otherwise
# be evaluated by bash as command substitution, so validate it against a charset
# that accepts all git-valid, shell-safe characters and fall back to the default
# branch when it does not match. `+`, `=`, `@`, `.`, `_`, `-`, `/` are all legal
# in git ref names and safe in the shell; anything else (dollar, backtick,
# parens, quotes, spaces, `;`, `&`, `|`, `<`, `>`, `!`, `*`, `?`, `[`, `]`,
# `~`, `\`, `#`) is rejected so a malicious branch cannot inject shell commands.
locals {
  requested_branch = data.coder_parameter.branch.value
  safe_branch      = can(regex("^[A-Za-z0-9._/+@=-]+$", local.requested_branch)) ? local.requested_branch : ""
}

data "coder_parameter" "tenant" {
  name        = "tenant"
  description  = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "a2a_pair_token" {
  name        = "a2a_pair_token"
  description = "Pair-scoped token used by Sentinel/FORGE to authenticate A2A verification RPCs"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "coder_url" {
  name        = "coder_url"
  description  = "Coder server URL for API calls"
  default     = ""
  type        = "string"
}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" >&2; }

    # TEMPORARY: Use mounted dev binaries for local testing
    # (In production, download from GitHub releases instead)
    HARNESS_BIN="/usr/local/bin/openflows-harness"
    if [ -f /opt/openflows-dev/openflows-harness ]; then
      log "Using mounted dev harness binary..."
      sudo cp /opt/openflows-dev/openflows-harness "$HARNESS_BIN"
      sudo chmod +x "$HARNESS_BIN"
    else
      # Fallback: download from GitHub releases, with retries. The harness is
      # REQUIRED — without it the agent cannot coordinate (dispatch/status/
      # heartbeat), so a missing harness must fail the startup script loudly
      # instead of leaving a silently uncoordinated workspace.
      HARNESS_ASSET="openflows-harness-x86_64-unknown-linux-musl.tar.gz"
      if [ "${var.harness_version}" = "harness-edge" ]; then
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/harness-edge/$${HARNESS_ASSET}"
        log "Downloading openflows-harness (harness-edge/latest build)..."
      else
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/${var.harness_version}/$${HARNESS_ASSET}"
        log "Downloading openflows-harness v${var.harness_version}..."
      fi
      for attempt in 1 2 3; do
        if curl -fsSL --retry 3 "$HARNESS_URL" -o /tmp/openflows-harness.tar.gz; then
          tar -xzf /tmp/openflows-harness.tar.gz -C /tmp/ || { log "FATAL: failed to extract harness tarball"; exit 1; }
          HARNESS_DIR=$(find /tmp/ -maxdepth 1 -type d -name "openflows-harness-*" 2>/dev/null | head -1)
          if [ -n "$HARNESS_DIR" ] && [ -f "$HARNESS_DIR/openflows-harness" ]; then
            sudo mv "$HARNESS_DIR/openflows-harness" "$HARNESS_BIN"
            sudo chmod +x "$HARNESS_BIN"
            rm -rf /tmp/openflows-harness.tar.gz "$HARNESS_DIR"
          else
            log "FATAL: could not find harness binary in extracted tarball"; exit 1
          fi
          break
        fi
        log "Harness download attempt $attempt failed; retrying in 5s..."
        sleep 5
      done
    fi
    if [ ! -x "$HARNESS_BIN" ]; then
      log "FATAL: openflows-harness is not installed — agent cannot coordinate; failing startup"
      exit 1
    fi

    # Provision the OpenFlows hook harness for the agent CLI so the agent
    # loop is controllable end-to-end: session start reads the dispatch,
    # tool-use guards enforce policy, and Stop refuses to end the session
    # until the flow artifacts (status/handoff/PR) exist.
    ROLE="${data.coder_parameter.role.value}"
    ROLE_BASE="$${ROLE%-*}"   # forge-1 -> forge
    HOOKS_SRC="/home/coder/.openflows/artifacts/plugin/hooks/$ROLE_BASE"
    HOOKS_DIR="/home/coder/.openflows/hooks"
    if [ -d "$HOOKS_SRC" ]; then
      mkdir -p "$HOOKS_DIR"
      cp -r "$HOOKS_SRC/." "$HOOKS_DIR/"
      chmod +x "$HOOKS_DIR"/*.sh 2>/dev/null || true
      log "Installed $ROLE_BASE hooks from artifacts volume"
    else
      log "WARNING: no hooks found for role $ROLE_BASE at $HOOKS_SRC"
    fi

    # Wire hooks into the Claude Code agent loop (settings.json). Only events
    # whose scripts exist are registered, so this works for every role.
    mkdir -p /home/coder/.claude
    python3 - "$HOOKS_DIR" /home/coder/.claude/settings.json <<'PYEOF'
    import json, os, sys
    hooks_dir, settings_path = sys.argv[1], sys.argv[2]
    def cmd(name):
        path = os.path.join(hooks_dir, name)
        return path if os.path.isfile(path) else None
    event_map = {
        "SessionStart": [(None, "session_start.sh")],
        "PreToolUse": [("Bash", "pre_bash_guard.sh"),
                        ("Bash", "pre_bash_readonly_guard.sh"),
                        ("Write|Edit|MultiEdit", "pre_write_check.sh")],
        "PostToolUse": [("Write|Edit|MultiEdit", "post_write_lint.sh"),
                         ("Write|Edit|MultiEdit", "post_write_validate.sh")],
        "PreCompact": [(None, "pre_compact_handoff.sh")],
        "Stop": [(None, "stop_require_artifact.sh"),
                  (None, "stop_require_eval.sh")],
        "SubagentStop": [(None, "subagent_stop.sh")],
    }
    hooks = {}
    for event, entries in event_map.items():
        matchers = []
        for matcher, script in entries:
            path = cmd(script)
            if not path:
                continue
            entry = {"hooks": [{"type": "command", "command": path}]}
            if matcher:
                entry["matcher"] = matcher
            matchers.append(entry)
        if matchers:
            hooks[event] = matchers
    settings = {}
    if os.path.exists(settings_path):
        try:
            settings = json.load(open(settings_path))
        except Exception:
            settings = {}
    settings["hooks"] = hooks
    json.dump(settings, open(settings_path, "w"), indent=2)
    print(f"wrote {settings_path} with {len(hooks)} hook events", file=sys.stderr)
    PYEOF

    # Inherit the controller-spawner's GitHub identity from Coder external
    # auth (the tenant's linked GitHub App token). The workspace owner IS the
    # controller-spawner (see create_workspace_for_user in coder-client), so
    # this token resolves to the real account the workspace should commit and
    # open PRs as. This is a REQUIRED, verified identity: startup fails loudly
    # if it cannot be established — never a silent REST-API fallback.
    GIT_TOKEN="${data.coder_external_auth.github.access_token}"
    if [ -z "$GIT_TOKEN" ]; then
      log "FATAL: no inherited GitHub credential for the controller-spawner (workspace owner external auth is not linked)"
      exit 1
    fi

    # System-level helper so git works for BOTH the agent user (pushes) and
    # the sudo'd root clone below (their $HOME differ).
    sudo git config --system credential.helper store
    echo "https://x-access-token:$${GIT_TOKEN}@github.com" > /home/coder/.git-credentials
    chmod 600 /home/coder/.git-credentials
    sudo mkdir -p /root
    sudo cp /home/coder/.git-credentials /root/.git-credentials
    sudo chmod 600 /root/.git-credentials
    log "Configured git credentials for GitHub push auth"

    # Install the GitHub CLI so the agent can open PRs with `gh pr create`
    # instead of hand-rolling the REST API. gh is best-effort, NOT a hard
    # startup dependency: the git credential helper above is the real push
    # mechanism, so a workspace with a valid repo seed still starts even if gh
    # cannot be installed (the agent can open the PR via REST/UI instead).
    if ! command -v gh >/dev/null 2>&1; then
      # Ubuntu base image (codercom/enterprise-base): prefer the official
      # GitHub apt repo (always current), fall back to the distro package.
      sudo mkdir -p -m 0755 /etc/apt/keyrings 2>/dev/null || true
      sudo curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        -o /etc/apt/keyrings/githubcli-archive-keyring.gpg 2>/dev/null || true
      if [ -s /etc/apt/keyrings/githubcli-archive-keyring.gpg ]; then
        echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
          | sudo tee /etc/apt/sources.list.d/github-cli.list >/dev/null
      fi
      sudo apt-get update -qq >/dev/null 2>&1 || true
      sudo apt-get install -y -qq gh >/dev/null 2>&1 || true
    fi

    if command -v gh >/dev/null 2>&1; then
      # Authenticate gh persistently as the inherited identity for BOTH the
      # `coder` user and `root` (the sudo'd clone below uses root's $HOME).
      # --hostname + --git-protocol make the stored credential explicit and
      # deterministic (non-interactive). The token travels through stdin for
      # the root login too — never embedded in a command-line argument that
      # another process could read. Failures are fatal, never swallowed.
      printf '%s' "$GIT_TOKEN" | gh auth login --hostname github.com --git-protocol https --with-token \
        || { log "FATAL: gh auth login failed for user coder"; exit 1; }
      printf '%s' "$GIT_TOKEN" | sudo -H bash -c 'IFS= read -r tok || [ -n "$tok" ] || exit 1; printf "%s" "$tok" | gh auth login --hostname github.com --git-protocol https --with-token' \
        || { log "FATAL: gh auth login failed for user root"; exit 1; }
    else
      log "WARNING: GitHub CLI (gh) unavailable — agent should push via git and open the PR through the REST API or GitHub UI"
    fi

    # Resolve the expected account from the inherited token via the GitHub API
    # (the coder_external_auth data source does not expose the login). Honor
    # GITHUB_API_BASE so self-hosted setups match the rest of the system.
    EXPECTED_LOGIN=$(curl -fsSL -H "Authorization: token $GIT_TOKEN" "$${GITHUB_API_BASE:-https://api.github.com}/user" \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["login"])') \
      || { log "FATAL: could not resolve the inherited GitHub identity from the external-auth token"; exit 1; }
    if [ -z "$EXPECTED_LOGIN" ]; then
      log "FATAL: resolved an empty inherited GitHub identity from the external-auth token"
      exit 1
    fi

    if command -v gh >/dev/null 2>&1; then
      # Verify the authed gh identity actually matches the inherited account, so
      # gh/git act as the controller-spawner — never a dummy or mismatched login.
      AUTHED_LOGIN=$(gh api user --jq .login) \
        || { log "FATAL: could not query the authed gh identity"; exit 1; }
      if [ "$AUTHED_LOGIN" != "$EXPECTED_LOGIN" ]; then
        log "FATAL: gh authed as '$AUTHED_LOGIN' but the inherited token resolves to '$EXPECTED_LOGIN' — identity mismatch"
        exit 1
      fi
      log "Inherited GitHub identity verified: $EXPECTED_LOGIN"
    else
      log "WARNING: gh unavailable — skipping gh identity check; git push uses the inherited token via the credential helper"
    fi

    # Set git author identity from the inherited account (the template sets
    # none today, so commits would fail or use a wrong default identity).
    # GitHub's noreply email keeps the address private and avoids the
    # "private email" bot. Apply for coder and root (both push/clone).
    git config --global user.name "$EXPECTED_LOGIN"
    git config --global user.email "$EXPECTED_LOGIN@users.noreply.github.com"
    sudo -H bash -c "git config --global user.name '$EXPECTED_LOGIN'; git config --global user.email '$EXPECTED_LOGIN@users.noreply.github.com'"

    # Record the resolved identity (non-secret) so FORGE knows which account it
    # is committing/pushing as. Stored in $HOME, NOT the clone destination
    # (/home/coder/workspace), so it never makes the destination non-empty and
    # blocks `git clone`/`cp -a` below. Never write the token anywhere.
    echo "$EXPECTED_LOGIN" > /home/coder/.openflows-gh-identity 2>/dev/null || true

    # Acquire the repository. Prefer the golden offline seed from the shared
    # artifacts volume (see docs §3.2): copy -> refresh -> checkout -> work.
    # Fall back to a direct network clone when no golden seed exists. Either
    # path FAILS LOUDLY (with a WARNING) instead of silently leaving an empty
    # workspace, because an agent with no repo cannot do useful work and the
    # old silent `2>/dev/null` clone was the root cause of empty workspaces.
    GOLDEN_REPO="/home/coder/.openflows/artifacts/repo"
    # The target branch: when the controller passes a PR `branch` (rework of an
    # existing PR), check that out so already-done work is NOT redone. When
    # empty, create or resume the worker/ticket branch (fresh work).
    TARGET_BRANCH="${local.safe_branch}"
    TICKET_ID="${data.coder_parameter.ticket_id.value}"
    # The fresh workspace volume is root-owned initially, so any copy/clone
    # into /home/coder/workspace must run via sudo (then be chowned back).
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
      git fetch --all --prune 2>/dev/null || true
    elif [ -d "$GOLDEN_REPO/.git" ]; then
      sudo cp -a "$GOLDEN_REPO/." /home/coder/workspace/ 2>/tmp/forge_golden_err.log
      sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
      log "Copied golden repo seed into /home/coder/workspace"
      # Refresh to latest before checkout (design: copy -> refresh -> checkout).
      cd /home/coder/workspace
      git fetch --all --prune 2>/dev/null || true
    elif [ -n "${data.coder_parameter.repo_url.value}" ]; then
      log "WARNING: no golden repo seed at $GOLDEN_REPO — falling back to direct clone"
      if sudo git clone "${data.coder_parameter.repo_url.value}" /home/coder/workspace 2>/tmp/forge_clone_err.log; then
        sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
        log "Cloned repository into /home/coder/workspace"
        cd /home/coder/workspace
        git fetch --all --prune 2>/dev/null || true
      else
        log "WARNING: git clone failed — $(tail -5 /tmp/forge_clone_err.log 2>/dev/null)"
      fi
    else
      log "WARNING: no repo_url and no golden repo seed — workspace has no repository"
    fi

    # When we cannot get onto the intended PR branch, record it so the rework
    # flow does not treat the current branch as the PR branch and push the fix
    # to the wrong remote branch. The agent stays available, but the mismatch is
    # made visible via a workspace-root marker plus a prominent startup log.
    warn_branch_mismatch() {
      local intended="$1"
      local current
      current="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
      log "WARNING: workspace is on branch '$current', NOT the intended PR branch '$intended' — rework must NOT push to the current branch"
      {
        echo "# REWORK BRANCH MISMATCH"
        echo ""
        echo "The startup script could not switch to the intended PR branch \`$intended\`."
        echo "The workspace is currently on branch \`$current\`, which is NOT the PR branch."
        echo ""
        echo "**Do NOT run \`git push\` on the current branch** — it is not the PR branch and"
        echo "pushing here would update the wrong branch, not the PR."
        echo "Resolve the local conflict or uncommitted changes and switch to \`$intended\`"
        echo "before performing any push."
      # Best-effort marker: if the workspace is not writable by `coder` the write
      # may fail. It must never abort startup via `set -e` — the log warning above
      # already carries the message, and FORGE must stay available to resolve the
      # mismatch.
      } > /home/coder/workspace/REWORK_BRANCH_MISMATCH.md || true
    }

    # Checkout the target branch. For rework of an existing PR, this resumes the
    # branch where the work was already done (never redo completed work).
    # Fresh assignments get a worker/ticket branch seeded from the default head.
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace
      if [ -n "$TARGET_BRANCH" ]; then
        # Resume the PR branch on a NAMED local branch (never detached, so the
        # CI-fix / /address_review flow can push with plain `git push`). Prefer
        # an already-existing local branch (reused workspace) to avoid discarding
        # in-flight work, then create a tracking branch from origin. If the PR
        # branch is not present locally or under origin (e.g. a fork-backed PR
        # whose head ref is not on this origin), fall back to the default branch
        # WITHOUT fabricating a branch of the PR's name at origin/HEAD — doing so
        # would start rework without the PR's commits and could reset existing
        # local work.
        if git rev-parse --verify --quiet "refs/heads/$TARGET_BRANCH" >/dev/null; then
          # The local branch exists but the switch can still fail (uncommitted
          # work or an unresolved merge blocks it). That must NOT abort startup
          # via `set -e` — keep the agent available, but flag the mismatch so the
          # rework flow does not push to the wrong branch.
          if git checkout "$TARGET_BRANCH" 2>/dev/null; then
            log "Checked out existing local branch: $TARGET_BRANCH"
          else
            warn_branch_mismatch "$TARGET_BRANCH"
          fi
        elif git checkout -B "$TARGET_BRANCH" --track "origin/$TARGET_BRANCH" 2>/dev/null; then
          log "Checked out target branch: $TARGET_BRANCH"
        else
          git checkout origin/HEAD 2>/dev/null || git checkout main 2>/dev/null || true
          warn_branch_mismatch "$TARGET_BRANCH"
        fi
      else
        # Fresh assignments own a named branch. Reuse local work on restart.
        WORK_BRANCH="$ROLE/$TICKET_ID"
        if [ -z "$TICKET_ID" ] || ! git check-ref-format --branch "$WORK_BRANCH" >/dev/null 2>&1; then
          log "ERROR: cannot derive worker/ticket branch"
          exit 1
        fi
        if git show-ref --verify --quiet "refs/heads/$WORK_BRANCH"; then
          git checkout "$WORK_BRANCH" || { warn_branch_mismatch "$WORK_BRANCH"; exit 1; }
        elif git show-ref --verify --quiet "refs/remotes/origin/$WORK_BRANCH"; then
          git checkout --track -b "$WORK_BRANCH" "origin/$WORK_BRANCH"
        else
          git checkout --no-track -b "$WORK_BRANCH" origin/HEAD || git checkout --no-track -b "$WORK_BRANCH"
        fi
        log "Assignment branch ready: $WORK_BRANCH"
      fi
    fi

    # Start heartbeat daemon (the ONLY Redis client in the workspace).
    # OPENFLOWS_ROLE must be the BASE role (forge), not the worker id
    # (forge-1): the controller writes dispatch and reads heartbeats under
    # the base-role key, so a worker-id role would never match.
    export REDIS_URL="${data.coder_parameter.redis_url.value}"
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export OPENFLOWS_TICKET="${data.coder_parameter.ticket_id.value}"
    export OPENFLOWS_ROLE="$ROLE_BASE"
    export A2A_RELAY_ADDR="${var.a2a_relay_addr}"
    export A2A_PAIR_TOKEN="${data.coder_parameter.a2a_pair_token.value}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
    log "Heartbeat daemon started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # Start verify executor daemon (task 5 of issue #143: A2A delegated verification)
    # Executes tasks in temporary checkouts using this workspace's tools and environment.
    # Uses the same environment as heartbeat (REDIS_URL, OPENFLOWS_TENANT, OPENFLOWS_TICKET, OPENFLOWS_ROLE).
    # Supervise startup failures (including Redis/relay races) outside the agent chat.
    # Keep diagnostics outside the checkout so candidate-head checks stay clean.
    mkdir -p /home/coder/.local/state/openflows
    nohup bash -c 'while true; do openflows-harness verify serve; sleep 5; done' \
      >>/home/coder/.local/state/openflows/verify.log 2>&1 &
    log "Verify executor started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET) — issue #143 task 5"

    # ── Start coding agent ──────────────────────────────────────────────
    # The agent CLI binary (claude, codex, aider, etc.) may be bind-mounted
    # from the host at /opt/openflows-dev/. If found, copy it to PATH and
    # launch it as the work agent. The SessionStart hook fires on launch,
    # providing the task dispatch, current phase, and workflow instructions.
    # If no CLI binary is available, the workspace relies on Coder control-
    # plane agents via the Coder Chat created by the controller.
    AGENT_CLI=""
    if [ -f /opt/openflows-dev/claude ]; then
      AGENT_CLI="claude"
      sudo cp /opt/openflows-dev/claude /usr/local/bin/claude
      sudo chmod +x /usr/local/bin/claude
      log "Mounted Claude Code binary from host — starting agent"
    elif command -v claude >/dev/null 2>&1; then
      AGENT_CLI="claude"
      log "Claude Code found on PATH — starting agent"
    elif [ -f /opt/openflows-dev/codex ]; then
      AGENT_CLI="codex"
      sudo cp /opt/openflows-dev/codex /usr/local/bin/codex
      sudo chmod +x /usr/local/bin/codex
      log "Mounted Codex binary from host — starting agent"
    elif command -v codex >/dev/null 2>&1; then
      AGENT_CLI="codex"
      log "Codex found on PATH — starting agent"
    fi

    if [ -n "$AGENT_CLI" ]; then
      nohup "$AGENT_CLI" -p "Start working on ticket $OPENFLOWS_TICKET. Read dispatch with 'openflows-harness dispatch read' and follow the workflow. Use 'openflows-harness status set <phase>' to report progress." </dev/null >/tmp/agent.log 2>&1 &
      log "Agent started (cli=$AGENT_CLI ticket=$OPENFLOWS_TICKET pid=$!)"
    else
      log "No agent CLI binary found — workspace will rely on Coder Chat control-plane agent"
    fi
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-${data.coder_parameter.role.value}-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-${data.coder_parameter.role.value}-${data.coder_workspace.me.id}"
  image = var.workspace_image
  # Match the Coder agent and dev binaries on Intel and Apple Silicon hosts.
  platform = "linux/amd64"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  # Mount shared artifact files (agent definitions, skills, standards, plans)
  # This volume is created by the Nexus workspace
  volumes {
    container_path = "/home/coder/.openflows/artifacts"
    volume_name    = "openflows-artifacts-${data.coder_parameter.tenant.value}"
    read_only      = true
  }

  # TEMPORARY: Mount dev binaries for local testing (remove when using GitHub releases)
  dynamic "volumes" {
    for_each = var.dev_binary_host_path != "" ? [1] : []
    content {
      container_path = "/opt/openflows-dev"
      host_path      = var.dev_binary_host_path
      read_only      = true
    }
  }

  env = [
    "GITHUB_API_BASE=${var.github_api_base}",
    "GITHUB_GIT_BASE=${var.github_git_base}",
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "OPENFLOWS_TICKET=${data.coder_parameter.ticket_id.value}",
    # Base role (forge-1 -> forge): harness Redis keys are namespaced by base role
    "OPENFLOWS_ROLE=${replace(data.coder_parameter.role.value, "/-[0-9]+$/", "")}",
    "A2A_RELAY_ADDR=${var.a2a_relay_addr}",
    "A2A_PAIR_TOKEN=${data.coder_parameter.a2a_pair_token.value}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  # egress allowlist: Coder control plane + github.com + Redis only
  # (enforced at network level; Redis is a documented exception per docs/governance.md)

  networks_advanced {
    name = var.docker_network
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
data "coder_external_auth" "github" {
  id = "primary-github"
}
