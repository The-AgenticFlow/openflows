terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

variable "role" {
  type        = string
  default     = "sentinel"
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

variable "a2a_relay_addr" {
  type        = string
  default     = "openflows-nexus:3000"
  description = "Address of the nexus A2A relay (JSON-RPC verify transport, issue #143). Workspaces resolve the nexus container over the shared docker network by its service name."
}

# TEMPORARY: Host path to the .dev-binaries directory on the Docker host.
# Set via TF_VAR_dev_binary_host_path before running `coder templates push`.
# The mounted dev harness is used when available (the current fallback when no
# GitHub Release asset exists); remove when switching to GitHub releases only.
variable "dev_binary_host_path" {
  description = "Absolute host path to the .dev-binaries directory"
  type        = string
  default     = ""
}

# Workspace-level parameters (set per-workspace via Coder API rich_parameter_values).
# These coder_parameter data sources mirror the Terraform variables so Coder BOTH
# (a) makes them available as rich parameters at workspace creation time, AND
# (b) lets the startup script interpolate them for env-injection by Coder's agent.
data "coder_parameter" "role" {
  name        = "role"
  description = "Agent role name"
  default     = "sentinel"
  type        = "string"
}

data "coder_parameter" "ticket_id" {
  name        = "ticket_id"
  description = "Ticket identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "redis_url" {
  name        = "redis_url"
  description = "Redis SharedStore URL"
  default     = "redis://redis:6379"
  type        = "string"
}

data "coder_parameter" "repo_url" {
  name        = "repo_url"
  description = "Git repository URL to clone into the workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "tenant" {
  name        = "tenant"
  description = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "coder_url" {
  name        = "coder_url"
  description = "Coder server URL for API calls"
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

    # Ensure the workspace dir is owned by the coder user so the agent and
    # the provisioner (skills/standards writes) can create files there.
    # The volume is initialized root-owned inside codercom/enterprise-base.
    sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
    mkdir -p /home/coder/workspace
    sudo chown -R coder:coder /home/coder/workspace

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
    elif [ -n "${var.repo_url}" ]; then
      git clone ${var.repo_url} /home/coder/workspace 2>/dev/null || true
    fi

    # Download and install openflows-harness from GitHub releases.
    # The harness is REQUIRED for session coordination (dispatch/status/heartbeat,
    # A2A verify requests), so a missing harness must fail loudly — a silently
    # uncoordinated workspace is worse than a startup error.
    # TEMPORARY: Use mounted dev binaries for local testing when available.
    HARNESS_BIN="/usr/local/bin/openflows-harness"
    if [ -f /opt/openflows-dev/openflows-harness ]; then
      log "Using mounted dev harness binary..."
      sudo cp /opt/openflows-dev/openflows-harness "$HARNESS_BIN"
      sudo chmod +x "$HARNESS_BIN"
    else
      # Construct download URL based on harness_version:
      #   harness-edge  → harness-edge pre-release (auto-updated on every main push)
      #   v1.2.0       → tagged feature release (stable, audited)
      # Asset naming: both tracks use fixed filename openflows-harness-x86_64-unknown-linux-musl.tar.gz
      # which GitHub auto-replaces on re-upload with the same name.
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
          tar -xzf /tmp/openflows-harness -C /tmp/ || {
            echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] FATAL: failed to extract harness tarball" >&2
            exit 1
          }
          HARNESS_DIR=$(find /tmp/ -maxdepth 1 -type d -name "openflows-harness-*" 2>/dev/null | head -1)
          if [ -n "$HARNESS_DIR" ] && [ -f "$HARNESS_DIR/openflows-harness" ]; then
            sudo mv "$HARNESS_DIR/openflows-harness" "$HARNESS_BIN"
            sudo chmod +x "$HARNESS_BIN"
            rm -rf /tmp/openflows-harness "$HARNESS_DIR"
          else
            echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] FATAL: could not find harness binary in extracted tarball" >&2
            exit 1
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

    # Start heartbeat daemon (the ONLY Redis client in the workspace).
    # Use the BASE role so the controller can find the heartbeat under the
    # base-role key (not the worker id — sentinel always matches base role).
    export REDIS_URL="${data.coder_parameter.redis_url.value}"
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export OPENFLOWS_TICKET="${data.coder_parameter.ticket_id.value}"
    export OPENFLOWS_ROLE="sentinel"
    export A2A_RELAY_ADDR="${var.a2a_relay_addr}"
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

  # Mount shared artifact files (agent definitions, skills, standards, plans).
  # This volume is created by the Nexus workspace.  Sentinel reads the
  # forge-written PLAN.md (and other planning gate artifacts) from here.
  volumes {
    container_path = "/home/coder/.openflows/artifacts"
    volume_name    = "openflows-artifacts-${data.coder_parameter.tenant.value}"
    read_only      = true
  }

  # TEMPORARY: Mount dev binaries for local testing (remove when using GitHub releases).
  # The sentinel startup script prefers the mounted /opt/openflows-dev/openflows-harness
  # when present; this is what keeps the harness available until a hosted release exists.
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
    "OPENFLOWS_ROLE=sentinel",
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
