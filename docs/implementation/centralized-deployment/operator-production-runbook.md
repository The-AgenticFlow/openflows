# AWS operator runbook

The hosted contract, including AWS Secrets Manager as the default and an explicit non-AWS path, is in [Portable workspace runtime](../../architecture/portable-workspace-runtime.md).

> **Production validation:** Kubernetes role templates now exist for all five agents. Before production use, complete the hosted release gates in staging, including workspace startup, tenant isolation, credential delivery, and recovery checks. This repository does not yet provide a turnkey production deployment command.

Deploy **staging**, validate it, then deploy **production** with separate
resources and credentials. The local Compose stack is not a production
deployment.

## 1. Provision each environment

For staging and production separately, provide:

| Component | Operator requirement |
| --- | --- |
| OpenFlows Manager and Coder | Public HTTPS origins and ingress |
| Data | Separate OpenFlows and Coder PostgreSQL databases; Redis |
| Runtime | Validated tenant-isolated provisioner and Nexus routing |
| Secrets | AWS Secrets Manager prefix and Manager workload IAM role per environment |
| GitHub | App plus separate OpenFlows and Coder sign-in clients |
| Releases | Pinned template artifacts and workspace images |

Route OpenFlows GitHub callbacks to `/auth/github/callback`,
`/api/v1/github/oauth/callback`, and `/api/v1/github/setup`; route the App
webhook to `/api/v1/webhooks/github`. Workspaces must reach Coder and the
Manager runtime API. The Manager must reach each tenant's Nexus hook server.

Give the Manager IAM role `secretsmanager:CreateSecret`, `GetSecretValue`,
`PutSecretValue`, `DeleteSecret`, and `DescribeSecret` on **its environment's
prefix only**. Add KMS decrypt/data-key permissions if using a customer-managed
key. Seed the GitHub client secret, App private key, webhook secret, and the
Coder hook secret under that prefix. Configure Coder with the **same** hook
secret before startup. Use a private HTTPS `OPENFLOWS_HOOK_URL` that both Coder
and Manager share; the signed hook JWT audience must match it exactly. Do not
put AWS keys or Manager credentials in workspace
environment variables.

## 2. Configure and initialize

Create an environment-specific configuration file in the deployment system.
It needs `OPENFLOWS_MODE=hosted`,
`OPENFLOWS_SECRET_PROVIDER=aws-secrets-manager`, `AWS_REGION`, a unique
`OPENFLOWS_AWS_SECRETS_PREFIX`, public OpenFlows/Coder URLs, PostgreSQL and
Redis URLs, GitHub App settings and secret references, the isolated provisioner
backend, and a pinned GitHub template release. The
[hosted release contract](06-hosted-template-releases.md) lists its required
repository, tag, and SHA-256 settings. Never put raw secrets in this file.

Workspaces receive their runtime credential bundle separately from the secret
store. On the `kubernetes` backend the Manager creates one Secret per workspace
in the organization's namespace (`OPENFLOWS_K8S_NAMESPACE_PREFIX`, default
`openflows-`, followed by the organization ID). A Secret delivery identity,
separate from the identity that sets up the namespaces, writes them: it needs
`create`, `patch` and `delete` on `secrets` in those namespaces and nothing else.
The Manager creates and walls each namespace, and in hosted mode refuses to start
unless the two identities are separate and the cluster enforces the network
rules; [tenant isolation](tenant-isolation.md) lists the permissions and
settings. Tenants of one organization share that
namespace, so tenant separation relies on those rules, on RBAC and on each
workspace mounting only the Secret it is given. The Kubernetes templates mount
the Secret's `runtime.json` key at `/run/openflows/runtime.json`. Set
`OPENFLOWS_K8S_RUNTIME_DELIVERY=1` when using these templates; the Manager
refuses to start on the `kubernetes` backend without it. On the `docker`
backend with a non-filesystem store, set an absolute
`OPENFLOWS_RUNTIME_DELIVERY_DIR` for the per-workspace bundle files; the
Manager refuses to start without it.

On the `kubernetes` backend, set `OPENFLOWS_RUNTIME_RELAY_BASE_URL` to
`http://openflows-nexus-{tenant}.{namespace}.svc.cluster.local:3001` so Coder hooks reach each
tenant's Nexus; [Hook delivery](workspace-pods.md#hook-delivery) explains the placeholders.

Hosted workspaces need no shared storage. The role templates mount the tenant
`openflows-artifacts-<tenant>` volume only when the workspace is not hosted, so
a hosted deployment creates none and requires no read-write-many volume; each
workspace installs the artifact bundle from the Manager and clones the
repository with its own brokered credential. Local installations keep the volume
for the artifact tree and the golden repository seed.

