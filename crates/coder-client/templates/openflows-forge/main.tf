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

variable "enable_ai_gateway" {
  type        = bool
  default     = true
  description = "Enable Coder AI Gateway for model routing"
}

variable "use_ai_gateway" {
  type    = string
  default = "true"
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

    # Install Claude Code hooks from orchestration/plugin/hooks/forge/
    HOOKS_SRC="/home/coder/workspace/orchestration/plugin/hooks/forge"
    HOOKS_DST="/home/coder/workspace/.claude/hooks/forge"
    if [ -d "$HOOKS_SRC" ]; then
      mkdir -p "$HOOKS_DST"
      for hook in "$HOOKS_SRC"/*.sh; do
        if [ -f "$hook" ]; then
          cp "$hook" "$HOOKS_DST/"
          chmod +x "$HOOKS_DST/$(basename "$hook")"
        fi
      done
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Forge hooks installed from $HOOKS_SRC" >&2
    else
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] WARNING: Forge hooks source not found at $HOOKS_SRC - hooks will be provisioned separately" >&2
    fi

    # SharedStore heartbeat writer
    nohup bash -c 'while true; do
      redis-cli -u ${var.redis_url} SET "heartbeat:forge-${var.ticket_id}" \
        "{\"ts\":$(date +%s),\"ws_id\":\"${data.coder_workspace.me.id}\",\"status\":\"running\"}" \
        2>/dev/null || true
      sleep 30
    done' >/dev/null 2>&1 &
  EOT
}

resource "docker_container" "workspace" {
  name  = "openflows-forge-${data.coder_workspace.me.id}"
  image = "codercom/enterprise-base:ubuntu"

  # NOTE: /home/coder/workspace is intentionally NOT backed by a named Docker
  # volume.  A `docker_volume` is created root-owned, but the agent runs as the
  # `coder` user, which then cannot write to it — breaking both the startup
  # `git clone` and the provisioner's `mkdir`/settings writes.  Using the
  # container's writable layer keeps the workspace dir coder-owned (it lives
  # under /home/coder in the image).  Forge workspaces are per-ticket and
  # ephemeral; the repo is re-cloned on each start via the startup_script, so
  # cross-restart persistence of the clone is not required.

  env = [
    "REPO_URL=${var.repo_url}",
    "REDIS_URL=${var.redis_url}",
    "LITELLM_PROXY_URL=${var.litellm_proxy_url}",
    "USE_AI_GATEWAY=${var.use_ai_gateway}",
    "ROLE=forge",
    "TICKET_ID=${var.ticket_id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  # Connect to the openflows_default compose network for Redis access.
  networks_advanced {
    name = "openflows_default"
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
