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
  default     = "ghcr.io/the-agenticflow/workspace-runtime-forge:v0.2.0"
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

variable "a2a_relay_addr" {
  type        = string
  default     = "openflows-nexus:3000"
  description = "Address of the nexus A2A relay (JSON-RPC verify transport, issue #143). Forge resolves the nexus container over the shared docker network by its service name to claim and execute verify tasks."
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

data "coder_parameter" "branch" {
  name        = "branch"
  description  = "Remote branch to check out (the PR branch for an existing PR). Empty = create or resume the worker/ticket branch."
  default     = ""
  type        = "string"
}

# The requested branch is interpolated directly into the startup bash script.
# A branch name containing shell metacharacters (e.g. `$(...)`) would otherwise
# be evaluated by bash as command substitution, so validate it against a charset
# that accepts all git-valid, shell-safe characters and fall back to the default
# branch when it does not match. `+`, `=`, `@`, `.`, `_`, `-`, `/` are all legal
# in git ref names and safe in the shell; anything else (dollar, backtick,
# parens, quotes, spaces, `;`, `&`, `|`, `<`, `>`, `!`, `*`, `?`, `[`, `]`,
# `~`, `\`, `#`) is rejected so a malicious branch cannot inject shell commands.
locals {
  requested_branch = data.coder_parameter.branch.value
  safe_branch      = can(regex("^[A-Za-z0-9._/+@=-]+$", local.requested_branch)) ? local.requested_branch : ""
}

data "coder_parameter" "tenant" {
  name        = "tenant"
  description  = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "a2a_pair_token" {
  name        = "a2a_pair_token"
  description = "Pair-scoped token used by Sentinel/FORGE to authenticate A2A verification RPCs"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "coder_url" {
  name        = "coder_url"
  description  = "Coder server URL for API calls"
  default     = ""
  type        = "string"
}

# Hosted runtime mode. When true, this worker sources scoped runtime
# credentials from the secret-injection backend's mounted file
# (OPENFLOWS_RUNTIME_CREDENTIAL_FILE) instead of the owner's Coder external-auth
# token, so the tenant's Git identity is scoped and rotation does not require a
# rebuild. Local mode (the default) preserves the existing operator workflow.
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
  description = "Release-pinned container repository for the Forge workspace image"
  default     = "ghcr.io/the-agenticflow/workspace-runtime-forge"
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

data "coder_parameter" "a2a_relay_addr" {
  name = "a2a_relay_addr"
  type = "string"
  default = "openflows-nexus:3000"
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

    # Role settings. The worker id (forge-1) names the task branch; the BASE
    # role (forge) selects the role assets and the controller's Redis keys.
    ROLE="${data.coder_parameter.role.value}"
    ROLE_BASE="$${ROLE%-*}"
    TICKET_ID="${data.coder_parameter.ticket_id.value}"
    OF_ROLE="$ROLE_BASE"
    OF_REPO_URL="${data.coder_parameter.repo_url.value}"
    # FORGE commits, pushes and opens PRs: every credential and the author
    # identity are required. The harness uses the API-purpose token for PRs.
    OF_GITHUB_ACCESS=required
    OF_GITHUB_API_TOKEN=true
    OF_GIT_IDENTITY=required
    OF_ROLE_HOOKS=true
    # A PR `branch` (rework) resumes the PR branch; otherwise ROLE/TICKET_ID.
    OF_CHECKOUT=assignment
    OF_TARGET_BRANCH="${local.safe_branch}"
    OF_WORKER="$ROLE"
    OF_TICKET="$TICKET_ID"

    # Ends with the readiness heartbeat the Manager waits for before creating a Chat.
    of_bootstrap_worker

    # Hosted mode already loaded the tenant-scoped Redis URL from the runtime
    # secret; Coder parameters are not a secret channel.
    if ! of_has_runtime_secret; then
      export REDIS_URL="${data.coder_parameter.redis_url.value}"
    fi
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export OPENFLOWS_TICKET="$TICKET_ID"
    export OPENFLOWS_ROLE="$ROLE_BASE"
    export A2A_RELAY_ADDR="${data.coder_parameter.a2a_relay_addr.value}"
    export A2A_PAIR_TOKEN="${data.coder_parameter.a2a_pair_token.value}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"

    # Role commands. The heartbeat daemon is the only Redis client in the workspace.
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
    log "Heartbeat daemon started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # The A2A verify executor (issue #143) runs delegated verification in
    # temporary checkouts, supervised outside the agent chat so Redis/relay
    # startup races retry. Diagnostics stay outside the checkout.
    nohup bash -c 'while true; do openflows-harness verify serve; sleep 5; done' \
      >>/home/coder/.local/state/openflows/verify.log 2>&1 &
    log "Verify executor started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # A local agent CLI (claude or codex), mounted from the host or on PATH,
    # works the ticket directly; its SessionStart hook supplies the dispatch.
    # Otherwise the workspace waits for the Coder Chat the controller creates.
    AGENT_CLI=""
    if ! of_hosted; then
      for cli in claude codex; do
        if [ -f "/opt/openflows-dev/$cli" ]; then
          sudo install -m 0755 "/opt/openflows-dev/$cli" "/usr/local/bin/$cli"
          AGENT_CLI="$cli"
          log "Mounted $cli binary from host — starting agent"
          break
        elif command -v "$cli" >/dev/null 2>&1; then
          AGENT_CLI="$cli"
          log "$cli found on PATH — starting agent"
          break
        fi
      done
    fi
    if of_hosted; then
      log "Hosted Forge execution is started by Nexus through a Manager-created Coder Chat"
    elif [ -n "$AGENT_CLI" ]; then
      nohup "$AGENT_CLI" -p "Start working on ticket $OPENFLOWS_TICKET. Read dispatch with 'openflows-harness dispatch read' and follow the workflow. Use 'openflows-harness status set <phase>' to report progress." </dev/null >/tmp/agent.log 2>&1 &
      log "Agent started (cli=$AGENT_CLI ticket=$OPENFLOWS_TICKET pid=$!)"
    else
      log "No agent CLI binary found — waiting for a Coder Chat from the controller"
    fi
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-${data.coder_parameter.role.value}-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  count = data.coder_workspace.me.start_count
  name  = "openflows-${data.coder_parameter.role.value}-${data.coder_workspace.me.id}"
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

  # Local mode only: the tenant golden repository seed that the Nexus workspace
  # keeps fresh, mounted read-only at a path DISTINCT from the role assets
  # (`$HOME/.openflows/artifacts`, installed from the pinned image) so the mount
  # can never shadow them. A hosted workspace mounts no shared storage and clones
  # with its own brokered credential.
  dynamic "volumes" {
    for_each = data.coder_parameter.hosted.value != "true" ? [1] : []
    content {
      container_path = "/home/coder/.openflows/golden"
      volume_name    = "openflows-golden-${data.coder_parameter.tenant.value}"
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
    "OPENFLOWS_RUNTIME_CREDENTIAL_FILE=/run/openflows/runtime.json",
    "OPENFLOWS_LOCAL_MANAGER_URL=${var.connected_local_manager_url}",
    "OPENFLOWS_CONNECTED_LOCAL_REPOSITORY=${data.coder_parameter.repo_url.value}",
    "OPENFLOWS_GITHUB_API_TOKEN_FILE=/home/coder/.config/openflows/github-api-token",
    "OPENFLOWS_TENANT=${data.coder_parameter.tenant.value}",
    "OPENFLOWS_TICKET=${data.coder_parameter.ticket_id.value}",
    # Base role (forge-1 -> forge): harness Redis keys are namespaced by base role
    "OPENFLOWS_ROLE=${replace(data.coder_parameter.role.value, "/-[0-9]+$/", "")}",
    "A2A_RELAY_ADDR=${data.coder_parameter.a2a_relay_addr.value}",
    "A2A_PAIR_TOKEN=${data.coder_parameter.a2a_pair_token.value}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
  ], data.coder_parameter.hosted.value == "true" ? [] : [
    "REDIS_URL=${data.coder_parameter.redis_url.value}",
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
