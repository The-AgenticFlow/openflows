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
  description = "Workspace image for local workspaces: the OpenFlows workspace image of the templates' release. Setup pins it by digest (OPENFLOWS_WORKSPACE_IMAGE); a source install builds openflows-workspace:dev. There is no floating default, so local workspaces never run a runtime of another release."
  type        = string
  default     = ""
}

variable "workspace_image_repository" {
  description = "Repository of the hosted workspace image. The template release pins its digest (workspace_image_digest)."
  type        = string
  default     = "ghcr.io/the-agenticflow/openflows-workspace"
}

locals {
  # Without an explicit image, local workspaces get a reference that cannot be
  # pulled and whose tag names the setting to fix, instead of a floating tag
  # whose runtime may belong to another release.
  local_workspace_image = var.workspace_image != "" ? var.workspace_image : "${var.workspace_image_repository}:unset-set-TF_VAR_workspace_image-or-OPENFLOWS_WORKSPACE_IMAGE"
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

# Optional development override: a host directory (normally .dev-binaries,
# filled by `make dev-sync`) whose OpenFlows programs and runtime scripts replace
# the image's. Releases never set it.
variable "dev_binary_host_path" {
  description = "Absolute host path to the .dev-binaries directory"
  type        = string
  default     = ""
}

data "coder_parameter" "hosted" {
  name = "hosted"
  type = "bool"
  default = "false"
  mutable = false
}
data "coder_parameter" "runtime_secret_ref" {
  name = "runtime_secret_ref"
  type = "string"
  default = ""
  mutable = false
}

data "coder_parameter" "workspace_image_digest" {
  name        = "workspace_image_digest"
  description = "Release-pinned sha256 digest for the hosted workspace image"
  default     = ""
  type        = "string"
  mutable     = false
}
data "coder_parameter" "role" {
  name = "role"
  type = "string"
  default = "lore"
}
data "coder_parameter" "tenant" {
  name = "tenant"
  type = "string"
  default = ""
}
data "coder_parameter" "repo_url" {
  name = "repo_url"
  type = "string"
  default = ""
}
data "coder_parameter" "ticket_id" {
  name = "ticket_id"
  type = "string"
  default = ""
}

locals {
  role = data.coder_parameter.role.value != "" ? data.coder_parameter.role.value : var.role
  tenant = data.coder_parameter.tenant.value != "" ? data.coder_parameter.tenant.value : var.tenant
  repo_url = data.coder_parameter.repo_url.value != "" ? data.coder_parameter.repo_url.value : var.repo_url
  ticket_id = data.coder_parameter.ticket_id.value != "" ? data.coder_parameter.ticket_id.value : var.ticket_id
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

    # Role settings: the BASE role selects the role assets and Redis keys.
    OF_ROLE="${local.role}"
    OF_REPO_URL="${local.repo_url}"
    # LORE commits and pushes its docs branch, so it needs an author identity.
    # Without a token yet, keep booting: the identity is set on the next start.
    OF_GITHUB_ACCESS=optional
    OF_GIT_IDENTITY=optional
    OF_CHECKOUT=default

    # Ends with the readiness heartbeat the Manager waits for before assigning work.
    of_bootstrap_worker

    # Role environment. Hosted mode already loaded the tenant-scoped Redis URL and
    # namespace from the runtime secret; Coder parameters are not a secret channel.
    if ! of_hosted; then
      export REDIS_URL="${var.redis_url}"
      export OPENFLOWS_TENANT="${local.tenant}"
    fi
    export OPENFLOWS_TICKET="${local.ticket_id}"
    export OPENFLOWS_ROLE="${local.role}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"

    # Role command: the heartbeat daemon, the only Redis client in the workspace.
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-${local.role}-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  count = data.coder_workspace.me.start_count
  name  = "openflows-${local.role}-${data.coder_workspace.me.id}"
  image = data.coder_parameter.hosted.value == "true" ? "${var.workspace_image_repository}@${data.coder_parameter.workspace_image_digest.value}" : local.local_workspace_image
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

  # Local mode reads the artifact tree and the repository seed from the tenant
  # volume, which the Nexus workspace creates. A hosted workspace installs the
  # tree from the Manager bundle and clones with its own credential, so it
  # mounts no shared storage.
  dynamic "volumes" {
    for_each = data.coder_parameter.hosted.value != "true" ? [1] : []
    content {
      container_path = "/home/coder/.openflows/artifacts"
      volume_name    = "openflows-artifacts-${local.tenant}"
      read_only      = true
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
    for_each = data.coder_parameter.hosted.value == "true" && data.coder_parameter.runtime_secret_ref.value != "" ? [1] : []
    content {
      container_path = "/run/openflows/runtime.json"
      host_path = data.coder_parameter.runtime_secret_ref.value
      read_only = true
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
    "OPENFLOWS_RUNTIME_CREDENTIAL_FILE=/run/openflows/runtime.json",
    "OPENFLOWS_LOCAL_MANAGER_URL=${var.connected_local_manager_url}",
    "OPENFLOWS_CONNECTED_LOCAL_REPOSITORY=${local.repo_url}",
    "OPENFLOWS_GITHUB_API_TOKEN_FILE=/home/coder/.config/openflows/github-api-token",
    "OPENFLOWS_TENANT=${local.tenant}",
    "OPENFLOWS_TICKET=${local.ticket_id}",
    "OPENFLOWS_ROLE=${local.role}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    # Agent GitHub API calls use the Manager-brokered token file.
  ], data.coder_parameter.hosted.value == "true" ? [] : [
    "REDIS_URL=${var.redis_url}",
  ])

  # egress allowlist: Coder control plane + github.com + Redis only
  # (enforced at network level; Redis is a documented exception per docs/governance.md)

  networks_advanced {
    name = var.docker_network
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
