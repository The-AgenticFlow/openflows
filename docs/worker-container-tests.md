# Container-backed tests in worker workspaces

FORGE and SENTINEL can opt into a private Docker daemon using Sysbox. The
default `standard` mode retains the existing workspace image and does not
enable container-backed testing. This feature requires a supported **Linux
amd64 Docker host with Sysbox installed**. Docker Desktop on macOS is not a
supported host for this option.

## Host setup and activation

Install and register `sysbox-runc` following the upstream
[Sysbox installation guide](https://github.com/nestybox/sysbox/tree/master/docs/user-guide).
Check the supported kernel, distribution, storage driver, and Docker versions
for the Sysbox release you install. Install it on the Docker **provisioning
host**, not inside the Coder server container. This repository does not modify
host runtimes or restart the host daemon automatically.

```bash
docker info --format '{{json .Runtimes}}'
docker build --platform linux/amd64 -f docker/worker/Dockerfile \
  -t openflows-worker:sysbox-dev .
bash scripts/test-worker-sysbox.sh openflows-worker:sysbox-dev
```

The image must exist on the same Docker host used by Coder's Terraform
provisioner. For remote deployments, publish the image to your registry and
pull it on that host; prefer an immutable digest for production. The image
contains Ubuntu, Docker, Python, Git, curl, and basic utilities. Install any
repository-specific toolchain in a derived image or during task setup.

Set these values in the `.env` read by `scripts/prod.sh`:

```dotenv
TF_VAR_worker_runtime=sysbox
TF_VAR_worker_image=openflows-worker:sysbox-dev
```

Run `./scripts/prod.sh bootstrap`. Both variables are sent as Coder template
variables for FORGE and SENTINEL only. Their values are part of the bootstrap
cache fingerprint, so changing them triggers a push even if Terraform source
is unchanged. Create a fresh worker pair for initial validation. Existing
workspaces use their existing template version until explicitly updated;
switching the image/runtime can replace the workspace container.

There is no automatic fallback when Sysbox is missing: Docker rejects the
unknown runtime. An empty image in Sysbox mode is rejected by Terraform.
To return to standard workspaces, set `TF_VAR_worker_runtime=standard`, clear
`TF_VAR_worker_image`, and bootstrap again. Update/delete existing workspaces
deliberately: switching away from Sysbox destroys their private engine volume.

## Lifecycle and isolation

- Every workspace gets its own engine volume named with its workspace UUID.
  The daemon listens only on its local Unix socket. No host socket is mounted,
  and the outer workspace is not privileged.
- Container root is remapped by Sysbox. The image checks for remapped root
  before starting Docker; this check is not a substitute for runtime policy.
- The socket uses Docker's `root:docker` ownership and mode `0660`; `coder`
  belongs to that group. It controls only this workspace's daemon.
- The supervisor waits for `docker info` to succeed as `coder` before launching
  the Coder agent. Either process exiting stops the other, with bounded cleanup.
- The workspace and nested Docker data use separate volumes. Engine data
  persists across workspace container restarts and is removed when Terraform
  deletes the workspace. Test suites should clean up their own containers.
- `DOCKER_HOST` and `TESTCONTAINERS_HOST_OVERRIDE=localhost` are container
  environment variables, available to the agent and A2A executor. Published
  test ports belong to the workspace network namespace, not the outer host.

Sysbox shares the host kernel. This is not a VM security boundary. The current
templates still join `openflows_default` to reach Coder, Redis, and the A2A
relay, and retain the read-only tenant artifact volume. This change isolates
container engines; it does **not** establish a network egress allowlist or
prevent access to every service on that shared network. Deployments requiring
strict network separation must enforce host-side network policy before using
untrusted workers. Never expose the host Docker API over TCP to these networks.
Validate read-only artifact/dev mounts with your installed Sysbox release;
user-namespace ownership translation must not prevent worker reads.

## Validation

Local checks that do not require Sysbox:

```bash
python3 -m unittest discover -s tests/worker -p test_entrypoint.py -v
cargo test -p coder-client --lib
bash -n docker/worker/entrypoint.sh scripts/test-worker-sysbox.sh
bash scripts/test-worker-templates.sh
```

The template tests require Terraform 1.7+ and network access to download the
pinned providers. They run mocked plans for both roles in temporary directories
and never contact Coder or provision Docker resources.

The opt-in integration script above creates two temporary workers and private
volumes, checks socket permissions and distinct host/worker engine IDs, checks
that a container created in one worker is absent from the other, and runs a
Testcontainers PostgreSQL query with published-port access and cleanup in each.
It downloads test dependencies and images. Its EXIT trap deletes only those
temporary workers and volumes.

Also validate on a real Coder FORGE/SENTINEL pair:

1. Run the repository's actual container-backed test as `coder` in both roles.
2. From SENTINEL, delegate the same test command using
   `openflows-harness verify request --expect-exit 0 -- <test-command>`.
   Direct `docker`/`podman` verification requests remain denied by the existing
   policy; invoking a test suite that uses Testcontainers is supported.
3. Restart a worker and rerun the test; verify its engine data persists.
4. Delete the pair through Coder and verify its engine volumes are removed.
5. Check that host-side network policy blocks sibling and control-plane
   services except the explicitly required Coder, Redis, and A2A endpoints.

Passing the standalone script does not replace Coder/A2A or network-policy
validation, and does not by itself establish complete tenant isolation.
