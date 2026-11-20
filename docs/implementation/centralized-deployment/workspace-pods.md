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
| `init_container` `prepare-workspace` | Runs first as uid 1000 and creates `/home/coder/workspace`; see [The workspace directory](#the-workspace-directory) |
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

Start-up works without privilege: `of_prepare` probes for `sudo` once and,
finding none, only creates the paths under `~/.openflows` that the home volume
already lets the workspace user write; `of_load_runtime_credentials` reads the
`0440` Secret file directly and falls back to `sudo` only for a root-owned
Docker file; and `of_release_privileges` treats an unavailable `sudo` as
already released. Under `allowPrivilegeEscalation: false` that `sudo` could not
work anyway. `fsGroup` is not relied on for the workspace directory itself,
because some storage classes ignore it; the init container below owns that. See
[tenant isolation](tenant-isolation.md) for the policies the Manager applies
around the pod.

## The workspace directory

The workspace image sets `WORKDIR /home/coder/workspace`, and the home volume is
mounted over `/home/coder`, which hides the image's copy. Left alone, the
container runtime creates the missing directory as `root:root`, and the
repository clone then fails with `Permission denied`. A Docker volume is seeded
from the image, so only the Kubernetes backend shows this.

The `prepare-workspace` init container of every role runs as uid 1000 with a
neutral working directory and handles each of these states on a home volume that
survives stop and start:

| Directory state | Result |
| --- | --- |
| Missing | Created, owned by uid 1000 |
| Present and writable | Left alone, so a rerun does nothing |
| Present, not writable, empty | Removed and created again as uid 1000 |
| Present, not writable, holding files | The init container fails with a message and touches nothing. Keep the files, fix the ownership, then restart |

The removal needs write access on `/home/coder`, which the volume's group
ownership gives on storage classes that apply `fsGroup`. If it does not, the pod
fails at the init container instead of at the clone.
`cargo test -p coder-client --test kubernetes_template` runs the script for all
five roles against an empty, a writable and a populated directory.

## Rollout state

All five variants exist, so a template manifest built from this tree advertises
both the `docker` and `kubernetes` backends. The backend only appears once every
required role has a variant. A Kubernetes tenant without the variants fails with
the existing `release '<name>' has no 'kubernetes' variant for required role
'<role>'` error.

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

## Bringing a tenant up on a new cluster

Do these once per cluster, in order.

1. **Match the platform ports to the platform pods.**
   `OPENFLOWS_K8S_PLATFORM_EGRESS_PORTS` lists the ports tenant pods may reach in
   the platform namespace (see [tenant isolation](tenant-isolation.md)). A
   NetworkPolicy matches the port the pod listens on, not the Service port. A
   Coder that listens on 8080 needs 8080 in the list; without it the agent cannot
   download its binary from Coder and the workspace stays at `connecting`.
2. **Seed the first platform operator.** Promoting a template release needs a
   platform operator, and only an existing operator can grant the role
   ([`releases/service.rs`](../../../crates/openflows-manager/src/releases/service.rs)),
   so whoever deploys the cluster writes the first row. The id is the one
   `/api/v1/me` returns for that user.

   ```sql
   INSERT INTO platform_operators (user_id, created_by)
   SELECT id, id FROM users WHERE id = '<user id>';
   ```
3. **Promote the configured release** into the organization
   (`POST /api/v1/platform/organizations/{org}/template-release/promote-configured`).
   Promotion never upgrades existing pins, so a tenant created earlier keeps its
   templates until it is [recreated](#recreating-a-tenant). Changed templates
   also need a new `OPENFLOWS_RELEASE_NAME`, as the
   [production runbook](operator-production-runbook.md) says.
4. **Add a chat model in each tenant's Coder organization.** Coder Agents
   providers are deployment-wide, but models belong to an organization, and a new
   organization has none (see the
   [Coder models documentation](https://coder.com/docs/ai-coder/agents/models)).
   Without one, `/api/v1/runtime/chats` returns 503 with
   `CODER_CHAT_UNAVAILABLE` (`Coder could not create the worker Chat`). The
   cause, `No chat model is available in this organization`, is in the Manager
   log.
5. **Keep the Manager's secrets directory on a persistent volume** when it uses
   the filesystem secret provider (`OPENFLOWS_SECRET_PROVIDER=filesystem`).
   Per-tenant hook keys and Redis credentials live there. Losing the directory
   makes a start fail with `cannot resolve hook key`.

## Recreating a tenant

Deleting a tenant removes its Coder workspaces, and with them the home volumes
(`openflows-<role>-<workspace id>` claims are removed only when Coder deletes the
workspace). Anything not pushed to the repository, and the agents' saved state,
is lost.

> [!WARNING]
> Recreate a tenant only when it holds nothing worth keeping. Before deleting
> one that does, push or copy its work out of `/home/coder/workspace` first.

On a disposable validation tenant, delete it and create it again. On a tenant
with data, avoid recreating it, and back up the Manager's secrets directory,
because a start fails without it.

## Troubleshooting

| Symptom | Cause | What to do |
| --- | --- | --- |
| Workspace stays at `connecting`; the startup log cannot download the agent | The platform egress ports omit the port Coder listens on | Step 1 |
| Promote returns 403 | The user is not a platform operator | Step 2 |
| A new tenant still runs the old templates | Promotion does not upgrade existing pins | [Recreate the tenant](#recreating-a-tenant) |
| `/api/v1/runtime/chats` returns 503 `CODER_CHAT_UNAVAILABLE`, and the Manager log says `No chat model is available in this organization` | The tenant's Coder organization has no model | Step 4 |
| A start fails with `cannot resolve hook key` | The Manager's secrets directory was lost | [Recreate the tenant](#recreating-a-tenant); see step 5 |
| `FATAL: Manager runtime registration failed` after restarting a workspace in Coder | The bootstrap credential is single use ([`bootstrap.sh`](../../../runtime/lib/bootstrap.sh)) | Stop, then start the tenant through OpenFlows |
| The `prepare-workspace` init container fails with `is not writable and holds files` | A populated workspace directory the user cannot write | Keep the files, fix the ownership, restart |
