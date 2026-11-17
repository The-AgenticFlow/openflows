terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

variable "docker_network" {
  description = "Docker network shared by the control plane and tenant workspaces"
  type        = string
  default     = "openflows_default"
}

variable "connected_local" {
  description = "Use the OpenFlows team's GitHub App with local user authorization"
  type        = bool
  default     = false
}

variable "connected_local_broker_secret_host_path" {
  description = "Host path to the local Manager broker key; never the key value"
  type        = string
  default     = ""
}

variable "connected_local_manager_url" {
  description = "Local Manager URL reachable from Coder workspaces"
  type        = string
  default     = "http://openflows-local-manager:3002"
}

variable "connected_local_helper_host_path" {
  description = "Host path to the connected-local workspace credential helper"
  type        = string
  default     = ""
}

variable "workspace_image" {
  description = "STANDALONE TEMPLATE USE ONLY. A mutable release tag used when no workspace_image_digest parameter is set. Manager-provisioned workspaces never use it: Manager always passes workspace_image_repository plus a sha256 workspace_image_digest from the validated workspace-runtime release."
  type        = string
  default     = "ghcr.io/the-agenticflow/workspace-runtime-nexus:v0.2.0"
}

variable "github_api_base" {
  description = "GitHub REST API origin, including any enterprise API path"
  type        = string
  default     = "https://api.github.com"
}

variable "github_git_base" {
  description = "Git transport origin used by the controller to provision repository checkouts"
  type        = string
  default     = "https://github.com"
}

# Optional development override: a host directory (normally .dev-binaries,
# filled by `make dev-sync`) whose OpenFlows programs and runtime scripts replace
# the image's. Releases never set it.
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

