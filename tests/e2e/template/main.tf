terraform {
  required_providers {
    coder  = { source = "coder/coder", version = "2.18.0" }
    docker = { source = "kreuzwerker/docker", version = "4.5.0" }
  }
}

variable "dev_binary_host_path" {
  # Reuse the client's existing template-variable transport to pass the unique
  # compose project name, which identifies this run's image and network.
  type = string
}

data "coder_workspace" "me" {}
data "coder_parameter" "tenant" {
  name    = "tenant"
  type    = "string"
  default = "ci"
}
data "coder_parameter" "role" {
  name    = "role"
  type    = "string"
  default = "forge"
}
data "coder_parameter" "a2a_relay_addr" {
  name    = "a2a_relay_addr"
  type    = "string"
  default = "127.0.0.1:3000"
}
data "coder_parameter" "a2a_pair_token" {
  name    = "a2a_pair_token"
  type    = "string"
  default = "unused-by-worker-lifecycle-test"
}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"
  env = {
    REDIS_URL          = "redis://redis:6379"
    OPENFLOWS_TENANT   = data.coder_parameter.tenant.value
    OPENFLOWS_TICKET   = "T-1"
    OPENFLOWS_ROLE     = data.coder_parameter.role.value
    A2A_RELAY_ADDR     = data.coder_parameter.a2a_relay_addr.value
    A2A_PAIR_TOKEN     = data.coder_parameter.a2a_pair_token.value
    CODER_WORKSPACE_ID = data.coder_workspace.me.id
  }
  startup_script = <<-EOT
    #!/bin/bash
    set -euo pipefail
    cd /home/coder/workspace
    git init
    git config user.email ci@example.test
    git config user.name CI
    git commit --allow-empty -m 'CI seed'
    printf '# CI plan\n\nExercise real worker coordination.\n' > /tmp/plan.md
    printf 'CI review evidence\n' > /tmp/review.md
    openflows-harness --help >/dev/null
  EOT
}

resource "docker_container" "worker" {
  count = data.coder_workspace.me.start_count
  name  = "${var.dev_binary_host_path}-worker-${data.coder_workspace.me.id}"
  image = "${var.dev_binary_host_path}-worker:ci"
  labels {
    label = "openflows.ci.project"
    value = var.dev_binary_host_path
  }
  networks_advanced {
    name = "${var.dev_binary_host_path}_default"
  }
  env = [
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    "CODER_AGENT_URL=http://coder:7080",
  ]
  entrypoint = ["sh", "-c", coder_agent.main.init_script]
}
