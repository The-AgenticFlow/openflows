terraform {
  required_providers {
    coder = { source = "coder/coder" }
    docker = { source = "kreuzwerker/docker" }
  }
}

# Workspace-level parameters (set per-workspace via Coder API rich_parameter_values)
data "coder_parameter" "coder_url" {
  name        = "coder_url"
  description  = "Coder server URL exposed to the Nexus workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "coder_session_token" {
  name        = "coder_session_token"
  description  = "Scoped Coder session token for the Controller"
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

data "coder_parameter" "registry_json" {
  name        = "registry_json"
  description  = "Registry JSON injected into the Nexus workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "github_repository" {
  name        = "github_repository"
  description  = "GitHub repository (owner/repo) for the Controller to monitor"
  default     = ""
  type        = "string"
}

data "coder_parameter" "github_pat" {
  name        = "github_pat"
  description  = "GitHub Personal Access Token for issue/PR sync"
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

    # TEMPORARY: Use mounted dev binary for local testing
    # (In production, download from GitHub releases instead)
    if [ -f /opt/openflows-dev/openflows ]; then
      echo "Using mounted dev binary..."
      sudo cp /opt/openflows-dev/openflows /usr/local/bin/openflows
      sudo chmod +x /usr/local/bin/openflows
    else
      echo "WARNING: Dev binary not found at /opt/openflows-dev/openflows"
      echo "Controller will not start. Mount .dev-binaries in docker-compose.yml"
    fi

    # git pull or clone (creds via Coder external auth)
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -n "${data.coder_parameter.repo_url.value}" ]; then
      git clone ${data.coder_parameter.repo_url.value} /home/coder/workspace 2>/dev/null || true
    fi

    # Start the OpenFlows Controller
    export CODER_URL="${data.coder_parameter.coder_url.value}"
    export CODER_SESSION_TOKEN="${data.coder_parameter.coder_session_token.value}"
    export REDIS_URL="${data.coder_parameter.redis_url.value}"
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export GITHUB_REPOSITORY="${data.coder_parameter.github_repository.value}"
    export OPENFLOWS_REGISTRY_JSON='${data.coder_parameter.registry_json.value}'
    # GitHub PAT for issue sync (in production, use Coder external auth instead)
    echo "${data.coder_parameter.github_pat.value}" > /tmp/github_token 2>/dev/null || true

    cd /home/coder/workspace
    
    if command -v openflows >/dev/null 2>&1; then
      echo "Starting OpenFlows Controller..."
      nohup openflows run >/tmp/openflows-controller.log 2>&1 &
      echo "Controller started. Check logs: tail -f /tmp/openflows-controller.log"
    else
      echo "ERROR: openflows binary not found. Controller not started."
    fi
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

  # TEMPORARY: Mount dev binaries for local testing (remove when using GitHub releases)
  volumes {
    container_path = "/opt/openflows-dev"
    host_path      = "/home/christian/sandbox/openflows/.dev-binaries"
    read_only      = true
  }

  env = [
    "CODER_URL=${data.coder_parameter.coder_url.value}",
    "CODER_SESSION_TOKEN=${data.coder_parameter.coder_session_token.value}",
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "GITHUB_REPOSITORY=${data.coder_parameter.github_repository.value}",
    "OPENFLOWS_REGISTRY_JSON=${data.coder_parameter.registry_json.value}",
    "GITHUB_TOKEN=${data.coder_parameter.github_pat.value}",
    "ROLE=nexus",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ]

  networks_advanced {
    name = "openflows_default"
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "172.17.0.1")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}