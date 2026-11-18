# Forge on the Kubernetes workspace backend.
#
# The hosted variant of the Docker template: one pod per workspace, no Docker
# daemon and no host path, so a cluster without a container runtime on its nodes
# can host the role. Startup is unchanged — the pod sources the same workspace
# runtime from the pinned image and calls of_bootstrap_worker — so the two
# backends share one role contract and only the infrastructure differs.
#
# The per-workspace runtime bundle arrives as the Manager-written Secret, mounted
# read-only at /run/openflows/runtime.json. The template never sees its values;
# it receives only the Secret's name (runtime_secret_ref).
#
# A working worker clones the repository itself with a Manager-brokered token and
# installs the role assets from the pinned image, so there is no shared artifacts
# volume on this backend.

terraform {
  required_providers {
    coder      = { source = "coder/coder", version = "~> 2.18.0" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 2.35.0" }
  }
}

# The provisioner runs in the organization's namespace, so an empty config_path
# uses its in-cluster service account. A local run can point at a kubeconfig.
variable "kubeconfig_path" {
  description = "Kubeconfig for the provisioner; empty uses the in-cluster configuration"
  type        = string
  default     = ""
}

variable "namespace" {
  description = "Kubernetes namespace for the workspace resources"
  type        = string
  default     = "openflows"
}

variable "coder_service_url" {
  description = "Coder server URL reachable from inside the cluster"
  type        = string
  default     = "http://coder.coder.svc.cluster.local:7080"
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

variable "storage_class_name" {
  description = "StorageClass for the workspace PVC; empty uses the cluster default"
  type        = string
  default     = ""
}

variable "storage_size" {
  description = "Size of the workspace PVC"
  type        = string
  default     = "20Gi"
}

variable "cpu_request" {
  description = "CPU requested by the forge pod"
  type        = string
  default     = "1"
}

variable "memory_request" {
  description = "Memory requested by the forge pod"
  type        = string
  default     = "2Gi"
}

variable "cpu_limit" {
  description = "CPU limit for the forge pod"
  type        = string
  default     = "4"
}

variable "memory_limit" {
  description = "Memory limit for the forge pod"
  type        = string
  default     = "8Gi"
}

provider "kubernetes" {
  config_path = var.kubeconfig_path
}

# Workspace-level parameters (set per-workspace via Coder API rich_parameter_values).
data "coder_parameter" "runtime_namespace" {
  name        = "runtime_namespace"
  description = "Namespace the Manager delivers this workspace's runtime Secret to; empty uses the template's namespace"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "runtime_secret_ref" {
  name        = "runtime_secret_ref"
  description = "Provisioner-resolved per-workspace secret reference: the Kubernetes Secret name"
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

data "coder_parameter" "organization" {
  name        = "organization"
  description = "OpenFlows organization identifier, for resource labels"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "tenant" {
  name        = "tenant"
  description = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "role" {
  name        = "role"
  description = "Agent role name"
  default     = "forge"
  type        = "string"
}

data "coder_parameter" "ticket_id" {
  name        = "ticket_id"
  description = "Ticket identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "repo_url" {
  name        = "repo_url"
  description = "Git repository URL to clone into the workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "branch" {
  name        = "branch"
  description = "Remote branch to check out (the PR branch for an existing PR). Empty = create or resume the worker/ticket branch."
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

data "coder_parameter" "a2a_relay_addr" {
  name        = "a2a_relay_addr"
  description = "Address of the tenant's Nexus A2A relay"
  default     = "openflows-nexus:3000"
  type        = "string"
}

# Declared so the Manager's hosted parameters are accepted. A Kubernetes
# workspace always runs hosted, so its value is informational here.
data "coder_parameter" "hosted" {
  name        = "hosted"
  description = "Whether this workspace is provisioned in hosted mode (always true on this backend)"
  default     = "true"
  type        = "bool"
  mutable     = false
}

data "coder_workspace" "me" {}
data "coder_workspace_owner" "me" {}

locals {
  # One namespace per organization. The Manager's delivery identity writes the
  # runtime Secret there, so the pod must live in the same one.
  namespace = data.coder_parameter.runtime_namespace.value != "" ? data.coder_parameter.runtime_namespace.value : var.namespace

  workspace_name = "openflows-${data.coder_parameter.role.value}-${data.coder_workspace.me.id}"

  # The tenant label is what the Manager's per-tenant NetworkPolicy selects, so
  # pods without it cannot reach the other roles or the platform services.
  labels = {
    "app.kubernetes.io/name"     = "openflows-role"
    "app.kubernetes.io/instance" = data.coder_workspace.me.id
    "openflows.io/tenant"        = data.coder_parameter.tenant.value
    "openflows.io/organization"  = data.coder_parameter.organization.value
    "openflows.io/workspace"     = data.coder_workspace.me.id
    "openflows.io/role"          = "forge"
  }

  # Selector labels must stay stable for the pod's lifetime.
  selector_labels = {
    "app.kubernetes.io/instance" = data.coder_workspace.me.id
  }

  image = "${data.coder_parameter.workspace_image_repository.value}@${data.coder_parameter.workspace_image_digest.value}"

  # Empty when the delivery adapter is not the Kubernetes one; the mount is then
  # skipped and the workspace has no bundle to read.
  runtime_secret_name = data.coder_parameter.runtime_secret_ref.value
}

# The requested branch is interpolated directly into the startup bash script.
# A branch name containing shell metacharacters (e.g. `$(...)`) would otherwise
# be evaluated by bash as command substitution, so validate it against a charset
# that accepts all git-valid, shell-safe characters and fall back to the default
# branch when it does not match.
locals {
  requested_branch = data.coder_parameter.branch.value
  safe_branch      = can(regex("^[A-Za-z0-9._/+@=-]+$", local.requested_branch)) ? local.requested_branch : ""
}


resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"

  startup_script = <<-EOT
    #!/bin/bash
    set -e

    # Shared startup from the workspace runtime (runtime/lib/bootstrap.sh in the
    # workspace image). The template and the runtime must agree on the runtime API.
    OF_RUNTIME_LIB=/opt/openflows/runtime/lib/bootstrap.sh
    . "$OF_RUNTIME_LIB" || { echo "FATAL: cannot load the workspace runtime from $OF_RUNTIME_LIB" >&2; exit 1; }
    [ "$${OF_RUNTIME_API:-}" = 1 ] || { echo "FATAL: $OF_RUNTIME_LIB provides workspace runtime API '$${OF_RUNTIME_API:-none}' but this template needs 1; use the workspace image of the templates' release" >&2; exit 1; }
    # A Kubernetes workspace is hosted by definition: its credentials come from
    # the mounted Secret and the Manager runtime session, never from parameters
    # or platform-wide credentials.
    OF_HOSTED=true
    OF_CONNECTED_LOCAL=false
    OF_RUNTIME_SECRET_REF="${data.coder_parameter.runtime_secret_ref.value}"

    # The worker id (forge-1) names the task branch; the base role (forge)
    # selects the role assets and the controller's Redis keys.
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
    export OPENFLOWS_TENANT="${data.coder_parameter.tenant.value}"
    export OPENFLOWS_TICKET="$TICKET_ID"
    export OPENFLOWS_ROLE="$ROLE_BASE"
    export A2A_RELAY_ADDR="${data.coder_parameter.a2a_relay_addr.value}"
    export A2A_PAIR_TOKEN="${data.coder_parameter.a2a_pair_token.value}"
    export CODER_WORKSPACE_ID="${data.coder_workspace.me.id}"

    # The heartbeat daemon is the only Redis client in the workspace.
    nohup openflows-harness heartbeat start >/dev/null 2>&1 &
    log "Heartbeat daemon started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # The A2A verify executor (issue #143) runs delegated verification in
    # temporary checkouts, supervised outside the agent chat so Redis/relay
    # startup races retry. Diagnostics stay outside the checkout.
    nohup bash -c 'while true; do openflows-harness verify serve; sleep 5; done' \
      >>/home/coder/.local/state/openflows/verify.log 2>&1 &
    log "Verify executor started (role=$ROLE_BASE ticket=$OPENFLOWS_TICKET)"

    # Hosted Forge works its ticket through a Coder Chat that Nexus creates, so
    # no agent binary is started here.
    log "Hosted Forge execution is started by Nexus through a Manager-created Coder Chat"

  EOT
}

# The workspace home persists across stop and start: it is created with the
# workspace and removed only when Coder deletes it, so a rebuild does not lose
# the checkout or the agent's state.
resource "kubernetes_persistent_volume_claim_v1" "workspace" {
  # A StorageClass with WaitForFirstConsumer only binds once a pod arrives, and
  # that pod is this workspace's Deployment. Waiting for the claim would deadlock
  # the first build, so Terraform returns while it is still pending.
  wait_until_bound = false

  metadata {
    name      = local.workspace_name
    namespace = local.namespace
    labels    = local.labels
  }

  spec {
    access_modes = ["ReadWriteOnce"]

    # Empty selects the cluster's default class; a literal "" would disable
    # dynamic provisioning instead.
    storage_class_name = var.storage_class_name != "" ? var.storage_class_name : null

    resources {
      requests = {
        storage = var.storage_size
      }
    }
  }
}

resource "kubernetes_deployment_v1" "workspace" {
  count = data.coder_workspace.me.start_count

  # The agent reports readiness to the Manager, not to a container probe, so a
  # slow first start must not fail the apply.
  wait_for_rollout = false

  metadata {
    name      = local.workspace_name
    namespace = local.namespace
    labels    = local.labels
  }

  spec {
    replicas = "1"

    # One pod owns the ReadWriteOnce home, so a replacement must never overlap the
    # pod it replaces: the default rolling update starts the new one first, which
    # either blocks on the volume (another node) or writes the same checkout (same
    # node). stop-then-start is the only safe order here.
    strategy {
      type = "Recreate"
    }

    selector {
      match_labels = local.selector_labels
    }

    template {
      metadata {
        labels = local.labels
      }

      spec {
        # The namespace enforces the restricted Pod Security Standard: no root,
        # no privilege escalation, no service-account token, no host mounts.
        automount_service_account_token = false

        security_context {
          run_as_non_root = true
          run_as_user     = 1000
          run_as_group    = 1000
          # The PVC is created root-owned; fsGroup hands it to the workspace user
          # so startup needs no chown and no sudo.
          fs_group = 1000

          seccomp_profile {
            type = "RuntimeDefault"
          }
        }

        # The workspace image is published for linux/amd64 only.
        node_selector = {
          "kubernetes.io/arch" = "amd64"
        }

        # The image WORKDIR is /home/coder/workspace, and the home PVC hides the image copy.
        # Create it as uid 1000 first, or the runtime makes it root-owned and the clone fails.
        init_container {
          name        = "prepare-workspace"
          image       = local.image
          working_dir = "/tmp"
          command     = ["mkdir", "-p", "/home/coder/workspace"]

          security_context {
            allow_privilege_escalation = false
            run_as_non_root            = true
            run_as_user                = 1000

            capabilities {
              drop = ["ALL"]
            }
          }

          resources {
            requests = {
              cpu    = "10m"
              memory = "16Mi"
            }

            limits = {
              cpu    = "100m"
              memory = "64Mi"
            }
          }

          volume_mount {
            name       = "home"
            mount_path = "/home/coder"
          }
        }

        container {
          name  = "forge"
          image = local.image

          command = ["sh", "-c", coder_agent.main.init_script]

          security_context {
            allow_privilege_escalation = false
            run_as_non_root            = true
            run_as_user                = 1000

            capabilities {
              drop = ["ALL"]
            }
          }

          env {
            name  = "GITHUB_API_BASE"
            value = var.github_api_base
          }

          env {
            name  = "GITHUB_GIT_BASE"
            value = var.github_git_base
          }

          env {
            name  = "CODER_URL"
            value = var.coder_service_url
          }

          env {
            name  = "OPENFLOWS_RUNTIME_CREDENTIAL_FILE"
            value = "/run/openflows/runtime.json"
          }

          env {
            name  = "OPENFLOWS_GITHUB_API_TOKEN_FILE"
            value = "/home/coder/.config/openflows/github-api-token"
          }

          env {
            name  = "OPENFLOWS_TENANT"
            value = data.coder_parameter.tenant.value
          }

          env {
            name  = "OPENFLOWS_TICKET"
            value = data.coder_parameter.ticket_id.value
          }

          env {
            name  = "OPENFLOWS_ROLE"
            value = replace(data.coder_parameter.role.value, "/-[0-9]+$/", "")
          }

          env {
            name  = "A2A_RELAY_ADDR"
            value = data.coder_parameter.a2a_relay_addr.value
          }

          env {
            name  = "A2A_PAIR_TOKEN"
            value = data.coder_parameter.a2a_pair_token.value
          }

          env {
            name  = "CODER_WORKSPACE_ID"
            value = data.coder_workspace.me.id
          }

          env {
            name  = "CODER_AGENT_TOKEN"
            value = coder_agent.main.token
          }

          resources {
            requests = {
              cpu    = var.cpu_request
              memory = var.memory_request
            }

            limits = {
              cpu    = var.cpu_limit
              memory = var.memory_limit
            }
          }

          volume_mount {
            name       = "home"
            mount_path = "/home/coder"
          }

          dynamic "volume_mount" {
            for_each = local.runtime_secret_name != "" ? [1] : []
            content {
              name       = "runtime"
              mount_path = "/run/openflows"
              read_only  = true
            }
          }
        }

        volume {
          name = "home"

          persistent_volume_claim {
            claim_name = kubernetes_persistent_volume_claim_v1.workspace.metadata[0].name
          }
        }

        # The bundle is the Secret's `runtime.json` key; 0440 with fsGroup makes
        # it readable by the workspace user without sudo.
        dynamic "volume" {
          for_each = local.runtime_secret_name != "" ? [1] : []
          content {
            name = "runtime"

            secret {
              secret_name  = local.runtime_secret_name
              default_mode = "0440"

              items {
                key  = "runtime.json"
                path = "runtime.json"
              }
            }
          }
        }
      }
    }
  }
}

resource "coder_metadata" "workspace" {
  count       = data.coder_workspace.me.start_count
  resource_id = kubernetes_deployment_v1.workspace[count.index].id

  item {
    key   = "namespace"
    value = local.namespace
  }

  item {
    key   = "role"
    value = data.coder_parameter.role.value
  }
}