Recreating an existing hosted tenant's Nexus workspace plans a destroy of
`docker_volume.artifacts`, because the template now declares it only in local
mode. Docker refuses to remove a volume another container still mounts, so stop
that tenant's role workspaces before the apply, or it fails with
`volume is in use`. If the volume was already removed by hand, clear the plan
with `terraform state rm 'docker_volume.artifacts[0]'` in the Nexus workspace.
A local-mode tenant keeps the volume: the resource gains an index, and the
existing object is adopted as `[0]` without replacement.

For a new, empty Coder, set `OPENFLOWS_CODER_BOOTSTRAP_EMAIL`; setup creates
and retains the administrator credential and Manager key. Configure Coder's
`CODER_MAX_ADMIN_TOKEN_LIFETIME` to at least `2160h`. For an existing Coder,
omit the bootstrap email and seed a trusted administrator token via
`OPENFLOWS_CODER_BOOTSTRAP_TOKEN_REF`; operate its rotation separately.

Run these as deployment jobs with the Manager IAM role and database access:

```sh
openflows-manager --config /path/to/environment.conf setup
openflows-manager --config /path/to/environment.conf doctor
```

## 3. Deploy and verify

Deploy with the approved infrastructure release. This repository has **no
production deployment command yet**. Its Manager container must run:

```sh
openflows-manager --config /path/to/environment.conf serve
```

Use `/health` for liveness and `/ready` for readiness. In staging, test
GitHub sign-in and App installation, organization/template setup, tenant
creation and start, Coder workspace connection, runtime heartbeat, and
cross-tenant credential denial. Check backups, alerts, and a restore test.
Promote the same pinned artifacts to production only after staging passes;
repeat a short production smoke test.

## Keep it running

Back up the OpenFlows database and its Secrets Manager namespace together;
losing the master key makes encrypted records unreadable. Monitor Manager and
Coder health, failed operations, credential expiry, and secret-version growth.
Changed templates need a new `OPENFLOWS_RELEASE_NAME`. Moving a local
installation to hosted mode requires an explicit database-and-secret
migration, not an environment-variable change.

### Upgrade order

The workspace harness pins the artifact bundle to the one compiled into its own
binary, so the harness and the Manager move together. Workspaces get the harness
from the workspace image their template release pins
(`ghcr.io/the-agenticflow/openflows-workspace`, by digest), so a template release
and a Manager of the same version always agree.

Deploying one half ahead of the other makes workspace rebuilds fail with a 404
that names both digests, and they keep failing until the other half catches up.
Upgrade both in one window. `openflows-harness artifacts fetch --any-release`
installs whichever bundle the Manager serves, which is a deliberate mixed fleet
for a migration window, not something to leave in a template.

### Upgrading from 1.8.2 or earlier: the workspace image

Releases up to 1.8.2 ran hosted workspaces from `codercom/enterprise-base`, and
`OPENFLOWS_WORKSPACE_IMAGE_DIGEST` held a digest of that image. Later releases
run them from `ghcr.io/the-agenticflow/openflows-workspace@<digest>`. An old
digest does not exist in that repository: the Manager starts normally, and
then every workspace build fails to pull its image. Before you start the
upgraded Manager:

1. Set `OPENFLOWS_WORKSPACE_IMAGE_DIGEST` in `.env.prod` to the
   `openflows-workspace` digest listed under **Docker images** in the release
   notes of the version you run.
2. Run `scripts/check-workspace-image.sh`. It pulls
   `openflows-workspace@<digest>` for `linux/amd64` and checks that it is an
   OpenFlows workspace image. Start the Manager only when it prints `ok:`.

The image is a minimal runtime (Ubuntu 24.04 with `git`, `curl`, `jq`,
`python3`, `ssh`, `sudo` and the OpenFlows programs), not a development
environment like `enterprise-base`. It has no compilers, `make`, `unzip`,
`wget` or language toolchains.

Hosted workspaces also give up `sudo` at the end of startup, before they report
ready: the runtime removes the workspace user's sudo rule and checks that `sudo`
no longer works, or startup fails. Agents in a hosted workspace therefore run
without root. They cannot read the root-owned runtime secret, and they cannot
`apt-get install` tools, which they could on `enterprise-base`. Tools a hosted
repository needs for delegated verification must be installable without root
(for example into the home directory or the checkout), or come with the image.
Local workspaces keep `sudo`.
