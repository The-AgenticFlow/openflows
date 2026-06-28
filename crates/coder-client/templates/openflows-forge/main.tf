terraform {
  required_providers {
    coder = { source = "coder/coder" }
    docker = { source = "kreuzwerker/docker" }
  }
}

resource "coder_agent" "main" {
  os         = "linux"
  arch       = "amd64"
  dir        = "/home/coder/workspace"
  access_url = "http://coder:7080"
}

variable "repo_url" {
  default     = ""
  description = "Git repository URL to clone into the workspace"
}

resource "docker_volume" "workspace" {
  name = "openflows-forge-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-forge-${data.coder_workspace.me.id}"
  image = "codercom/enterprise-base:ubuntu"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  env = [
    "REPO_URL=${var.repo_url}",
  ]

  # Connect to the openflows_default compose network so the init_script
  # can download the agent binary from the Coder server by service name.
  networks_advanced {
    name = "openflows_default"
  }

  # Run the Coder agent init script which downloads the agent binary and
  # starts it as a foreground process. This keeps the container alive and
  # enables SSH/workspace exec operations.
  entrypoint = ["sh", "-c"]
  command = [coder_agent.main.init_script]
}

data "coder_workspace" "me" {}

data "coder_workspace_owner" "me" {}