terraform {
  required_providers {
    coder = { source = "coder/coder" }
    docker = { source = "kreuzwerker/docker" }
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
  default     = "1.1.6"
  description = "openflows-harness binary version to download"
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

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # TEMPORARY: Use mounted dev binaries for local testing
    # (In production, download from GitHub releases instead)
    if [ -f /opt/openflows-dev/openflows-harness ]; then
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Using mounted dev harness binary..." >&2
      sudo cp /opt/openflows-dev/openflows-harness /usr/local/bin/openflows-harness
      sudo chmod +x /usr/local/bin/openflows-harness
    else
      # Fallback: try to download from GitHub releases
      HARNESS_URL="https://github.com/Kilo-Org/openflows/releases/download/v${var.harness_version}/openflows-harness-v${var.harness_version}-x86_64-unknown-linux-musl"
      HARNESS_BIN="/usr/local/bin/openflows-harness"
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Downloading openflows-harness v${var.harness_version}..." >&2
      curl -fsSL "$HARNESS_URL" -o "$HARNESS_BIN" && chmod +x "$HARNESS_BIN" || {
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] WARNING: Failed to download openflows-harness — agent will not be able to coordinate" >&2
      }
    fi

    # git pull or clone (creds via Coder external auth)
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

    # Start heartbeat daemon (the ONLY Redis client in the workspace)
    export REDIS_URL="${data.coder_parameter.redis_url.value}"
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export OPENFLOWS_TICKET="${data.coder_parameter.ticket_id.value}"
    export OPENFLOWS_ROLE="${data.coder_parameter.role.value}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
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

  # Mount shared orchestration files (agent definitions, skills, standards)
  # This volume is created by the Nexus workspace
  volumes {
    container_path = "/home/coder/.openflows/orchestration"
    volume_name    = "openflows-orchestration-${data.coder_parameter.tenant.value}"
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
    "OPENFLOWS_ROLE=${data.coder_parameter.role.value}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  # egress allowlist: Coder control plane + github.com + Redis only
  # (enforced at network level; Redis is a documented exception per docs/governance.md)

  networks_advanced {
    name = "openflows_default"
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "172.17.0.1")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
