# Copy into either worker template's tests/ directory and run terraform test.
mock_provider "coder" {
  mock_data "coder_workspace" {
    defaults = { id = "test-workspace" }
  }
  mock_resource "coder_agent" {
    defaults = { init_script = "exec sleep infinity", token = "test-token" }
  }
}
mock_provider "docker" {}

run "standard_is_unchanged" {
  command = plan
  assert {
    condition     = docker_container.workspace.image == "codercom/enterprise-base:ubuntu" && length(docker_volume.engine) == 0
    error_message = "Standard workspaces must not provision a private engine."
  }
}

run "sysbox_private_engine" {
  command = plan
  variables {
    worker_runtime = "sysbox"
    worker_image   = "openflows-worker:test"
  }
  assert {
    condition     = docker_container.workspace.runtime == "sysbox-runc" && !docker_container.workspace.privileged
    error_message = "Sysbox workers must use the isolated runtime without privileged mode."
  }
  assert {
    condition     = docker_container.workspace.image == "openflows-worker:test" && docker_container.workspace.entrypoint[0] == "/usr/local/bin/openflows-worker-entrypoint"
    error_message = "Sysbox must use the configured image and supervised entrypoint."
  }
  assert {
    condition     = length(docker_volume.engine) == 1 && docker_volume.engine[0].name == "openflows-engine-test-workspace"
    error_message = "Each workspace must own its engine storage."
  }
  assert {
    condition     = alltrue([for volume in docker_container.workspace.volumes : volume.container_path != "/var/run/docker.sock"])
    error_message = "Never mount a Docker socket into a worker."
  }
  assert {
    condition     = contains(docker_container.workspace.env, "DOCKER_HOST=unix:///var/run/docker.sock") && contains(docker_container.workspace.env, "TESTCONTAINERS_HOST_OVERRIDE=localhost")
    error_message = "Agent and executor must discover the local engine and published test ports."
  }
}

run "sysbox_requires_image" {
  command = plan
  variables {
    worker_runtime = "sysbox"
    worker_image   = ""
  }
  expect_failures = [docker_container.workspace]
}

run "reject_unknown_runtime" {
  command = plan
  variables {
    worker_runtime = "privileged"
  }
  expect_failures = [var.worker_runtime]
}
