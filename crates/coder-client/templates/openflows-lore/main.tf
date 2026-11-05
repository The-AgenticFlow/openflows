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

variable "harness_version" {
  type        = string
  default     = "harness-edge"
  description = "openflows-harness binary version to download. Use 'harness-edge' for the latest main-branch build, or a specific version tag (e.g. 'v1.2.0')."
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

    log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" >&2; }

    # Setup git credentials from Coder external auth (the tenant's linked
    # GitHub App token). PAT flow removed — external auth is the sole source.
    mkdir -p /home/coder/.local/state/openflows
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      test -r "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" || { log "FATAL: runtime secret is not mounted"; exit 1; }
      eval "$(python3 - "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" <<'PY'
import json, shlex, sys
d=json.load(open(sys.argv[1]))
for env,key in [('OF_MANAGER_URL','manager_url'),('OF_BOOTSTRAP','bootstrap_token'),('REDIS_URL','redis_url'),('OPENFLOWS_TENANT','redis_namespace')]: print('export %s=%s' % (env,shlex.quote(str(d[key]))))
PY
)"
      SESSION=$(curl -fsS -H 'Content-Type: application/json' --data "$(python3 -c 'import json,os; print(json.dumps({"bootstrap":os.environ["OF_BOOTSTRAP"]}))')" "$${OF_MANAGER_URL}/api/v1/runtime/register")
      OF_RUNTIME_ACCESS_TOKEN=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["access_token"])' "$SESSION"); export OF_RUNTIME_ACCESS_TOKEN
      OF_RUNTIME_REFRESH_TOKEN=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["refresh_token"])' "$SESSION"); export OF_RUNTIME_REFRESH_TOKEN
      GIT_RESPONSE=$(curl -fsS -H "Authorization: Bearer $${OF_RUNTIME_ACCESS_TOKEN}" -H 'Content-Type: application/json' --data '{"purpose":"git"}' "$${OF_MANAGER_URL}/api/v1/runtime/github-credentials")
      GIT_TOKEN=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["token"])' "$GIT_RESPONSE")
    else
      if [ "${var.connected_local}" = "true" ]; then
        python3 /opt/openflows/connected-local-github.py setup || { echo "FATAL: connected-local GitHub authorization failed" >&2; exit 1; }
        GIT_TOKEN=$(python3 /opt/openflows/connected-local-github.py token)
      else
        GIT_TOKEN="${data.coder_parameter.hosted.value == "true" || var.connected_local ? "" : data.coder_external_auth.github[0].access_token}"
      fi
    fi
    if [ -n "$GIT_TOKEN" ]; then
      # System-level helper so git works for BOTH the agent user (pushes) and
      # the sudo'd root clone below (their $HOME differ).
      sudo git config --system credential.helper store
      echo "https://x-access-token:$${GIT_TOKEN}@github.com" > /home/coder/.git-credentials
      chmod 600 /home/coder/.git-credentials
      sudo mkdir -p /root
      sudo cp /home/coder/.git-credentials /root/.git-credentials
      sudo chmod 600 /root/.git-credentials
      log "Configured git credentials for GitHub push auth"
    else
      log "WARNING: No GitHub token available — open this workspace and click 'Login with GitHub' (external auth) to grant repo access"
    fi

    if [ "${var.connected_local}" = "true" ]; then
      git config --global --unset-all credential.helper 2>/dev/null || true
      sudo git config --system --unset-all credential.helper 2>/dev/null || true
      sudo git config --system credential.helper '!python3 /opt/openflows/connected-local-github.py credential'
      rm -f /home/coder/.git-credentials
      sudo rm -f /root/.git-credentials
    fi

    # LORE commits and pushes its docs branch, so it needs an author identity.
    # Resolve it exactly as FORGE does: hosted installation tokens commit as
    # the App bot; otherwise use the inherited GitHub account.
    # Without a token yet (first run before "Login with GitHub"), keep booting:
    # the repository step below reports the missing access, and the identity
    # is set on the next start once the token exists.
    EXPECTED_LOGIN=""
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      EXPECTED_LOGIN="openflows[bot]"
    elif [ -n "$GIT_TOKEN" ]; then
      # Retry transient GitHub failures (outage, rate limit) before giving up,
      # so one blip does not fail the workspace; NEXUS parks LORE if it keeps
      # failing to come up.
      for attempt in 1 2 3 4 5; do
        EXPECTED_LOGIN=$(curl -fsSL --max-time 20 -H "Authorization: token $GIT_TOKEN" "$${GITHUB_API_BASE:-https://api.github.com}/user" \
          | python3 -c 'import sys,json;print(json.load(sys.stdin)["login"])') && break
        EXPECTED_LOGIN=""
        log "GitHub identity lookup failed (attempt $attempt/5); retrying"
        sleep $((attempt * 5))
      done
      if [ -z "$EXPECTED_LOGIN" ]; then
        log "FATAL: could not resolve the inherited GitHub identity from the external-auth token"
        exit 1
      fi
    fi
    if [ -n "$EXPECTED_LOGIN" ]; then
      log "GitHub identity resolved: $EXPECTED_LOGIN"
      git config --global user.name "$EXPECTED_LOGIN"
      git config --global user.email "$EXPECTED_LOGIN@users.noreply.github.com"
      sudo -H git config --global user.name "$EXPECTED_LOGIN"
      sudo -H git config --global user.email "$EXPECTED_LOGIN@users.noreply.github.com"
    else
      log "WARNING: no GitHub token yet — git author identity not configured; LORE cannot push until 'Login with GitHub' is completed"
    fi

    # Acquire the repository. Prefer the golden offline seed from the shared
    # artifacts volume (docs §3.2): copy -> refresh -> checkout -> work.
    # Fall back to a direct network clone when no golden seed exists, and fail
    # LOUDLY (WARNING) rather than silently leaving an empty workspace.
    GOLDEN_REPO="/home/coder/.openflows/artifacts/repo"
    # The fresh workspace volume is root-owned initially, so any copy/clone
    # into /home/coder/workspace must run via sudo (then be chowned back).
    if [ -d /home/coder/workspace/.git ]; then
      cd /home/coder/workspace && git pull 2>/dev/null || true
    elif [ -d "$GOLDEN_REPO/.git" ]; then
      sudo cp -a "$GOLDEN_REPO/." /home/coder/workspace/ 2>/dev/null || true
      sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
      log "Copied golden repo seed into /home/coder/workspace"
      # Refresh to latest before checkout (design: copy -> refresh -> checkout).
      cd /home/coder/workspace
      git fetch --all --prune 2>/dev/null || true
      git checkout origin/HEAD 2>/dev/null || git checkout main 2>/dev/null || true
    elif [ -n "${local.repo_url}" ]; then
      log "WARNING: no golden repo seed at $GOLDEN_REPO — falling back to direct clone"
      if sudo git clone ${local.repo_url} /home/coder/workspace 2>/tmp/lore_clone_err.log; then
        sudo chown -R coder:coder /home/coder/workspace 2>/dev/null || true
        log "Cloned repository into /home/coder/workspace"
      else
        log "WARNING: git clone failed — $(tail -5 /tmp/lore_clone_err.log 2>/dev/null)"
      fi
    else
      log "WARNING: no repo_url and no golden repo seed — workspace has no repository"
    fi

    # Download and install openflows-harness from GitHub releases.
    # The harness is REQUIRED for session coordination (dispatch/status/heartbeat,
    # A2A verify requests), so a missing harness must fail loudly — a silently
    # uncoordinated workspace is worse than a startup error.
    # TEMPORARY: Use mounted dev binaries for local testing when available.
    HARNESS_BIN="/usr/local/bin/openflows-harness"
    if [ -f /opt/openflows-dev/openflows-harness ]; then
      sudo cp /opt/openflows-dev/openflows-harness "$HARNESS_BIN"
      sudo chmod +x "$HARNESS_BIN"
    else
      # Asset naming: both harness-edge and tagged releases use fixed basename
      HARNESS_ASSET="openflows-harness-x86_64-unknown-linux-musl.tar.gz"
      if [ "${var.harness_version}" = "harness-edge" ]; then
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/harness-edge/$${HARNESS_ASSET}"
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Downloading openflows-harness (harness-edge/latest build)..." >&2
      else
        HARNESS_URL="https://github.com/The-AgenticFlow/openflows/releases/download/${var.harness_version}/$${HARNESS_ASSET}"
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Downloading openflows-harness v${var.harness_version}..." >&2
      fi
      for attempt in 1 2 3; do
        if curl -fsSL --retry 3 "$HARNESS_URL" -o /tmp/openflows-harness; then
          tar -xzf /tmp/openflows-harness -C /tmp/ || { echo "FATAL: failed to extract harness tarball" >&2; exit 1; }
          HARNESS_DIR=$(find /tmp/ -maxdepth 1 -type d -name "openflows-harness-*" 2>/dev/null | head -1)
          if [ -n "$HARNESS_DIR" ] && [ -f "$HARNESS_DIR/openflows-harness" ]; then
            sudo mv "$HARNESS_DIR/openflows-harness" "$HARNESS_BIN"
            sudo chmod +x "$HARNESS_BIN"
            rm -rf /tmp/openflows-harness "$HARNESS_DIR"
          else
            echo "FATAL: could not find harness binary in extracted tarball" >&2; exit 1
          fi
          break
        fi
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Harness download attempt $attempt failed; retrying in 5s..." >&2
        sleep 5
      done
    fi
    if [ ! -x "$HARNESS_BIN" ]; then
      echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] FATAL: openflows-harness could not be installed — agent will not be able to coordinate; failing startup" >&2
      exit 1
    fi

    # Broker-backed git credential refresh. The startup installation token is
    # short-lived, so a long-running worker's push can outlive it. Refresh the
    # runtime session and re-fetch a fresh installation token from the Manager
    # broker, then rewrite the stored git credentials so pushes keep
    # authenticating mid-task. Runs in the background for the workspace
    # lifetime; harmless when the Manager is unreachable.
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      rm -f /tmp/openflows-manager-ready
      nohup bash -c '
        ACCESS="$${OF_RUNTIME_ACCESS_TOKEN}"
        REFRESH="$${OF_RUNTIME_REFRESH_TOKEN}"
        TICKS=0
        while true; do
          sleep 30
          if [ ! -f /tmp/openflows-manager-ready ]; then continue; fi
          TICKS=$((TICKS + 1))
          HEARTBEAT_STATUS=$(curl -s -o /dev/null -w "%%{http_code}" -X POST -H "Authorization: Bearer $${ACCESS}" "$${OF_MANAGER_URL}/api/v1/runtime/heartbeat")
          if [ "$${HEARTBEAT_STATUS}" = "401" ]; then
            RESP=$(curl -fsS -H "Authorization: Bearer $${REFRESH}" -H "Content-Type: application/json" --data "{\"refresh_token\":\"$${REFRESH}\"}" "$${OF_MANAGER_URL}/api/v1/runtime/refresh" 2>/dev/null) || continue
            ACCESS=$(printf "%s" "$${RESP}" | python3 -c "import json,sys;print(json.load(sys.stdin)[\"access_token\"])")
            REFRESH=$(printf "%s" "$${RESP}" | python3 -c "import json,sys;print(json.load(sys.stdin)[\"refresh_token\"])")
            curl -fsS -X POST -H "Authorization: Bearer $${ACCESS}" "$${OF_MANAGER_URL}/api/v1/runtime/heartbeat" >/dev/null || true
          fi
          if [ "$TICKS" -lt 16 ]; then continue; fi
          TICKS=0
          RESP=$(curl -fsS -H "Authorization: Bearer $${REFRESH}" -H "Content-Type: application/json" --data "{\"refresh_token\":\"$${REFRESH}\"}" "$${OF_MANAGER_URL}/api/v1/runtime/refresh" 2>/dev/null) || continue
          ACCESS=$(printf "%s" "$${RESP}" | python3 -c "import json,sys;print(json.load(sys.stdin)[\"access_token\"])")
          REFRESH=$(printf "%s" "$${RESP}" | python3 -c "import json,sys;print(json.load(sys.stdin)[\"refresh_token\"])")
          GIT=$(curl -fsS -H "Authorization: Bearer $${ACCESS}" -H "Content-Type: application/json" --data "{\"purpose\":\"git\"}" "$${OF_MANAGER_URL}/api/v1/runtime/github-credentials" 2>/dev/null) || continue
          TOKEN=$(printf "%s" "$${GIT}" | python3 -c "import json,sys;print(json.load(sys.stdin)[\"token\"])")
          echo "https://x-access-token:$${TOKEN}@github.com" > /home/coder/.git-credentials
          sudo cp /home/coder/.git-credentials /root/.git-credentials
          sudo chmod 600 /root/.git-credentials
        done
      ' >>/home/coder/.local/state/openflows/git-refresh.log 2>&1 &
    fi

    if [ ! -d /home/coder/workspace/.git ]; then
      log "FATAL: repository bootstrap did not produce /home/coder/workspace/.git"
      exit 1
    fi

    INSTRUCTION_ROOT="/home/coder/.openflows/artifacts/orchestration"
    PERSONA="$INSTRUCTION_ROOT/agent/agents/${local.role}.agent.md"
    if [ ! -f "$PERSONA" ]; then
      log "FATAL: hosted instruction bundle is missing $PERSONA"
      exit 1
    fi
    sudo cp "$PERSONA" /home/coder/workspace/AGENTS.md
    sudo cp "$PERSONA" "/home/coder/workspace/${local.role}.agent.md"
    sudo mkdir -p /home/coder/workspace/.agents/skills /home/coder/workspace/.agents/commands
    SKILL_MANIFEST="$INSTRUCTION_ROOT/agent/roles/${local.role}.skills"
    if [ ! -f "$SKILL_MANIFEST" ]; then
      log "FATAL: hosted skill manifest is missing $SKILL_MANIFEST"
      exit 1
    fi
    while IFS= read -r skill; do
      [ -z "$skill" ] && continue
      sudo mkdir -p "/home/coder/workspace/.agents/skills/$skill"
      sudo cp "$INSTRUCTION_ROOT/plugin/skills/$skill/SKILL.md" "/home/coder/workspace/.agents/skills/$skill/SKILL.md"
    done < "$SKILL_MANIFEST"
    sudo cp -a "$INSTRUCTION_ROOT/plugin/commands/." /home/coder/workspace/.agents/commands/
    for standard in CODING.md SECURITY.md REVIEW.md; do
      [ ! -f "$INSTRUCTION_ROOT/agent/standards/$standard" ] || sudo cp "$INSTRUCTION_ROOT/agent/standards/$standard" /home/coder/workspace/
    done
    sudo chown -R coder:coder /home/coder/workspace
    EXCLUDE=$(git -C /home/coder/workspace rev-parse --absolute-git-dir)/info/exclude
    sudo mkdir -p "$(dirname "$EXCLUDE")"
    sudo touch "$EXCLUDE"
    for pattern in /AGENTS.md "/${local.role}.agent.md" '/.agents/skills/' '/.agents/commands/' /CODING.md /SECURITY.md /REVIEW.md; do
      sudo grep -qxF -- "$pattern" "$EXCLUDE" || printf '\n%s\n' "$pattern" | sudo tee -a "$EXCLUDE" >/dev/null
    done
    log "Installed hosted instructions for ${local.role}"

    # Start heartbeat only after repository bootstrap completes. Manager uses
    # this as the readiness boundary before assigning work.
    # Start heartbeat daemon (the ONLY Redis client in the workspace)
    if [ "${data.coder_parameter.hosted.value}" != "true" ]; then
      export REDIS_URL="${var.redis_url}"
      export OPENFLOWS_TENANT="${local.tenant}"
    fi
    export OPENFLOWS_TICKET="${local.ticket_id}"
    export OPENFLOWS_ROLE="${local.role}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"
    if [ "${data.coder_parameter.hosted.value}" = "true" ]; then
      curl -fsS -X POST -H "Authorization: Bearer $${OF_RUNTIME_ACCESS_TOKEN}" "$${OF_MANAGER_URL}/api/v1/runtime/heartbeat" >/dev/null
      log "Manager readiness heartbeat sent after bootstrap"
      touch /tmp/openflows-manager-ready
    fi
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
  EOT
}

