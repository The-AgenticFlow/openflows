# Manager foundations and human authorization

The manager owns the Openflows product database, separately from Coder. It adds
GitHub human sign-in, browser and CLI sessions, device approval, organization
membership, and invitations. GitHub installation connections, Coder
provisioning workers, and the complete hosted CLI remain later packages.

It also adds runtime authentication and the GitHub credential broker: a
workspace runtime principal (distinct from human sessions) that obtains
short-lived GitHub installation tokens scoped to its authorized repository and
permission profile. See [runtime credentials and the GitHub credential
broker](../../docs/architecture/runtime-credentials-broker.md).

## Validation

Migration `0012_current_organization_release.sql` adds the organization release
selection. Publication history is retained in `organization_release_bindings`;
the single current selection lives in `organization_current_releases`. Existing
bindings are backfilled by most recent creation time, with release ID breaking
ties. Successful organization readiness, release selection, and operation
completion are now committed together. Startup preserves retired releases.

Failed template imports receive at most three attempts per organization, role,
and release. Replacement versions use deterministic `-retry-1` / `-retry-2`
suffixes; successful and still-running imports are reused. Coder's top-level
job error can hide the underlying provider-download failure, so this bounded
retry policy applies to failed imports generally. Cancelled jobs are not
automatically replaced. Repeated failures still prevent organization readiness.
The test console's operation ID field and **Retry operation** button can requeue
an existing failed `org.provision` operation with `IMPORT_FAILED`, including
operations created before this recovery support. Retrying does not reset the
three-version import budget.

With `OPENFLOWS_PROVISIONER_BACKEND=docker`, Manager automatically reserves a
key identity, creates its Coder provisioner key, encrypts the credential in
`managed_provisioners`, and starts an organization-specific Docker container.
Only an `idle` or `busy` daemon authenticated with that key qualifies as ready.
Retries reuse the key and container. Interrupted key creation is recovered by
revoking only the uniquely reserved key before recreating it. Docker's
`unless-stopped` policy handles process/host restarts, and a Manager loop every
30 seconds restores missing or stopped containers for active organizations.

The Docker host must be trusted: provisioners mount its Docker socket. Each
container receives only its own credential file (0600) from
`OPENFLOWS_PROVISIONER_SECRET_DIR` (default `.local/provisioners`, directory
0700). Keep this directory durable and at the same absolute path for all
Manager replicas sharing the Docker host. For a containerized Manager, mount
the directory at the same path on host and Manager and supply Docker access.
The master key must remain stable so stored credentials can be decrypted.

`OPENFLOWS_PROVISIONER_CODER_URL` overrides the container-facing Coder URL;
otherwise loopback addresses become `host.docker.internal`. Optional settings
are `OPENFLOWS_PROVISIONER_NETWORK`, `OPENFLOWS_PROVISIONER_IMAGE` (defaults to
the pinned Coder version), `OPENFLOWS_PROVISIONER_DOCKER_SOCKET`, and
`OPENFLOWS_DOCKER_BINARY`. Docker Desktop's bundled CLI is detected on macOS.
Other backends still require an operator-managed daemon with key name
`openflows-provisioner-<coder-organization-id>`; Kubernetes launch automation
is not provided by this Docker implementation. Template publication and tenant
execution must still be validated separately from provisioner readiness.

Ordinary tests require no PostgreSQL server:

```sh
cargo test -p openflows-manager
cargo clippy -p openflows-manager --all-targets --all-features -- -D warnings
```

Run the PostgreSQL integration suite with:

```sh
bash crates/openflows-manager/scripts/run-integration-tests.sh
```

Without `OPENFLOWS_TEST_DATABASE_URL`, the runner starts the Compose
`openflows-db` service, waits at most 30 seconds for readiness, and uses the
local development credentials. Its published port binds to loopback by default;
`OPENFLOWS_PG_BIND_ADDRESS` explicitly changes that binding. If using a custom
password, supply a correctly encoded `OPENFLOWS_TEST_DATABASE_URL`.

With that variable set, the runner uses the supplied test server without
starting or stopping Compose services and never prints the URL. The role must
have CREATEDB permission. Each test creates a unique `of_test_*` database and
synchronously drops only that database on teardown, including during unwinding.
Use a dedicated test server. Stop the bundled service with
`docker compose --profile manager stop openflows-db`.

