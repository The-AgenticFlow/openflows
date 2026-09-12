terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

# TEMPORARY: Host path to the .dev-binaries directory on the Docker host.
# Set via TF_VAR_dev_binary_host_path before running `coder templates push`.
# (Remove when switching to GitHub releases for the openflows binary.)
variable "dev_binary_host_path" {
  description = "Absolute host path to the .dev-binaries directory"
  type        = string
  default     = ""
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

data "coder_parameter" "start_controller" {
  name        = "start_controller"
  description  = "Whether to auto-start the OpenFlows controller on workspace startup"
  default     = "true"
  type        = "bool"
}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # Fix artifacts directory ownership (shared volume is created as root)
    sudo chown -R coder:coder /home/coder/.openflows

    # TEMPORARY: Use mounted dev binary for local testing
    # (In production, download from GitHub releases instead)
    if [ -f /opt/openflows-dev/openflows ]; then
      echo "Using mounted dev binary..."
      sudo cp /opt/openflows-dev/openflows /usr/local/bin/openflows
      sudo chmod +x /usr/local/bin/openflows

      # Self-healing: if the workspace has a git checkout, warn when the
      # mounted binary is older than the latest source commit.  This catches
      # the case where someone forgot to run 'make dev-sync' before starting
      # the workspace.
      if [ -d /home/coder/workspace/.git ]; then
        BIN_MTIME=$(stat -c %Y /opt/openflows-dev/openflows 2>/dev/null || echo 0)
        LAST_COMMIT=$(cd /home/coder/workspace && git log -1 --format=%ct 2>/dev/null || echo 0)
        if [ "$LAST_COMMIT" -gt 0 ] && [ "$BIN_MTIME" -gt 0 ] && [ "$LAST_COMMIT" -gt "$BIN_MTIME" ]; then
          echo "WARNING: Dev binary (mt=$BIN_MTIME) is older than the latest commit (ts=$LAST_COMMIT)" >&2
          echo "         Run 'make dev-sync' on the host to rebuild and update the binary" >&2
        fi
      fi
    else
      echo "WARNING: Dev binary not found at /opt/openflows-dev/openflows"
      echo "Controller will not start. Mount .dev-binaries in docker-compose.yml"
    fi

    # Setup git credentials from Coder external auth (GitHub App).
    # Declared via data "coder_external_auth" -> this also surfaces the
    # "Login with GitHub" button in the workspace UI.
    GITHUB_TOKEN="${data.coder_external_auth.github.access_token}"
    if [ -n "$GITHUB_TOKEN" ]; then
      git config --global credential.helper store
      echo "https://x-access-token:$${GITHUB_TOKEN}@github.com" > /home/coder/.git-credentials
      chmod 600 /home/coder/.git-credentials
      echo "Configured git credentials for GitHub"
    else
      echo "WARNING: No GitHub token available — open this workspace and click 'Login with GitHub' (external auth)"
    fi

    # git pull or clone (creds via Coder external auth)
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -n "${data.coder_parameter.repo_url.value}" ]; then
      git clone ${data.coder_parameter.repo_url.value} /home/coder/workspace 2>/dev/null || true
    fi

    # Start the OpenFlows Controller
    # Detect Coder URL: use internal 'coder' hostname when running inside workspace,
    # otherwise use the passed-in external URL (for bootstrap/provisioning from host)
    if curl -sf http://coder:7080/api/v2/buildinfo >/dev/null 2>&1; then
      export CODER_URL="http://coder:7080"
    else
      export CODER_URL="${data.coder_parameter.coder_url.value}"
    fi
    export CODER_SESSION_TOKEN="${data.coder_parameter.coder_session_token.value}"
    export REDIS_URL="${data.coder_parameter.redis_url.value}"
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export GITHUB_REPOSITORY="${data.coder_parameter.github_repository.value}"
    export OPENFLOWS_REGISTRY_JSON='${data.coder_parameter.registry_json.value}'
    # GitHub PAT for issue sync - export as env var so controller picks it up automatically
    export GITHUB_TOKEN="${data.coder_parameter.github_pat.value}"
    echo "${data.coder_parameter.github_pat.value}" > /tmp/github_token 2>/dev/null || true

    cd /home/coder/workspace

    # Only auto-start the controller if explicitly enabled
    if [ "${data.coder_parameter.start_controller.value}" = "true" ]; then
      if command -v openflows >/dev/null 2>&1; then
        echo "Starting OpenFlows Controller..."
        nohup openflows run >/tmp/openflows-controller.log 2>&1 &
        echo "Controller started. Check logs: tail -f /tmp/openflows-controller.log"
      else
        echo "ERROR: openflows binary not found. Controller not started."
      fi
    else
      echo "Manual mode: controller not auto-started. Run 'openflows run' when ready."
    fi
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-nexus-${data.coder_workspace.me.id}"
}

# Shared artifacts volume — Nexus writes (skills, standards, personas),
# forge/sentinel read (and forge writes PLAN.md for sentinel review).
resource "docker_volume" "artifacts" {
  name = "openflows-artifacts-${data.coder_parameter.tenant.value}"
}

resource "docker_container" "workspace" {
  name  = "openflows-nexus-${data.coder_workspace.me.id}"
  image = "codercom/enterprise-base:ubuntu"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  # Artifacts volume (written by Nexus, read by forge/sentinel/lore/vessel)
  volumes {
    container_path = "/home/coder/.openflows/artifacts"
    volume_name    = docker_volume.artifacts.name
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
    # Always use internal 'coder' hostname - Coder is reachable as 'coder' from workspace containers
    "CODER_URL=http://coder:7080",
    "CODER_SESSION_TOKEN=${data.coder_parameter.coder_session_token.value}",
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "GITHUB_REPOSITORY=${data.coder_parameter.github_repository.value}",
    "OPENFLOWS_REGISTRY_JSON=${data.coder_parameter.registry_json.value}",
    "GITHUB_TOKEN=${data.coder_parameter.github_pat.value}",
    "ROLE=nexus",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    # Bind the A2A relay on all interfaces so Forge/Sentinel workspaces can
    # reach it over the shared docker network (issue #143). Workspaces address
    # it via the `openflows-nexus` network alias below.
    "A2A_RELAY_ADDR=0.0.0.0:3000",
    # Path to shared artifacts (agent personas, skills, standards, plans)
    "ARTIFACTS_DIR=/home/coder/.openflows/artifacts",
  ]

  networks_advanced {
    name = "openflows_default"
    # Stable service name so forge/sentinel templates can default
    # A2A_RELAY_ADDR to openflows-nexus:3000 without knowing the workspace id.
    aliases = ["openflows-nexus"]
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
data "coder_external_auth" "github" {
  id = "primary-github"
}