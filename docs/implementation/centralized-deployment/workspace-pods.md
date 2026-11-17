# Workspace pods on Kubernetes

The `kubernetes` backend runs each role workspace as a pod instead of a Docker
container, so a hosted cluster needs no Docker daemon and no privileged
container. The design and its tradeoffs are in
[portable workspace runtime](../../architecture/portable-workspace-runtime.md);
this page records what the templates actually do and what is still landing.

## What the templates create

The Kubernetes variants live beside the Docker ones in
`crates/coder-client/templates/kubernetes/<role>/` and are packaged as
`openflows-kubernetes-<role>.tar.gz`, the archive name the Manager already
expects for a Kubernetes variant (`Role::archive_name_for_backend`).

Per role, the Terraform creates:

| Object | Notes |
| --- | --- |
| `kubernetes_deployment_v1` | One replica, `count = coder_workspace.me.start_count`, so the pod follows the workspace's start and stop. `wait_for_rollout = false`: the agent reports readiness to the Manager, not to a container probe |
| `kubernetes_persistent_volume_claim_v1` | `ReadWriteOnce`, created with the workspace and **not** tied to `start_count`, so a stop does not discard the home |
| `kubernetes_service_v1` (Nexus only) | `openflows-nexus-<tenant>` on port 3000, the address the Manager passes to workers as `a2a_relay_addr` |

Startup is unchanged: the pod sources the same `runtime/lib/bootstrap.sh` from
the pinned image, checks `OF_RUNTIME_API`, and calls `of_bootstrap_controller`
(Nexus) or `of_bootstrap_worker` (the other roles). Only the infrastructure
differs, so a role contract is written once.

## The runtime bundle

The Manager writes one Secret per workspace — `openflows-runtime-<workspace id>`
in the organization's namespace — and passes its name as the
`runtime_secret_ref` parameter. The template's only knowledge of it is the name.
It is mounted read-only at `/run/openflows`, whose `runtime.json` key is the
`/run/openflows/runtime.json` path every role already reads, and the parameter
`OPENFLOWS_RUNTIME_CREDENTIAL_FILE` points at it.

`runtime_namespace` carries the namespace the Secret lives in, so a pod always
lands beside its own Secret; it falls back to the template's `namespace`
variable for a manual push. `organization` labels the resources.

## Namespace and isolation

The pod's namespace enforces the `restricted` Pod Security Standard, so the
templates satisfy it directly:

- `runAsNonRoot`, uid/gid 1000, `fsGroup` 1000;
- `allowPrivilegeEscalation: false` and all capabilities dropped;
- `seccompProfile: RuntimeDefault`;
- `automountServiceAccountToken: false`.

`fsGroup` also makes the start-up work without privilege: `of_prepare` probes
for `sudo` once and, finding none, only creates the paths the PVC already handed
over writable; `of_load_runtime_credentials` reads the `0440` Secret file
directly and falls back to `sudo` only for a root-owned Docker file; and
`of_release_privileges` treats an unavailable `sudo` as already released. Under
`allowPrivilegeEscalation: false` that `sudo` could not work anyway. See
[tenant isolation](tenant-isolation.md) for the policies the Manager applies
around the pod.

## Rollout state

All five variants exist, so a template manifest built from this tree advertises
both the `docker` and `kubernetes` backends: the backend only appears once every
required role has a variant, which is now satisfied. What is left in issue #500
is running the set on a real cluster and proving it there; a Kubernetes tenant
without the variants fails with the existing
`release '<name>' has no 'kubernetes' variant for required role '<role>'` error.

Only Nexus declares a Service, because it is the only role other workspaces
address by name. Every pod still carries the tenant and organization labels the
per-tenant NetworkPolicy selects.

## Checking a template

```bash
cd crates/coder-client/templates/kubernetes/openflows-nexus
terraform init -backend=false && terraform validate
terraform fmt -check
```

`cargo test -p coder-client --test kubernetes_template` guards the structure
(start count, digest pinning, Secret mount, Service name, labels, pod security)
without needing a cluster. `.terraform/` is excluded from the packaged archives.