CI has a dedicated PostgreSQL 16 service/job that explicitly runs the ignored
database tests as well as the health tests.

## Transaction and lease contracts

`IdempotencyService::execute` owns a transaction and passes its connection to
the mutation callback. The callback returns a nonempty result reference. Use
that connection for all tenant, operation, audit, and outbox writes; do not use
a repository's standalone pool method from inside the callback. External side
effects must be enqueued, rather than performed before the transaction commits.
The key, mutation, and response commit together. Concurrent retries replay the
committed result; failure or cancellation rolls the entire transaction back.
Request hashing returns serialization errors to the caller. Records expire
after seven days; expired-key replacement is serialized by PostgreSQL.

`TenantsRepository::create_in_tx`, `operations::create_in_tx`, `audit::insert`,
and `outbox::insert_in_tx` accept the transaction's connection. The standalone
tenant `create` method uses the same implementation inside its own transaction.

Every operation/outbox claim has a fresh token. Owner, token, and unexpired
lease are required on acknowledgments and guarded writes. Reusing a worker
name does not revive an old guard. Each successful operation claim increments
its attempt count. These checks fence database updates; downstream side effects
still require idempotent adapters and recovery in later work packages.

Migration 0005 upgrades databases initialized with 0001–0004 without editing
their checksums. It enforces organization relationships and reserves tenant
names/repositories and workspace role/slots until both desired and observed
states are deleted. NULL workspace slots represent a singleton slot.

## Template release source

Template releases come from an operator-configured source, never
customer-supplied URLs (see
[docs/implementation/centralized-deployment/06-hosted-template-releases.md](../../../docs/implementation/centralized-deployment/06-hosted-template-releases.md)):

- **Local** (default): packages the checked-in `crates/coder-client/templates`
  directory. For development and tests; requires no network.
- **GitHub** (`OPENFLOWS_RELEASE_SOURCE=github`): resolves a pinned immutable
  GitHub release, verifies the manifest and every role archive digest, rejects
  unsafe archive contents, bounds download sizes, applies timeouts, and caches
  verified artifacts on disk.

All settings are centralized and typed in `config.rs`. The release is pinned to
an immutable tag plus the manifest digest; never a mutable branch or `latest`.

### Running the hosted source locally

Publish a release with a `manifest.json` asset plus one archive per role (see
the deployment doc for the layout), then run:

```sh
export OPENFLOWS_RELEASE_SOURCE=github
export OPENFLOWS_GITHUB_TEMPLATE_REPOSITORY=The-AgenticFlow/openflows-templates
export OPENFLOWS_GITHUB_TEMPLATE_TAG=2026.10.0
export OPENFLOWS_TEMPLATE_MANIFEST_SHA256=<64-char manifest digest>
export OPENFLOWS_RELEASE_CACHE_DIR=/tmp/openflows-release-cache
cargo run -p openflows-manager
```

### Testing the release source

```sh
cargo test -p openflows-manager provisioning::source
```

# Container image

`ghcr.io/the-agenticflow/openflows-manager:<tag>` runs `openflows-manager serve`
as a non-root user and listens on `0.0.0.0:3002` (`OPENFLOWS_MANAGER_ADDR`).
`GET /health` is the liveness and readiness probe. The other subcommands run in
the same image: `setup`, `doctor`, `repair-runtime` and `set-coder-hook-secret`.

All settings come from the environment, as listed in `.env.openflows.example`;
the image holds no configuration or secrets. It is built from
[`Dockerfile.manager`](../../Dockerfile.manager) by the `Docker Publish Manager`
workflow, which `release.yml` dispatches for each release and which can be
dispatched by hand for any ref and tag. It is `linux/amd64` only. Deploy it by
digest, not by tag.

# Deployment commands

Use `openflows-manager --config .env.openflows setup`, then `doctor` and `serve`.
From source, prefix each with `cargo run -p openflows-manager --` instead of
`openflows-manager`. See the [operator deployment guide](../../docs/implementation/centralized-deployment/operator-deployment.md)
for the shared configuration file, generated credentials, local Compose stack,
and the remaining production isolation requirement.