resource "docker_volume" "workspace" {
  name = "openflows-${local.role}-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  count = data.coder_workspace.me.start_count
  name  = "openflows-${local.role}-${data.coder_workspace.me.id}"
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

  # Mount shared artifact files (repo golden seed, plans, skills).
  # This volume is created by the Nexus workspace and is read-only here.
  volumes {
    container_path = "/home/coder/.openflows/artifacts"
    volume_name    = "openflows-artifacts-${local.tenant}"
    read_only      = true
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
    "CODER_EXTERNAL_AUTH_PRIMARY_GITHUB_TOKEN_FILE=/home/coder/.config/openflows/github-api-token",
    "OPENFLOWS_TENANT=${local.tenant}",
    "OPENFLOWS_TICKET=${local.ticket_id}",
    "OPENFLOWS_ROLE=${local.role}",
    "CODER_WORKSPACE_ID=${data.coder_workspace.me.id}",
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    # Lore resolves its GitHub token from CODER_EXTERNAL_AUTH_* (the tenant
    # owner's linked GitHub App token). Export it here — Coder only injects the
    # provider token into `git` via GIT_ASKPASS, not as a general env var.
  ], data.coder_parameter.hosted.value == "true" ? [] : [
    "REDIS_URL=${var.redis_url}",
    "CODER_EXTERNAL_AUTH_PRIMARY_GITHUB_ACCESS_TOKEN=${var.connected_local ? "" : data.coder_external_auth.github[0].access_token}",
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
data "coder_external_auth" "github" {
  count = data.coder_parameter.hosted.value == "true" || var.connected_local ? 0 : 1
  id       = "primary-github"
  optional = true
}