data "coder_parameter" "redis_username" {
  name        = "redis_username"
  description = "Tenant-scoped Redis ACL username for hosted workspaces"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "redis_namespace" {
  name        = "redis_namespace"
  description = "Immutable tenant Redis namespace for hosted workspaces"
  default     = ""
  type        = "string"
  mutable     = false
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

data "coder_parameter" "fleet_size" {
  name        = "fleet_size"
  description = "Requested number of Forge/Sentinel worker pairs"
  default     = "1"
  type        = "number"
  mutable     = false
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

data "coder_parameter" "coder_chat_hook_secret" {
  name        = "coder_chat_hook_secret"
  description = "Required shared OpenFlows/Coder lifecycle hook signing secret (32+ random bytes)"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "docs_gate_secret" {
  name        = "docs_gate_secret"
  description = "Optional dedicated secret (32+ bytes) for sealing operator docs approvals (OPENFLOWS_DOCS_GATE_SECRET). Controller only; empty falls back to the hook secret."
  default     = ""
  type        = "string"
  mutable     = true
}

data "coder_parameter" "docs_gate_secret_previous" {
  name        = "docs_gate_secret_previous"
  description = "The previous docs gate secret during a rotation (OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS); approvals sealed with it still verify."
  default     = ""
  type        = "string"
  mutable     = true
}

data "coder_parameter" "coder_chat_hook_url" {
  name        = "coder_chat_hook_url"
  description = "The exact hook URL Coder is configured to POST to (CODER_CHAT_HOOK_URL). When set it is the authoritative JWT audience the consumer validates against, keeping it in lockstep with Coder and preventing aud drift."
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "start_controller" {
  name        = "start_controller"
  description  = "Whether to auto-start the OpenFlows controller on workspace startup"
  default     = "true"
  type        = "bool"
}

# Hosted runtime mode. When true, the workspace sources scoped runtime
# credentials from the secret-injection backend's mounted file
# (OPENFLOWS_RUNTIME_CREDENTIAL_FILE) instead of platform/deployment-wide
# credentials: the platform Coder session token, the deployment-wide lifecycle
# hook signing secret, and the owner's GitHub external-auth token are never
# exported into the runtime in this mode. Local mode (the default) preserves the
# existing operator workflow unchanged.
data "coder_parameter" "hosted" {
  name        = "hosted"
  description  = "Whether this workspace is provisioned in hosted mode (scoped runtime credentials)"
  default     = "false"
  type        = "bool"
  mutable     = false
}

data "coder_parameter" "runtime_secret_ref" {
  name        = "runtime_secret_ref"
  description = "Provisioner-resolved per-workspace secret reference"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "workspace_image_repository" {
  name        = "workspace_image_repository"
  description = "Release-pinned container repository for the Nexus workspace image"
  default     = "ghcr.io/the-agenticflow/workspace-runtime-nexus"
  type        = "string"
  mutable     = false
}

data "coder_parameter" "workspace_image_digest" {
  name        = "workspace_image_digest"
  description = "Release-pinned sha256 digest for the hosted workspace image"
  default     = ""
  type        = "string"
  mutable     = false
}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # Shared startup from the workspace runtime (runtime/lib/bootstrap.sh in the
    # workspace image). A development copy mounted by `make dev-sync` replaces
    # the image's. The template and the runtime must agree on the runtime API.
    OF_RUNTIME_LIB=/opt/openflows/runtime/lib/bootstrap.sh
    [ ! -f /opt/openflows-dev/runtime/lib/bootstrap.sh ] || OF_RUNTIME_LIB=/opt/openflows-dev/runtime/lib/bootstrap.sh
    . "$OF_RUNTIME_LIB" || { echo "FATAL: cannot load the workspace runtime from $OF_RUNTIME_LIB" >&2; exit 1; }
    [ "$${OF_RUNTIME_API:-}" = 1 ] || { echo "FATAL: $OF_RUNTIME_LIB provides workspace runtime API '$${OF_RUNTIME_API:-none}' but this template needs 1; use the workspace image of the templates' release" >&2; exit 1; }
    OF_HOSTED="${data.coder_parameter.hosted.value}"
    OF_CONNECTED_LOCAL="${var.connected_local}"
    OF_RUNTIME_SECRET_REF="${data.coder_parameter.runtime_secret_ref.value}"
    OF_REPO_URL="${data.coder_parameter.repo_url.value}"

    # Nexus owns the tenant artifacts volume (local mode) and seeds the golden
    # checkout workers copy. Hosted Nexus also receives the tenant's lifecycle
    # hook key and URL from the runtime secret.
    of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url
    if of_has_runtime_secret; then
      # The controller uses this Manager runtime session for hosted
      # control-plane calls; Coder user credentials never reach the workspace.
      export OPENFLOWS_MANAGER_URL="$OF_MANAGER_URL"
    fi

    # Role settings. Use the internal 'coder' hostname when it resolves inside
    # the workspace, otherwise the URL passed in for host-side provisioning.
    if curl -sf http://coder:7080/api/v2/buildinfo >/dev/null 2>&1; then
      export CODER_URL="http://coder:7080"
    else
      export CODER_URL="${data.coder_parameter.coder_url.value}"
    fi
    # Hosted mode never exports the platform Coder session token or the
    # deployment-wide hook signing secret, and keeps the tenant-scoped Redis
    # endpoint it loaded from the runtime secret: parameters are visible to
    # Coder and are not the secret delivery channel.
    if ! of_has_runtime_secret; then
      export CODER_SESSION_TOKEN="${data.coder_parameter.coder_session_token.value}"
      export CODER_CHAT_HOOK_SECRET="${data.coder_parameter.coder_chat_hook_secret.value}"
      export OPENFLOWS_DOCS_GATE_SECRET="${data.coder_parameter.docs_gate_secret.value}"
      export OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS="${data.coder_parameter.docs_gate_secret_previous.value}"
      export REDIS_URL="${data.coder_parameter.redis_url.value}"
      export OPENFLOWS_REDIS_USERNAME="${data.coder_parameter.redis_username.value}"
      export OPENFLOWS_REDIS_NAMESPACE="${data.coder_parameter.redis_namespace.value}"
      export OPENFLOWS_REGISTRY_JSON='${data.coder_parameter.registry_json.value}'
    fi
    # Fleet size is authoritative only when provisioned by the Manager (hosted or
    # local Manager with runtime_secret_ref), where the Manager passes it as a
    # workspace parameter. Legacy local tenants supply their fleet through
    # OPENFLOWS_REGISTRY_JSON (tenant add --fleet N); exporting a parameter that
    # defaults to 1 here would shrink that registry fleet.
    if of_has_runtime_secret; then
      export OPENFLOWS_FLEET_SIZE="${data.coder_parameter.fleet_size.value}"
    fi
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export GITHUB_REPOSITORY="${data.coder_parameter.github_repository.value}"
    if [ -n "${data.coder_parameter.coder_chat_hook_url.value}" ]; then
      export CODER_CHAT_HOOK_URL="${data.coder_parameter.coder_chat_hook_url.value}"
    fi

    # Role command: the controller. It reports readiness to the Manager itself
    # after opening its tenant store; a finished bootstrap does not mean it runs.
    cd /home/coder/workspace
    if [ "${data.coder_parameter.start_controller.value}" = "true" ]; then
      log "Starting OpenFlows Controller..."
      nohup openflows run >/tmp/openflows-controller.log 2>&1 &
      log "Controller started. Check logs: tail -f /tmp/openflows-controller.log"
    else
      log "Manual mode: controller not auto-started. Run 'openflows run' when ready."
    fi
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-nexus-${data.coder_workspace.me.id}"
}

# Tenant golden repository volume, local mode only. A hosted workspace clones
# from the remote with its own brokered credential, so hosted mode creates no
# shared volume and requires no read-write-many storage.
resource "docker_volume" "golden" {
  count = data.coder_parameter.hosted.value != "true" ? 1 : 0
  name  = "openflows-golden-${data.coder_parameter.tenant.value}"
}

resource "docker_container" "workspace" {
  count = data.coder_workspace.me.start_count
  name  = "openflows-nexus-${data.coder_workspace.me.id}"
  image = data.coder_parameter.workspace_image_digest.value != "" ? "${data.coder_parameter.workspace_image_repository.value}@${data.coder_parameter.workspace_image_digest.value}" : var.workspace_image
  # The workspace image is published for linux/amd64 only; Docker emulates
  # it on Apple Silicon and other ARM64 hosts.
  platform = "linux/amd64"

  # Linux Docker has no built-in host.docker.internal. Map it to the host
  # gateway so workspaces can reach a Manager running on the Docker host
  # (Docker Desktop on macOS/Windows already provides this name).
  host {
    host = "host.docker.internal"
    ip   = "host-gateway"
  }

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  # Tenant golden repository volume, local mode only. Nexus seeds it; local
  # workers mount it read-only. Mounted at a path distinct from the role assets
  # (`$HOME/.openflows/artifacts`, installed from the pinned image).
  dynamic "volumes" {
    for_each = data.coder_parameter.hosted.value != "true" ? [1] : []
    content {
      container_path = "/home/coder/.openflows/golden"
      volume_name    = docker_volume.golden[0].name
    }
  }

  # Optional development override (see variable "dev_binary_host_path").
  dynamic "volumes" {
    for_each = var.dev_binary_host_path != "" ? [1] : []
    content {
      container_path = "/opt/openflows-dev"
      host_path      = var.dev_binary_host_path
      read_only      = true
    }
  }

  dynamic "volumes" {
    for_each = data.coder_parameter.runtime_secret_ref.value != "" ? [1] : []
    content {
      container_path = "/run/openflows/runtime.json"
      host_path      = data.coder_parameter.runtime_secret_ref.value
      read_only      = true
    }
  }

  dynamic "volumes" {
    for_each = var.connected_local && var.connected_local_broker_secret_host_path != "" ? [1] : []
    content {
      container_path = "/run/openflows/local-broker-key"
      host_path      = var.connected_local_broker_secret_host_path
      read_only      = true
    }
  }

  dynamic "volumes" {
    for_each = var.connected_local && var.connected_local_helper_host_path != "" ? [1] : []
    content {
      container_path = "/opt/openflows/connected-local-github.py"
      host_path      = var.connected_local_helper_host_path
      read_only      = true
    }
  }

  env = concat([
    "GITHUB_API_BASE=${var.github_api_base}",
    "GITHUB_GIT_BASE=${var.github_git_base}",
    # Always use internal 'coder' hostname - Coder is reachable as 'coder' from workspace containers
    "CODER_URL=http://coder:7080",
    "OPENFLOWS_RUNTIME_CREDENTIAL_FILE=/run/openflows/runtime.json",
    "OPENFLOWS_LOCAL_MANAGER_URL=${var.connected_local_manager_url}",
    "OPENFLOWS_CONNECTED_LOCAL_REPOSITORY=${data.coder_parameter.github_repository.value}",
    "OPENFLOWS_GITHUB_API_TOKEN_FILE=/home/coder/.config/openflows/github-api-token",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "GITHUB_REPOSITORY=${data.coder_parameter.github_repository.value}",
    "CODER_CHAT_HOOK_URL=${data.coder_parameter.coder_chat_hook_url.value}",
    "ROLE=nexus",
    "OPENFLOWS_ROLE=nexus",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    # Bind the A2A relay on all interfaces so Forge/Sentinel workspaces can
    # reach it over the shared docker network (issue #143). Workspaces address
    # it via the `openflows-nexus` network alias below.
    "A2A_RELAY_ADDR=0.0.0.0:3000",
    # Path to shared artifacts (agent personas, skills, standards, plans)
    "ARTIFACTS_DIR=/home/coder/.openflows/artifacts",
  ], data.coder_parameter.hosted.value == "true" ? [] : [
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
    "OPENFLOWS_REDIS_USERNAME=${data.coder_parameter.redis_username.value}",
    "OPENFLOWS_REDIS_NAMESPACE=${data.coder_parameter.redis_namespace.value}",
    # Local mode only: platform/deployment-wide credentials and the legacy
    # registry. Hosted mode sources scoped runtime credentials from the
    # secret-injection backend and never exports these into the runtime.
    "CODER_SESSION_TOKEN=${data.coder_parameter.coder_session_token.value}",
    "OPENFLOWS_REGISTRY_JSON=${data.coder_parameter.registry_json.value}",
    "CODER_CHAT_HOOK_SECRET=${data.coder_parameter.coder_chat_hook_secret.value}",
    "OPENFLOWS_DOCS_GATE_SECRET=${data.coder_parameter.docs_gate_secret.value}",
    "OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS=${data.coder_parameter.docs_gate_secret_previous.value}",
    # Agent GitHub API calls use the Manager-brokered token file.
  ])

  networks_advanced {
    name = var.docker_network
    # Stable service name so forge/sentinel templates can default
    # A2A_RELAY_ADDR to openflows-nexus:3000 without knowing the workspace id.
    # Manager-provisioned workers (hosted and local with runtime_secret_ref)
    # address the tenant-scoped alias openflows-nexus-<tenant>:3000.
    aliases = data.coder_parameter.hosted.value == "true" ? ["openflows-nexus-${data.coder_parameter.tenant.value}"] : (data.coder_parameter.runtime_secret_ref.value != "" ? ["openflows-nexus", "openflows-nexus-${data.coder_parameter.tenant.value}"] : ["openflows-nexus"])
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
