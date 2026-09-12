terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

# TEMPORARY: Host path to the .dev-binaries directory on the Docker host.
# Set via TF_VAR_dev_binary_host_path before running `coder templates push`.
# (Remove when switching to GitHub releases for the openflows binaries.)
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

data "coder_parameter" "tenant" {
  name        = "tenant"
  description  = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
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

    # Setup git credentials from Coder external auth (GitHub App).
    # Declared via data "coder_external_auth" -> this also surfaces the
    # "Login with GitHub" button in the workspace UI, which is how the
    # workspace owner links the GitHub App and grants repo access.
    GITHUB_TOKEN="${data.coder_external_auth.github.access_token}"
    if [ -n "$GITHUB_TOKEN" ]; then
      git config --global credential.helper store
      echo "https://x-access-token:$${GITHUB_TOKEN}@github.com" > /home/coder/.git-credentials
      chmod 600 /home/coder/.git-credentials
      log "Configured git credentials for GitHub push auth"
    else
      log "WARNING: No GitHub token available — open this workspace and click 'Login with GitHub' (external auth) to grant repo access"
    fi

    # git pull or clone (creds via Coder external auth or configured above)
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -n "${data.coder_parameter.repo_url.value}" ]; then
      # Clone into a temp dir first, then move contents
      TEMP_DIR=$(mktemp -d)
      if git clone "${data.coder_parameter.repo_url.value}" "$TEMP_DIR" 2>/dev/null; then
        # Move all files (including .git) to workspace
        sudo chown -R coder:coder /home/coder/workspace
        mv "$TEMP_DIR"/* "$TEMP_DIR"/.* /home/coder/workspace/ 2>/dev/null || true
        rmdir "$TEMP_DIR"
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
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
    log "Heartbeat daemon started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # Start verify executor daemon (task 5 of issue #143: A2A delegated verification)
    # Subscribes to verify tasks from nexus relay, executes them in sandbox, returns results.
    # Uses the same environment as heartbeat (REDIS_URL, OPENFLOWS_TENANT, OPENFLOWS_TICKET, OPENFLOWS_ROLE).
    # This is a long-running process that polls for task assignments via SSE.
    nohup openflows-harness verify serve >/dev/null 2>&1 &
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
  image = "codercom/enterprise-base:ubuntu"

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
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "OPENFLOWS_TICKET=${data.coder_parameter.ticket_id.value}",
    # Base role (forge-1 -> forge): harness Redis keys are namespaced by base role
    "OPENFLOWS_ROLE=${replace(data.coder_parameter.role.value, "/-[0-9]+$/", "")}",
    "A2A_RELAY_ADDR=${var.a2a_relay_addr}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  # egress allowlist: Coder control plane + github.com + Redis only
  # (enforced at network level; Redis is a documented exception per docs/governance.md)

  networks_advanced {
    name = "openflows_default"
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
data "coder_external_auth" "github" {
  id = "primary-github"
}
