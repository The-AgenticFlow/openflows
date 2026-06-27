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
  description = "Git repository URL for the project under review"
}

resource "docker_volume" "workspace" {
  name = "openflows-sentinel-${data.coder_workspace.me.id}"
}

resource "docker_container" "workspace" {
  name  = "openflows-sentinel-${data.coder_workspace.me.id}"
  image = "coder/openflows-sentinel:latest"

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