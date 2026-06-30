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
  access_url = "http://coder:7080"
  startup_script_timeout = 300

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # Install documentation tooling
    apt-get update -qq >/dev/null 2>&1 || true
    apt-get install -y -qq pandoc >/dev/null 2>&1 || true
    
    # Install mdbook if cargo is available
    if command -v cargo &>/dev/null; then
      cargo install mdbook --quiet 2>/dev/null || true
    fi
    
    # Install markdownlint-cli via npm
    if command -v npm &>/dev/null; then
      npm install -g markdownlint-cli 2>/dev/null || true
    fi

    # git pull or clone
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull origin main 2>/dev/null || true
    elif [ -n "${var.repo_url}" ]; then
      git clone ${var.repo_url} /home/coder/workspace 2>/dev/null || true
    fi

    # SharedStore heartbeat writer
    nohup bash -c 'while true; do
      redis-cli -u ${var.redis_url} HSET "heartbeat:lore-T-${var.ticket_id}" \
        "ts" "$(date +%s)" \
        "ws_id" "${data.coder_workspace.me.id}" \
        "status" "running" 2>/dev/null || true
      sleep 30
    done' >/dev/null 2>&1 &
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-lore-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-lore-${data.coder_workspace.me.id}"
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
    "ROLE=lore",
  ]

  # Connect to the openflows_default compose network for Redis access.
  networks_advanced {
    name = "openflows_default"
  }

  entrypoint = ["sh", "-c"]
  command    = [coder_agent.main.startup_script]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}

# Lore agent module (CLI backend installer)
module "agent" {
  source  = var.agent_module_source
  version = var.agent_module_version

  agent_id          = coder_agent.main.id
  workdir           = "/home/coder/workspace"
  permission_mode   = "acceptEdits"
  enable_ai_gateway = var.enable_ai_gateway

  mcp = var.mcp_config != "" ? var.mcp_config : null
}

# Git configuration module
module "git_config" {
  source  = "registry.coder.com/coder/git-config/coder"
  version = "1.0.0"

  agent_id = coder_agent.main.id
}

# Slack notification module (conditional)
module "slackme" {
  count  = var.enable_slackme ? 1 : 0
  source = "registry.coder.com/coder/slackme/coder"
  version = "1.0.33"

  agent_id         = coder_agent.main.id
  auth_provider_id = "slack"
}
