# Nexus on the Kubernetes workspace backend.
#
# This is the hosted variant of the Docker template: it runs one Nexus pod and
# never mounts a host path, so a cluster without a Docker daemon can host the
# tenant. Startup is unchanged — the pod sources the same workspace runtime from
# the pinned image and calls of_bootstrap_controller — so the two backends share
# one role contract and only the infrastructure differs.
#
# The per-workspace runtime bundle arrives as the Manager-written Secret, mounted
# read-only at /run/openflows/runtime.json. The template never sees its values;
# it receives only the Secret's name (runtime_secret_ref).

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

# The namespace the organization's namespaces are created in by the Manager's
# reconciler; the template's runtime_namespace parameter overrides it when the
# Manager passes one, so the pod lands beside its own Secret.
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
  description = "CPU requested by the Nexus pod"
  type        = string
  default     = "500m"
}

variable "memory_request" {
  description = "Memory requested by the Nexus pod"
  type        = string
  default     = "1Gi"
}

variable "cpu_limit" {
  description = "CPU limit for the Nexus pod"
  type        = string
  default     = "2"
}

variable "memory_limit" {
  description = "Memory limit for the Nexus pod"
  type        = string
  default     = "4Gi"
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

data "coder_parameter" "tenant" {
  name        = "tenant"
  description = "OpenFlows tenant identifier"
  default     = ""
  type        = "string"
}

data "coder_parameter" "organization" {
  name        = "organization"
  description = "OpenFlows organization identifier, for resource labels"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "repo_url" {
  name        = "repo_url"
  description = "Git repository URL to clone into the workspace"
  default     = ""
  type        = "string"
}

data "coder_parameter" "github_repository" {
  name        = "github_repository"
  description = "GitHub repository (owner/repo) for the Controller to monitor"
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

data "coder_parameter" "coder_chat_hook_url" {
  name        = "coder_chat_hook_url"
  description = "The exact hook URL Coder is configured to POST to (CODER_CHAT_HOOK_URL)"
  default     = ""
  type        = "string"
  mutable     = false
}

data "coder_parameter" "start_controller" {
  name        = "start_controller"
  description = "Whether to auto-start the OpenFlows controller on workspace startup"
  default     = "true"
  type        = "bool"
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
  # runtime Secret there, so the pod, Service and PVC must live in the same one.
  namespace = data.coder_parameter.runtime_namespace.value != "" ? data.coder_parameter.runtime_namespace.value : var.namespace

  # Hosting contract: one Nexus workspace per tenant, addressed by the other
  # roles as `openflows-nexus-<tenant>:3000` (the Manager passes that address as
  # a2a_relay_addr to the workers).
  service_name = "openflows-nexus-${data.coder_parameter.tenant.value}"

  workspace_name = "openflows-nexus-${data.coder_workspace.me.id}"

  # The tenant label is what the Manager's per-tenant NetworkPolicy selects, so
  # pods without it cannot reach the other roles or the platform services.
  labels = {
    "app.kubernetes.io/name"     = "openflows-nexus"
    "app.kubernetes.io/instance" = data.coder_workspace.me.id
    "openflows.io/tenant"        = data.coder_parameter.tenant.value
    "openflows.io/organization"  = data.coder_parameter.organization.value
    "openflows.io/workspace"     = data.coder_workspace.me.id
  }

  # Selector labels must stay stable for the pod's lifetime, so the tenant is not
  # part of them: only the workspace instance identifies the pod.
  selector_labels = {
    "app.kubernetes.io/name"     = "openflows-nexus"
    "app.kubernetes.io/instance" = data.coder_workspace.me.id
  }

  image = "${data.coder_parameter.workspace_image_repository.value}@${data.coder_parameter.workspace_image_digest.value}"

  # Empty when the delivery adapter is not the Kubernetes one; the mount is then
  # skipped and the workspace reaches its Secret at registration time instead.
  runtime_secret_name = data.coder_parameter.runtime_secret_ref.value
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
    OF_REPO_URL="${data.coder_parameter.repo_url.value}"

    # Nexus owns the tenant's seed checkout inside its own PVC and delivers what
    # workers need through the Manager; there is no shared volume on this backend.
    of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url
    # The controller uses this Manager runtime session for hosted control-plane
    # calls; Coder user credentials never reach the workspace.
    export OPENFLOWS_MANAGER_URL="$OF_MANAGER_URL"

    export OPENFLOWS_FLEET_SIZE="${data.coder_parameter.fleet_size.value}"
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

# The workspace home persists across stop and start: it is created with the
# workspace and removed only when Coder deletes it, so a rebuild does not lose
# the seed checkout or the agent's state.
resource "kubernetes_persistent_volume_claim_v1" "workspace" {
  metadata {
    name      = local.workspace_name
    namespace = local.namespace
    labels    = local.labels
  }

  # A StorageClass with WaitForFirstConsumer only binds once a pod arrives, and
  # that pod is this workspace's Deployment. Waiting for the claim would deadlock
  # the first build, so Terraform returns while it is still pending.
  wait_until_bound = false

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

# Stable per-tenant address for the A2A relay. The workers receive
# `openflows-nexus-<tenant>:3000` from the Manager, so the name must match even
# while the Nexus pod is restarting; the selector simply has no endpoints then.
resource "kubernetes_service_v1" "relay" {
  metadata {
    name      = local.service_name
    namespace = local.namespace
    labels    = local.labels
  }

  spec {
    selector = local.selector_labels

    port {
      name        = "a2a-relay"
      port        = 3000
      target_port = 3000
      protocol    = "TCP"
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

        container {
          name  = "nexus"
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
            name  = "GITHUB_REPOSITORY"
            value = data.coder_parameter.github_repository.value
          }

          env {
            name  = "CODER_CHAT_HOOK_URL"
            value = data.coder_parameter.coder_chat_hook_url.value
          }

          env {
            name  = "ROLE"
            value = "nexus"
          }

          env {
            name  = "CODER_AGENT_TOKEN"
            value = coder_agent.main.token
          }

          # Bind the A2A relay on all interfaces so the workers reach it over the
          # tenant Service; the agent's own traffic is separate.
          env {
            name  = "A2A_RELAY_ADDR"
            value = "0.0.0.0:3000"
          }

          env {
            name  = "ARTIFACTS_DIR"
            value = "/home/coder/.openflows/artifacts"
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
    key   = "relay"
    value = "${local.service_name}:3000"
  }
}
