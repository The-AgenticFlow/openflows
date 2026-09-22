terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

variable "role" {
  type        = string
  default     = "lore"
  description = "Agent role name"
}

variable "ticket_id" {
  type        = string
  default     = ""
  description = "Ticket identifier"
}

variable "redis_url" {
  type    = string
  default = "redis://redis:6379"
}

variable "repo_url" {
  type        = string
  default     = ""
  description = "Git repository URL to clone into the workspace"
}

variable "tenant" {
  type        = string
  default     = ""
  description = "OpenFlows tenant identifier"
}

variable "coder_url" {
  type        = string
  default     = ""
  description = "Coder server URL for API calls"
}

variable "harness_version" {
  type        = string
  default     = "harness-edge"
  description = "openflows-harness binary version to download. Use 'harness-edge' for the latest main-branch build, or a specific version tag (e.g. 'v1.2.0')."
}

data "coder_parameter" "github_pat" {
  name        = "github_pat"
  description = "GitHub Personal Access Token for git clone/push in the workspace"
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

    # Setup git credentials. Prefer an explicit GitHub Personal Access Token
    # (github_pat) because a PAT can be scoped to the target repo/org and works
    # regardless of GitHub App install scope. Fall back to the Coder GitHub App
    # external-auth token (surfaces the "Login with GitHub" button in the UI).
    # A persistent store is a deliberate fallback over Coder's automatic
    # GIT_ASKPASS auth: agent-executed git (push) can run in a subprocess
    # environment without GIT_ASKPASS, so we pin the token once here.
    GIT_TOKEN="${data.coder_parameter.github_pat.value}"
    if [ -z "$GIT_TOKEN" ]; then
      GIT_TOKEN="${data.coder_external_auth.github.access_token}"
    fi
    if [ -n "$GIT_TOKEN" ]; then
      git config --global credential.helper store
      echo "https://x-access-token:$${GIT_TOKEN}@github.com" > /home/coder/.git-credentials
      chmod 600 /home/coder/.git-credentials
      log "Configured git credentials for GitHub push auth"
    else
      log "WARNING: No GitHub token available — set CODER_GITHUB_TOKEN (PAT) or open this workspace and click 'Login with GitHub' (external auth) to grant repo access"
    fi

    # Acquire the repository. Prefer the golden offline seed from the shared
    # artifacts volume (docs §3.2): copy -> refresh -> checkout -> work.
    # Fall back to a direct network clone when no golden seed exists, and fail
    # LOUDLY (WARNING) rather than silently leaving an empty workspace.
    GOLDEN_REPO="/home/coder/.openflows/artifacts/repo"
    # The fresh workspace volume is root-owned initially, so any copy/clone
    # into /home/coder/workspace must run via sudo (then be chowned back).
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -d "$GOLDEN_REPO/.git" ]; then
      sudo cp -a "$GOLDEN_REPO/." /home/coder/workspace/ 2>/dev/null || true
      sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
      log "Copied golden repo seed into /home/coder/workspace"
      # Refresh to latest before checkout (design: copy -> refresh -> checkout).
      cd /home/coder/workspace
      git fetch --all --prune 2>/dev/null || true
      git checkout origin/HEAD 2>/dev/null || git checkout main 2>/dev/null || true
    elif [ -n "${var.repo_url}" ]; then
      log "WARNING: no golden repo seed at $GOLDEN_REPO — falling back to direct clone"
      if sudo git clone ${var.repo_url} /home/coder/workspace 2>/tmp/lore_clone_err.log; then
        sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
        log "Cloned repository into /home/coder/workspace"
      else
        log "WARNING: git clone failed — $(tail -5 /tmp/lore_clone_err.log 2>/dev/null)"
      fi
    else
      log "WARNING: no repo_url and no golden repo seed — workspace has no repository"
    fi

    # Download and install openflows-harness from GitHub releases.
    # The harness is REQUIRED for session coordination (dispatch/status/heartbeat,
    # A2A verify requests), so a missing harness must fail loudly — a silently
    # uncoordinated workspace is worse than a startup error.
    # TEMPORARY: Use mounted dev binaries for local testing when available.
    HARNESS_BIN="/usr/local/bin/openflows-harness"
    if [ -f /opt/openflows-dev/openflows-harness ]; then
      sudo cp /opt/openflows-dev/openflows-harness "$HARNESS_BIN"
      sudo chmod +x "$HARNESS_BIN"
    else
      # Asset naming: both harness-edge and tagged releases use fixed basename
      HARNESS_ASSET="openflows-harness-x86_64-unknown-linux-musl.tar.gz"
      if [ "${var.harness_version}" = "harness-edge" ]; then
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/harness-edge/$${HARNESS_ASSET}"
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Downloading openflows-harness (harness-edge/latest build)..." >&2
      else
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/${var.harness_version}/$${HARNESS_ASSET}"
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Downloading openflows-harness v${var.harness_version}..." >&2
      fi
      for attempt in 1 2 3; do
        if curl -fsSL --retry 3 "$HARNESS_URL" -o /tmp/openflows-harness; then
          tar -xzf /tmp/openflows-harness -C /tmp/ || { echo "FATAL: failed to extract harness tarball" >&2; exit 1; }
          HARNESS_DIR=$(find /tmp/ -maxdepth 1 -type d -name "openflows-harness-*" 2>/dev/null | head -1)
          if [ -n "$HARNESS_DIR" ] && [ -f "$HARNESS_DIR/openflows-harness" ]; then
            sudo mv "$HARNESS_DIR/openflows-harness" "$HARNESS_BIN"
            sudo chmod +x "$HARNESS_BIN"
            rm -rf /tmp/openflows-harness "$HARNESS_DIR"
          else
            echo "FATAL: could not find harness binary in extracted tarball" >&2; exit 1
          fi
          break
        fi
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Harness download attempt $attempt failed; retrying in 5s..." >&2
        sleep 5
      done
    fi
    if [ ! -x "$HARNESS_BIN" ]; then
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] FATAL: openflows-harness could not be installed — agent will not be able to coordinate; failing startup" >&2
      exit 1
    fi

    # Start heartbeat daemon (the ONLY Redis client in the workspace)
    export REDIS_URL="${var.redis_url}"
    export OPENFLOWS_TENANT="${var.tenant}"
    export OPENFLOWS_TICKET="${var.ticket_id}"
    export OPENFLOWS_ROLE="${var.role}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-${var.role}-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-${var.role}-${data.coder_workspace.me.id}"
  image = "codercom/enterprise-base:ubuntu"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  # Mount shared artifact files (repo golden seed, plans, skills).
  # This volume is created by the Nexus workspace and is read-only here.
  volumes {
    container_path = "/home/coder/.openflows/artifacts"
    volume_name    = "openflows-artifacts-${var.tenant}"
    read_only      = true
  }

  env = [
    "REDIS_URL=${var.redis_url}",
    "OPENFLOWS_TENANT=${var.tenant}",
    "OPENFLOWS_TICKET=${var.ticket_id}",
    "OPENFLOWS_ROLE=${var.role}",
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
