terraform {
  required_providers {
    coder = { source = "coder/coder" }
    docker = { source = "kreuzwerker/docker" }
  }
}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
  dir  = "/home/coder/workspace"
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
  image = "coder/openflows-forge:latest"

  volumes {
    container_path = "/home/coder/workspace"
    volume_name    = docker_volume.workspace.name
  }

  env = [
    "CODER_AGENT_TOKEN=${coder_agent.main.token}",
    "REPO_URL=${var.repo_url}",
  ]
}

data "coder_workspace" "me" {}