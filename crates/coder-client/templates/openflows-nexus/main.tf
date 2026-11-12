terraform {
  required_providers {
    coder = { source = "coder/coder", version = "~> 2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

# TEMPORARY: Host path to the .dev-binaries directory on the Docker host.
# Set via TF_VAR_dev_binary_host_path before running `coder templates push`.
# (Remove when switching to GitHub releases for the openflows binary.)
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
  description = "Workspace image containing the tools used by the production bootstrap"
  type        = string
  default     = "codercom/enterprise-base:ubuntu"
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

    # Fix artifacts directory ownership (shared volume is created as root)
    sudo chown -R coder:coder /home/coder/.openflows

    # TEMPORARY: Use mounted dev binary for local testing
    # (In production, download from GitHub releases instead)
    if [ -f /opt/openflows-dev/openflows ]; then
      # Guard against a non-ELF binary being mounted (e.g. a macOS Mach-O
      # accidentally synced from a macOS dev host). Copying it over
      # /usr/local/bin/openflows would brick the controller with cryptic
      # `__PAGEZERO__: not found` errors when sh falls back to parsing the
      # binary as a script after the kernel returns ENOEXEC.
      if head -c 4 /opt/openflows-dev/openflows | grep -q $'\x7fELF'; then
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
        echo "WARNING: Mounted dev binary is not a Linux ELF; keeping the image's built-in binary." >&2
        echo "         Run 'make dev-sync' on the host to rebuild for $${LINUX_TARGET:-x86_64-unknown-linux-musl}." >&2
      fi
    else
      echo "WARNING: Dev binary not found at /opt/openflows-dev/openflows"
      echo "Controller will not start. Mount .dev-binaries in docker-compose.yml"
    fi

    # Hosted mode uses a mounted runtime secret. Local mode requests a
    # short-lived GitHub App user token from the local Manager.
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      RUNTIME_CREDENTIAL_COPY=$(mktemp)
      chmod 600 "$RUNTIME_CREDENTIAL_COPY"
      if ! sudo cat "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" > "$RUNTIME_CREDENTIAL_COPY"; then
        rm -f "$RUNTIME_CREDENTIAL_COPY"
        echo "FATAL: runtime secret is not mounted" >&2
        exit 1
      fi
      eval "$(python3 - "$RUNTIME_CREDENTIAL_COPY" <<'PY'
import json, shlex, sys
d=json.load(open(sys.argv[1]))
for env,key in [('OF_MANAGER_URL','manager_url'),('OF_BOOTSTRAP','bootstrap_token'),('REDIS_URL','redis_url'),('OPENFLOWS_TENANT','redis_namespace'),('CODER_CHAT_HOOK_SECRET','hook_key'),('CODER_CHAT_HOOK_URL','hook_url')]:
    print('export %s=%s' % (env, shlex.quote(str(d[key]))))
PY
)"
      rm -f "$RUNTIME_CREDENTIAL_COPY"
      SESSION=$(curl -fsS -H 'Content-Type: application/json' --data "$(python3 -c 'import json,os; print(json.dumps({"bootstrap":os.environ["OF_BOOTSTRAP"]}))')" "$${OF_MANAGER_URL}/api/v1/runtime/register") || {
        echo "FATAL: Manager runtime registration failed. Stop and then start this tenant through OpenFlows to inject a fresh bootstrap; restarting directly in Coder reuses a single-use credential." >&2
        exit 1
      }
      eval "$(python3 - "$SESSION" <<'PY'
import json, shlex, sys
d=json.loads(sys.argv[1])
print('export OF_RUNTIME_ACCESS_TOKEN=%s' % shlex.quote(d['access_token']))
print('export OF_RUNTIME_REFRESH_TOKEN=%s' % shlex.quote(d['refresh_token']))
PY
)"
      # The controller uses this Manager runtime session for hosted
      # control-plane calls. It is intentionally distinct from Coder user
      # credentials, which are never exported into the workspace.
      export OPENFLOWS_MANAGER_URL="$OF_MANAGER_URL"
      GIT_RESPONSE=$(curl -fsS -H "Authorization: Bearer $${OF_RUNTIME_ACCESS_TOKEN}" -H 'Content-Type: application/json' --data '{"purpose":"git"}' "$${OF_MANAGER_URL}/api/v1/runtime/github-credentials") || {
        echo "FATAL: Manager GitHub credential exchange failed after runtime registration." >&2
        exit 1
      }
      GIT_TOKEN=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["token"])' "$GIT_RESPONSE")
      # The controller reports readiness after opening its tenant store.
      # A successful bootstrap alone does not mean the controller is running.
    else
      python3 /opt/openflows/connected-local-github.py setup || { echo "FATAL: connected-local GitHub authorization failed" >&2; exit 1; }
      GIT_TOKEN=$(python3 /opt/openflows/connected-local-github.py token)
    fi
    if [ -n "$GIT_TOKEN" ]; then
      git config --global credential.helper store
      echo "https://x-access-token:$${GIT_TOKEN}@github.com" > /home/coder/.git-credentials
      chmod 600 /home/coder/.git-credentials
      echo "Configured git credentials for GitHub"
    else
      echo "FATAL: No GitHub token available from the local Manager" >&2
      exit 1
    fi

    if [ "${var.connected_local}" = "true" ]; then
      git config --global --unset-all credential.helper 2>/dev/null || true
      sudo git config --system --unset-all credential.helper 2>/dev/null || true
      sudo git config --system credential.helper '!python3 /opt/openflows/connected-local-github.py credential'
      rm -f /home/coder/.git-credentials
      sudo rm -f /root/.git-credentials
    fi

    # git pull or clone (credentials from the local Manager in local mode)
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -n "${data.coder_parameter.repo_url.value}" ]; then
      git clone ${data.coder_parameter.repo_url.value} /home/coder/workspace 2>/dev/null || true
    fi

    # Maintain a golden repo checkout on the shared artifacts volume so worker
    # workspaces (forge/sentinel/vessel/lore) seed from an offline copy instead
    # of a per-spawn network clone (see docs §3.2 "Repo acquisition: the golden
    # mount clone"). Nexus owns the artifacts volume (writable here) and holds
    # the git credentials / network egress, so the expensive fetch happens once.
    # The golden tree is refreshed (fetch + reset) before any worker copies it.
    GOLDEN_REPO="/home/coder/.openflows/artifacts/repo"
    if [ -n "${data.coder_parameter.repo_url.value}" ]; then
      if [ -d "$GOLDEN_REPO/.git" ]; then
        cd "$GOLDEN_REPO" && git fetch --prune 2>/dev/null || true
        git reset --hard "origin/HEAD" 2>/dev/null || git reset --hard 2>/dev/null || true
        echo "Refreshed golden repo at $GOLDEN_REPO" >&2
      else
        mkdir -p "$(dirname "$GOLDEN_REPO")"
        sudo chown -R coder:coder /home/coder/.openflows/artifacts
        if git clone "${data.coder_parameter.repo_url.value}" "$GOLDEN_REPO" 2>/dev/null; then
          echo "Seeded golden repo at $GOLDEN_REPO" >&2
        else
          echo "WARNING: could not seed golden repo at $GOLDEN_REPO — workers will not get an offline repo seed" >&2
        fi
      fi
      sudo chown -R coder:coder "$GOLDEN_REPO" 2>/dev/null || true
    fi

    # Start the OpenFlows Controller
    # Detect Coder URL: use internal 'coder' hostname when running inside workspace,
    # otherwise use the passed-in external URL (for bootstrap/provisioning from host)
    if curl -sf http://coder:7080/api/v2/buildinfo >/dev/null 2>&1; then
      export CODER_URL="http://coder:7080"
    else
      export CODER_URL="${data.coder_parameter.coder_url.value}"
    fi
    # Hosted mode never exports the platform Coder session token or the
    # deployment-wide hook signing secret into the runtime; scoped runtime
    # credentials come from the mounted secret-injection file. Local mode keeps
    # the existing operator workflow unchanged.
    if [ "${data.coder_parameter.hosted.value}" != "true" ]; then
      export CODER_SESSION_TOKEN="${data.coder_parameter.coder_session_token.value}"
      export CODER_CHAT_HOOK_SECRET="${data.coder_parameter.coder_chat_hook_secret.value}"
      export OPENFLOWS_DOCS_GATE_SECRET="${data.coder_parameter.docs_gate_secret.value}"
      export OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS="${data.coder_parameter.docs_gate_secret_previous.value}"
    fi
    # Hosted mode already loaded the tenant-scoped Redis endpoint from the
    # mounted runtime secret above. Do not overwrite it with a Terraform
    # parameter: parameters are visible to Coder and are not the secret
    # delivery channel.
    if [ "${data.coder_parameter.hosted.value}" != "true" ]; then
      export REDIS_URL="${data.coder_parameter.redis_url.value}"
      export OPENFLOWS_REDIS_USERNAME="${data.coder_parameter.redis_username.value}"
      export OPENFLOWS_REDIS_NAMESPACE="${data.coder_parameter.redis_namespace.value}"
    fi
    # Fleet size is authoritative only for hosted runtimes, where the Manager
    # passes it as a workspace parameter. Local tenants supply their fleet
    # through OPENFLOWS_REGISTRY_JSON (tenant add --fleet N); exporting a
    # parameter that defaults to 1 here would shrink that registry fleet.
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      export OPENFLOWS_FLEET_SIZE="${data.coder_parameter.fleet_size.value}"
    fi
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export GITHUB_REPOSITORY="${data.coder_parameter.github_repository.value}"
    if [ "${data.coder_parameter.hosted.value}" != "true" ]; then
      export OPENFLOWS_REGISTRY_JSON='${data.coder_parameter.registry_json.value}'
    fi
    if [ -n "${data.coder_parameter.coder_chat_hook_url.value}" ]; then
      export CODER_CHAT_HOOK_URL="${data.coder_parameter.coder_chat_hook_url.value}"
    fi

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
  count = data.coder_workspace.me.start_count
  name  = "openflows-nexus-${data.coder_workspace.me.id}"
  image = data.coder_parameter.hosted.value == "true" ? "codercom/enterprise-base@${data.coder_parameter.workspace_image_digest.value}" : var.workspace_image
  # Match the Coder agent and dev binaries on Intel and Apple Silicon hosts.
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

  dynamic "volumes" {
    for_each = data.coder_parameter.hosted.value == "true" && data.coder_parameter.runtime_secret_ref.value != "" ? [1] : []
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
    # In connected-local mode, Agent GitHub API calls use the Manager-brokered
    # token file instead of receiving the external-auth token directly.
    # Controller resolves its GitHub token from CODER_EXTERNAL_AUTH_* (the
    # tenant owner's linked GitHub App token). Export it here: Coder injects the
    # provider token only for `git` via GIT_ASKPASS, not as a general-purpose
    # env var, so without this the controller's resolve_token() would find nothing.
    "CODER_EXTERNAL_AUTH_PRIMARY_GITHUB_ACCESS_TOKEN=${var.connected_local ? "" : data.coder_external_auth.github[0].access_token}",
  ])

  networks_advanced {
    name = var.docker_network
    # Stable service name so forge/sentinel templates can default
    # A2A_RELAY_ADDR to openflows-nexus:3000 without knowing the workspace id.
    aliases = data.coder_parameter.hosted.value == "true" ? ["openflows-nexus-${data.coder_parameter.tenant.value}"] : ["openflows-nexus"]
  }

  entrypoint = ["sh", "-c", replace(coder_agent.main.init_script, "/localhost|127\\.0\\.0\\.1/", "coder")]
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}
