terraform {
  required_providers {
    coder = { source = "coder/coder" }
    docker = { source = "kreuzwerker/docker" }
  }
}

variable "agent_module_source" {
  type        = string
  default     = "registry.coder.com/coder/claude-code/coder"
  description = "Coder Registry module source for agent CLI"
}

variable "agent_module_version" {
  type        = string
  default     = "5.2.0"
  description = "Version of the agent module"
}

variable "enable_ai_gateway" {
  type        = bool
  default     = true
  description = "Enable Coder AI Gateway for model routing"
}

variable "coder_url" {
  type        = string
  default     = ""
  description = "Coder server URL exposed to the Nexus workspace"
}

variable "coder_api_token" {
  type        = string
  default     = ""
  description = "Coder API token exposed to the Nexus workspace"
}

variable "registry_json" {
  type        = string
  default     = ""
  description = "Registry JSON injected into the Nexus workspace"
}

variable "use_ai_gateway" {
  type    = string
  default = "true"
}

variable "host_cli_binary" {
  type        = string
  default     = ""
  description = "Host path to a pre-built CLI ELF binary (bind-mounted to skip startup download)"
}

variable "cli_binary_name" {
  type        = string
  default     = "claude"
  description = "Name of the CLI binary to expose on PATH inside the workspace"
}

variable "litellm_proxy_url" {
  type    = string
  default = "http://proxy:4000"
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

variable "enable_slackme" {
  type        = bool
  default     = false
  description = "Enable Slack command-completion notifications"
}

variable "mcp_config" {
  type        = string
  default     = ""
  description = "MCP server configuration (JSON string)"
}

resource "coder_agent" "main" {
  os         = "linux"
  arch       = "amd64"
  dir        = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # git pull or clone
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull origin main 2>/dev/null || true
    elif [ -n "${var.repo_url}" ]; then
      git clone ${var.repo_url} /home/coder/workspace 2>/dev/null || true
    fi

    # If a host-provided CLI binary is bind-mounted, install it onto PATH.
    if [ -x /opt/host-cli/${var.cli_binary_name} ]; then
      mkdir -p /home/coder/.local/bin /tmp/coder-script-data/bin
      ln -sf /opt/host-cli/${var.cli_binary_name} /home/coder/.local/bin/${var.cli_binary_name}
      ln -sf /opt/host-cli/${var.cli_binary_name} /tmp/coder-script-data/bin/${var.cli_binary_name}
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] ${var.cli_binary_name} installed from host bind-mount" >&2
    fi

    # Install Claude Code hooks from orchestration/plugin/hooks/nexus/
    HOOKS_SRC="/home/coder/workspace/orchestration/plugin/hooks/nexus"
    HOOKS_DST="/home/coder/workspace/.claude/hooks/nexus"
    if [ -d "$HOOKS_SRC" ]; then
      mkdir -p "$HOOKS_DST"
      for hook in "$HOOKS_SRC"/*.sh; do
        if [ -f "$hook" ]; then
          cp "$hook" "$HOOKS_DST/"
          chmod +x "$HOOKS_DST/$(basename "$hook")"
        fi
      done
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Nexus hooks installed from $HOOKS_SRC" >&2
    else
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] WARNING: Nexus hooks source not found at $HOOKS_SRC - hooks will be provisioned separately" >&2
    fi

    # SharedStore heartbeat writer
    nohup bash -c 'while true; do
      redis-cli -u ${var.redis_url} SET "heartbeat:nexus" \
        "{\"ts\":$(date +%s),\"ws_id\":\"${data.coder_workspace.me.id}\",\"status\":\"running\"}" \
        2>/dev/null || true
      sleep 30
    done' >/dev/null 2>&1 &
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-nexus-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-nexus-${data.coder_workspace.me.id}"
  image = "codercom/enterprise-base:ubuntu"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  env = [
    "REPO_URL=${var.repo_url}",
    "REDIS_URL=${var.redis_url}",
    "LITELLM_PROXY_URL=${var.litellm_proxy_url}",
    "USE_AI_GATEWAY=${var.use_ai_gateway}",
    "CODER_URL=${var.coder_url}",
    "CODER_API_TOKEN=${var.coder_api_token}",
    "OPENFLOWS_REGISTRY_JSON=${var.registry_json}",
    "ROLE=nexus",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  # Connect to the openflows_default compose network for Redis access.
  networks_advanced {
    name = "openflows_default"
  }

  dynamic "volumes" {
    for_each = var.host_cli_binary != "" ? [var.host_cli_binary] : []
    content {
      host_path      = volumes.value
      container_path = "/opt/host-cli/${var.cli_binary_name}"
      read_only      = true
    }
  }

  # Run Coder agent init script as entrypoint (downloads + starts agent, runs startup_script, keeps container alive)
  # Replace localhost/127.0.0.1 with Docker host gateway so the agent can reach the Coder server
  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "172.17.0.1")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}

# Agent module (configurable CLI backend)
module "agent" {
  source  = "registry.coder.com/coder/claude-code/coder"
  version = "5.2.0"

  agent_id          = coder_agent.main.id
  workdir           = "/home/coder/workspace"
  enable_ai_gateway = var.enable_ai_gateway

}


# Slack notification module (conditional)
module "slackme" {
  count  = var.enable_slackme ? 1 : 0
  source = "registry.coder.com/coder/slackme/coder"
  version = "1.0.33"

  agent_id         = coder_agent.main.id
  auth_provider_id = "slack"
}
