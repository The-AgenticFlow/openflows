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

variable "role" {
  type        = string
  default     = "forge"
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

variable "harness_version" {
  type        = string
  default     = "1.1.6"
  description = "openflows-harness binary version to download"
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
    elif [ -n "${var.repo_url}" ]; then
      git clone ${var.repo_url} /home/coder/workspace 2>/dev/null || true
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

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "172.17.0.1")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
