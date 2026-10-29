# AWS operator runbook

Deploy **staging**, validate it, then deploy **production** with separate
resources and credentials. The local Compose stack is not a production
deployment.

> **Current blocker:** This repository does not yet ship a validated isolated
> provisioner backend or an AWS deployment manifest/Helm chart. Hosted mode
> rejects the shared Docker backend. The steps below become executable only
> when an infrastructure release supplies those missing pieces. Do not use an
> arbitrary backend name to bypass the check.

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
key. Where IAM decides by tag, set
`OPENFLOWS_AWS_SECRETS_TAGS=Key=Value,Key=Value`: the Manager sends those tags in
`CreateSecret`, so its role also needs `secretsmanager:TagResource`. Seed the GitHub client secret, App private key, webhook secret, and the
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
